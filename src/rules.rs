//! The rule table and the engine that runs it.
//!
//! A rule is plain data: a name, a condition, a decision kind, and rows of
//! (subject, guidance). A subject is a command pattern from [`pattern`] or a
//! path prefix for the file tools. Guidance is a reason and an alternative,
//! rendered together so the model always learns what to do instead.
//!
//! Evaluation order is fixed: a command the parser rejects asks the user;
//! then rules in table order, first opinion wins; then, when nothing spoke,
//! a warning about any `$(...)` text the segmenter could not inspect.

use std::path::PathBuf;

use crate::input::{HookInput, Tool, string_id};
use crate::output::Decision;
use crate::pattern::{self, Pattern, PatternError};
use crate::repo;
use crate::segment::{self, RedirectKind, SegmentError, Segments, SimpleCommand, Word};

/// One row of the built-in table.
pub struct Rule {
  pub name: &'static str,
  pub when: When,
  pub decision: Kind,
  /// Checked in order; the first subject that matches supplies the guidance.
  pub subjects: &'static [(Subject, Guidance)],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
  Always,
  InJjRepo,
}

/// The table has only deny rows today. Ask and Warn wait for a row that
/// needs them; the engine already renders all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Kind {
  Deny,
  Ask,
  Warn,
}

pub enum Subject {
  /// A pattern in the [`pattern`] language, matched against every simple
  /// command in a Bash call.
  Command(&'static str),
  /// A path prefix, matched against the file path of Write, Edit, and
  /// MultiEdit. `/tmp` and `/private/tmp` are the same place.
  FileUnder(&'static str),
}

/// Why a call is stopped and what to do instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guidance {
  pub reason: &'static str,
  pub instead: &'static str,
}

const fn say(
  reason: &'static str,
  instead: &'static str,
) -> Guidance {
  Guidance { reason, instead }
}

const NO_TMP: Guidance = say(
  "no files under /tmp.",
  "write inside the project, or use a test or an example.",
);

const JJ_REPO: &str = "this repo is managed by jj.";

/// The built-in table, in evaluation order. Warn rules go last so a warn
/// never shadows a deny on the same command.
pub const RULES: &[Rule] = &[
  Rule {
    name: "hard-denies",
    when: When::Always,
    decision: Kind::Deny,
    subjects: &[
      (
        Subject::Command("git -... worktree ..."),
        say(
          "git worktrees are banned here.",
          "use `jj workspace add`, and ask the user before creating one.",
        ),
      ),
      (
        Subject::Command("git -... stash ..."),
        say(
          "jj has no dirty tree, so there is nothing to stash.",
          "use `jj new` to park the current change or `jj describe` to name it.",
        ),
      ),
      (
        Subject::Command("git -... checkout ..."),
        say(
          "checkout moves a git HEAD that jj does not track.",
          "use `jj edit <rev>` or `jj new <rev>`.",
        ),
      ),
      (
        Subject::Command("sed ..."),
        say(
          "sed is banned.",
          "use `sd` for replacements, `rg` for searching, or the Edit tool.",
        ),
      ),
      (
        Subject::Command("chezmoi -... apply ..."),
        say(
          "chezmoi apply changes the live dotfiles and is never run from a session.",
          "show the diff with `chezmoi diff` and let the user apply.",
        ),
      ),
      (
        Subject::Command("cat ... > *"),
        say(
          "writing files through a cat redirect is banned.",
          "use the Write tool.",
        ),
      ),
    ],
  },
  Rule {
    name: "git-in-jj",
    when: When::InJjRepo,
    decision: Kind::Deny,
    subjects: &[
      (
        Subject::Command("git -... log ..."),
        say(JJ_REPO, "use `jj log`."),
      ),
      (
        Subject::Command("git -... status ..."),
        say(JJ_REPO, "use `jj status`."),
      ),
      (
        Subject::Command("git -... diff ..."),
        say(JJ_REPO, "use `jj diff`."),
      ),
      (
        Subject::Command("git -... show ..."),
        say(JJ_REPO, "use `jj show`."),
      ),
      (
        Subject::Command("git -... blame ..."),
        say(JJ_REPO, "use `jj file annotate`."),
      ),
      (
        Subject::Command("git -... add ..."),
        say(JJ_REPO, "nothing; jj tracks new files on its own."),
      ),
      (
        Subject::Command("git -... commit ..."),
        say(JJ_REPO, "use `jj commit` or `jj describe`."),
      ),
      (
        Subject::Command("git -... push ..."),
        say(JJ_REPO, "use `jj git push`."),
      ),
      (
        Subject::Command("git -... pull ..."),
        say(JJ_REPO, "use `jj git fetch`, then `jj rebase`."),
      ),
      (
        Subject::Command("git -... fetch ..."),
        say(JJ_REPO, "use `jj git fetch`."),
      ),
      (
        Subject::Command("git -... rebase ..."),
        say(JJ_REPO, "use `jj rebase`."),
      ),
      (
        Subject::Command("git -... branch ..."),
        say(JJ_REPO, "use `jj bookmark`."),
      ),
      (
        Subject::Command("git ..."),
        say(
          JJ_REPO,
          "use the jj equivalent, or `jj git <subcommand>` for remote operations.",
        ),
      ),
    ],
  },
  Rule {
    name: "tmp-writes",
    when: When::Always,
    decision: Kind::Deny,
    subjects: &[
      (Subject::Command("... > /tmp/**"), NO_TMP),
      (Subject::Command("tee ... /tmp/**"), NO_TMP),
      (
        Subject::Command("mktemp ..."),
        say(
          "mktemp creates files under /tmp.",
          "write inside the project, or use a test or an example.",
        ),
      ),
      (Subject::Command("cp ... /tmp/**"), NO_TMP),
      (Subject::Command("mv ... /tmp/**"), NO_TMP),
      (Subject::FileUnder("/tmp"), NO_TMP),
    ],
  },
  Rule {
    name: "tool-nudges",
    when: When::Always,
    decision: Kind::Deny,
    subjects: &[
      (
        Subject::Command("grep ..."),
        say("grep is not the search tool here.", "use `rg`."),
      ),
      (
        Subject::Command("find ..."),
        say("find is not the file finder here.", "use `fd`."),
      ),
    ],
  },
];

/// Everything the engine knows about one tool call. Built once in `hook`.
#[derive(Debug)]
pub struct Context {
  pub input: HookInput,
  pub seen: Seen,
  pub in_jj_repo: bool,
}

/// The part of the tool input the rules look at.
#[derive(Debug)]
pub enum Seen {
  Bash(Result<Segments, SegmentError>),
  File(PathBuf),
  Other,
}

impl Context {
  /// Segment the command and look for a jj repo around `cwd`.
  pub fn new(input: HookInput) -> Context {
    let in_jj_repo = repo::in_jj_repo(input.cwd.as_ref());
    Context::with_repo(input, in_jj_repo)
  }

  /// Like [`Context::new`] with the repo answer supplied, for tests.
  pub fn with_repo(
    input: HookInput,
    in_jj_repo: bool,
  ) -> Context {
    let seen = match &input.tool {
      Tool::Bash { command } => Seen::Bash(segment::segment(command)),
      Tool::Write { path } | Tool::Edit { path } | Tool::MultiEdit { path } => {
        Seen::File(path.clone())
      }
      Tool::Read { .. }
      | Tool::WebFetch { .. }
      | Tool::Glob { .. }
      | Tool::Grep { .. }
      | Tool::Mcp { .. }
      | Tool::Other { .. } => Seen::Other,
    };
    Context {
      input,
      seen,
      in_jj_repo,
    }
  }
}

string_id! {
  /// A rule's name from the table, or `parse-error` and `uninspected` for
  /// the two answers the engine gives on its own.
  RuleName
}

string_id! {
  /// The text of the subject that fired: a pattern, or a path prefix.
  PatternText
}

/// The engine's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
  pub rule: RuleName,
  /// `None` for the engine's own answers, which have no table row.
  pub pattern: Option<PatternText>,
  pub decision: Decision,
}

/// The table with every pattern parsed.
pub struct Ruleset {
  rules: Vec<Compiled>,
}

struct Compiled {
  name: &'static str,
  when: When,
  decision: Kind,
  subjects: Vec<(Matcher, Guidance)>,
}

enum Matcher {
  Command(Pattern),
  FileUnder(PathBuf),
}

impl Ruleset {
  /// Compile [`RULES`]. Fails on the first pattern that does not parse.
  pub fn builtin() -> Result<Ruleset, PatternError> {
    Ruleset::compile(RULES)
  }

  pub fn compile(rules: &[Rule]) -> Result<Ruleset, PatternError> {
    let rules = rules
      .iter()
      .map(|rule| {
        let subjects = rule
          .subjects
          .iter()
          .map(|(subject, guidance)| {
            let matcher = match subject {
              Subject::Command(source) => Matcher::Command(Pattern::parse(source)?),
              Subject::FileUnder(prefix) => Matcher::FileUnder(PathBuf::from(prefix)),
            };
            Ok((matcher, *guidance))
          })
          .collect::<Result<_, _>>()?;
        Ok(Compiled {
          name: rule.name,
          when: rule.when,
          decision: rule.decision,
          subjects,
        })
      })
      .collect::<Result<_, _>>()?;
    Ok(Ruleset { rules })
  }

  /// First opinion wins. `None` means the call proceeds untouched.
  pub fn evaluate(
    &self,
    ctx: &Context,
  ) -> Option<Verdict> {
    if let Seen::Bash(Err(e)) = &ctx.seen {
      return Some(Verdict {
        rule: RuleName::from("parse-error"),
        pattern: None,
        decision: Decision::Ask {
          reason: format!("claude-guard could not parse this command: {e}"),
        },
      });
    }

    for rule in &self.rules {
      if rule.when == When::InJjRepo && !ctx.in_jj_repo {
        continue;
      }
      for (matcher, guidance) in &rule.subjects {
        if let Some(what) = matcher.find(&ctx.seen) {
          return Some(Verdict {
            rule: RuleName::from(rule.name),
            pattern: Some(matcher.text()),
            decision: render(rule.decision, &what, guidance),
          });
        }
      }
    }

    if let Seen::Bash(Ok(segments)) = &ctx.seen
      && !segments.uninspected.is_empty()
    {
      let list = segments
        .uninspected
        .iter()
        .map(|text| format!("$({text})"))
        .collect::<Vec<_>>()
        .join(", ");
      return Some(Verdict {
        rule: RuleName::from("uninspected"),
        pattern: None,
        decision: Decision::Warn {
          context: format!("claude-guard did not inspect: {list}"),
        },
      });
    }

    None
  }
}

impl Matcher {
  /// The subject as written in the table, for the log.
  fn text(&self) -> PatternText {
    match self {
      Matcher::Command(pattern) => PatternText::from(pattern.to_string()),
      Matcher::FileUnder(prefix) => PatternText::from(format!("{}/**", prefix.display())),
    }
  }

  /// The text of what matched, for the reason line.
  fn find(
    &self,
    seen: &Seen,
  ) -> Option<String> {
    match (self, seen) {
      (Matcher::Command(pattern), Seen::Bash(Ok(segments))) => segments
        .commands
        .iter()
        .find(|command| pattern.matches(command))
        .map(describe),
      (Matcher::FileUnder(prefix), Seen::File(path)) => pattern::normalize_path(path)
        .starts_with(prefix)
        .then(|| path.display().to_string()),
      _ => None,
    }
  }
}

fn render(
  kind: Kind,
  what: &str,
  guidance: &Guidance,
) -> Decision {
  let Guidance { reason, instead } = guidance;
  match kind {
    Kind::Deny => Decision::Deny {
      reason: format!("claude-guard denied `{what}`: {reason} Instead: {instead}"),
    },
    Kind::Ask => Decision::Ask {
      reason: format!("claude-guard asks about `{what}`: {reason} Instead: {instead}"),
    },
    Kind::Warn => Decision::Warn {
      context: format!("claude-guard noted `{what}`: {reason} Instead: {instead}"),
    },
  }
}

/// A simple command as one line: words, then redirects.
fn describe(command: &SimpleCommand) -> String {
  let mut parts: Vec<String> = command
    .words
    .iter()
    .map(|word| match word {
      Word::Literal(text) | Word::Dynamic(text) => text.clone(),
    })
    .collect();
  for redirect in &command.redirects {
    let op = match redirect.kind {
      RedirectKind::Write => ">",
      RedirectKind::Append => ">>",
      RedirectKind::Read => "<",
    };
    let target = match &redirect.target {
      Word::Literal(text) | Word::Dynamic(text) => text,
    };
    parts.push(format!("{op} {target}"));
  }
  parts.join(" ")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn input(tool: Tool) -> HookInput {
    HookInput {
      session_id: "s".into(),
      cwd: "/nowhere".into(),
      tool_use_id: "t".into(),
      agent_id: None,
      tool,
    }
  }

  fn bash(command: &str) -> Tool {
    Tool::Bash {
      command: command.into(),
    }
  }

  fn ruleset() -> Ruleset {
    Ruleset::builtin().expect("built-in rules compile")
  }

  fn run(
    tool: Tool,
    in_jj_repo: bool,
  ) -> Option<Verdict> {
    ruleset().evaluate(&Context::with_repo(input(tool), in_jj_repo))
  }

  fn deny_reason(verdict: Option<Verdict>) -> String {
    match verdict {
      Some(Verdict {
        decision: Decision::Deny { reason },
        ..
      }) => reason,
      other => panic!("expected a deny, got {other:?}"),
    }
  }

  fn rule_name(verdict: Option<Verdict>) -> String {
    verdict.expect("a verdict").rule.to_string()
  }

  #[test]
  fn a_verdict_names_the_row_that_fired() {
    let verdict = run(bash("git stash"), false).unwrap();
    assert_eq!(
      verdict.pattern,
      Some(PatternText::from("git -... stash ..."))
    );

    let verdict = run(
      Tool::Write {
        path: "/tmp/x".into(),
      },
      false,
    )
    .unwrap();
    assert_eq!(verdict.pattern, Some(PatternText::from("/tmp/**")));

    let verdict = run(bash("git stash &&"), false).unwrap();
    assert_eq!(verdict.pattern, None);
  }

  // --- the table itself ---

  #[test]
  fn the_builtin_table_compiles() {
    let rules = ruleset();
    assert_eq!(rules.rules.len(), RULES.len());
  }

  #[test]
  fn warn_rules_come_after_every_deny_and_ask() {
    let last_stop = RULES
      .iter()
      .rposition(|r| r.decision != Kind::Warn)
      .unwrap_or(0);
    let first_warn = RULES.iter().position(|r| r.decision == Kind::Warn);
    if let Some(first_warn) = first_warn {
      assert!(first_warn > last_stop, "a warn rule precedes a deny rule");
    }
  }

  #[test]
  fn every_guidance_ends_with_a_period() {
    for rule in RULES {
      for (_, guidance) in rule.subjects {
        assert!(
          guidance.reason.ends_with('.'),
          "{}: {:?}",
          rule.name,
          guidance.reason
        );
        assert!(
          guidance.instead.ends_with('.'),
          "{}: {:?}",
          rule.name,
          guidance.instead
        );
      }
    }
  }

  // --- hard denies ---

  #[test]
  fn git_stash_is_denied_everywhere_with_the_alternative() {
    let reason = deny_reason(run(bash("git stash"), false));
    assert_eq!(
      reason,
      "claude-guard denied `git stash`: jj has no dirty tree, so there is nothing to stash. \
       Instead: use `jj new` to park the current change or `jj describe` to name it."
    );
  }

  #[test]
  fn hard_denies_take_leading_flags_and_trailing_arguments() {
    assert_eq!(
      rule_name(run(bash("git --no-pager worktree add ../x"), false)),
      "hard-denies"
    );
    assert_eq!(
      rule_name(run(bash("git checkout -b feature"), false)),
      "hard-denies"
    );
    assert_eq!(
      rule_name(run(bash("sed -i '' 's/a/b/' file"), false)),
      "hard-denies"
    );
    assert_eq!(
      rule_name(run(bash("chezmoi -v apply"), false)),
      "hard-denies"
    );
  }

  #[test]
  fn a_denied_command_is_found_anywhere_in_a_pipeline_or_list() {
    let reason = deny_reason(run(bash("ls && git stash"), false));
    assert!(
      reason.starts_with("claude-guard denied `git stash`:"),
      "{reason}"
    );
    assert_eq!(
      rule_name(run(bash("cat x | sed s/a/b/ | head"), false)),
      "hard-denies"
    );
  }

  #[test]
  fn cat_is_denied_only_when_it_writes() {
    assert_eq!(
      rule_name(run(bash("cat > file <<EOF\nhi\nEOF"), false)),
      "hard-denies"
    );
    assert_eq!(rule_name(run(bash("cat a >> b"), false)), "hard-denies");
    assert_eq!(run(bash("cat a b"), false), None);
    assert_eq!(run(bash("cat a | head"), false), None);
  }

  // --- git in a jj repo ---

  #[test]
  fn git_in_a_jj_repo_names_the_jj_equivalent() {
    let reason = deny_reason(run(bash("git log --oneline"), true));
    assert_eq!(
      reason,
      "claude-guard denied `git log --oneline`: this repo is managed by jj. Instead: use `jj log`."
    );
    assert!(
      deny_reason(run(bash("git --no-verify push origin main"), true))
        .ends_with("use `jj git push`.")
    );
  }

  #[test]
  fn unknown_git_subcommands_get_the_generic_guidance() {
    let generic = "or `jj git <subcommand>` for remote operations.";
    let reason = deny_reason(run(bash("git reflog"), true));
    assert!(reason.ends_with(generic), "{reason}");
    // A flag with a value hides the subcommand from `-...`, by design.
    let reason = deny_reason(run(bash("git -C . push origin main"), true));
    assert!(reason.ends_with(generic), "{reason}");
    let reason = deny_reason(run(bash("git $cmd"), true));
    assert!(
      reason.starts_with("claude-guard denied `git $cmd`:"),
      "{reason}"
    );
  }

  #[test]
  fn git_outside_a_jj_repo_passes() {
    assert_eq!(run(bash("git log"), false), None);
    assert_eq!(run(bash("git push"), false), None);
  }

  #[test]
  fn jj_git_needs_no_exemption() {
    assert_eq!(run(bash("jj git push"), true), None);
    assert_eq!(run(bash("jj git fetch --all-remotes"), true), None);
  }

  #[test]
  fn hard_denies_win_over_the_jj_rule() {
    assert_eq!(rule_name(run(bash("git stash"), true)), "hard-denies");
  }

  // --- /tmp ---

  #[test]
  fn tmp_writes_are_denied_in_every_spelling() {
    for command in [
      "echo hi > /tmp/x",
      "cargo test 2> /private/tmp/err",
      "make >> /tmp/log",
      "( a; b ) > /tmp/out",
      "cargo build | tee /tmp/build.log",
      "mktemp -d",
      "cp -r dist /tmp/dist",
      "mv a /tmp/",
    ] {
      assert_eq!(
        rule_name(run(bash(command), false)),
        "tmp-writes",
        "{command}"
      );
    }
  }

  #[test]
  fn tmp_reads_and_other_paths_pass() {
    assert_eq!(run(bash("cat < /tmp/x"), false), None);
    assert_eq!(run(bash("cp a /var/tmp/b"), false), None);
    assert_eq!(run(bash("echo hi > out.txt"), false), None);
  }

  #[test]
  fn file_tools_under_tmp_are_denied() {
    for tool in [
      Tool::Write {
        path: "/tmp/notes.md".into(),
      },
      Tool::Edit {
        path: "/private/tmp/x/y.rs".into(),
      },
      Tool::MultiEdit {
        path: "/tmp/z".into(),
      },
    ] {
      let reason = deny_reason(run(tool, false));
      assert!(reason.starts_with("claude-guard denied `/"), "{reason}");
      assert!(reason.contains("no files under /tmp."), "{reason}");
    }
    assert_eq!(
      run(
        Tool::Write {
          path: "/Users/x/proj/src/main.rs".into()
        },
        false
      ),
      None
    );
  }

  // --- nudges ---

  #[test]
  fn grep_and_find_are_denied_with_replacements() {
    assert!(deny_reason(run(bash("grep -r foo src"), false)).ends_with("Instead: use `rg`."));
    assert!(deny_reason(run(bash("find . -name '*.rs'"), false)).ends_with("Instead: use `fd`."));
    assert_eq!(
      rule_name(run(bash("cargo test 2>&1 | grep FAIL"), false)),
      "tool-nudges"
    );
  }

  // --- engine behavior ---

  #[test]
  fn an_unparseable_command_asks_the_user() {
    let verdict = run(bash("git stash &&"), false).unwrap();
    assert_eq!(verdict.rule, RuleName::from("parse-error"));
    assert_eq!(
      verdict.decision,
      Decision::Ask {
        reason: "claude-guard could not parse this command: syntax error at end of input".into()
      }
    );
  }

  #[test]
  fn uninspected_substitutions_warn_when_nothing_else_speaks() {
    let verdict = run(bash("echo $(git stash) `date`"), false).unwrap();
    assert_eq!(verdict.rule, RuleName::from("uninspected"));
    assert_eq!(
      verdict.decision,
      Decision::Warn {
        context: "claude-guard did not inspect: $(git stash), $(date)".into()
      }
    );
  }

  #[test]
  fn a_deny_wins_over_the_uninspected_warning() {
    assert_eq!(
      rule_name(run(bash("grep foo $(fd -e rs)"), false)),
      "tool-nudges"
    );
  }

  #[test]
  fn earlier_rules_win_over_later_ones() {
    assert_eq!(
      rule_name(run(bash("grep foo > /tmp/x"), false)),
      "tmp-writes"
    );
  }

  #[test]
  fn tools_without_rules_pass() {
    assert_eq!(
      run(
        Tool::Read {
          path: "/tmp/x".into()
        },
        true
      ),
      None
    );
    assert_eq!(
      run(
        Tool::Other {
          name: "Agent".into(),
          input: serde_json::json!({"prompt": "git stash"}),
        },
        true
      ),
      None
    );
    assert_eq!(run(bash("cargo nextest run"), true), None);
  }

  #[test]
  fn describe_renders_words_and_redirects() {
    let segments = segment::segment("echo \"hi there\" > /tmp/x 2>> err").unwrap();
    assert_eq!(
      describe(&segments.commands[0]),
      "echo hi there > /tmp/x >> err"
    );
  }
}
