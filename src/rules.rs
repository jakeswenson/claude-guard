//! The engine that runs a rule table against one tool call.
//!
//! The table comes from a rule file, see [`load`]; the engine holds no
//! rules of its own. Evaluation order is fixed: a command the parser
//! rejects asks the user; then rules in file order, rows in rule order,
//! first opinion wins; then, when nothing spoke, a warning about any
//! `$(...)` text the segmenter could not inspect.
//!
//! A row fires when its subject matches and its condition holds under
//! some binding set the match produced. A rule's own `:when` is checked
//! once, before its rows. An unknown condition does not fire a row in
//! this version.

use std::path::{Path, PathBuf};

use crate::cond::{self, Env, Fs, Truth};
use crate::elaborate::{Declarations, Elaborated, Inner};
use crate::input::{HookInput, Tool, string_id};
use crate::load::{self, LoadError, Loaded, Source};
use crate::output::Decision;
use crate::pattern::{self, Bindings, Pattern};
use crate::segment::{self, RedirectKind, SegmentError, Segments, Word};
use crate::syntax::{FileTool, PathArg, Row, Rule, Subject};

/// A row's decision kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
  Deny,
  Ask,
  Warn,
}

/// Everything the engine knows about one tool call. Built once in `hook`.
#[derive(Debug)]
pub struct Context {
  pub input: HookInput,
  pub seen: Seen,
  /// Each simple command of a Bash call, elaborated under the
  /// declarations in force. Empty for every other tool.
  pub elaborated: Vec<Elaborated>,
}

/// The part of the tool input the rules look at.
#[derive(Debug)]
pub enum Seen {
  Bash(Result<Segments, SegmentError>),
  File { tool: FileTool, path: PathBuf },
  Other,
}

impl Context {
  /// Segment and elaborate a Bash command, or note the file a file tool
  /// touches.
  pub fn new(
    input: HookInput,
    declarations: &Declarations,
  ) -> Context {
    let seen = match &input.tool {
      Tool::Bash { command } => Seen::Bash(segment::segment(command)),
      Tool::Write { path } => Seen::File {
        tool: FileTool::Write,
        path: path.clone(),
      },
      Tool::Edit { path } => Seen::File {
        tool: FileTool::Edit,
        path: path.clone(),
      },
      Tool::MultiEdit { path } => Seen::File {
        tool: FileTool::MultiEdit,
        path: path.clone(),
      },
      Tool::Read { path } => Seen::File {
        tool: FileTool::Read,
        path: path.clone(),
      },
      Tool::WebFetch { .. }
      | Tool::Glob { .. }
      | Tool::Grep { .. }
      | Tool::Mcp { .. }
      | Tool::Other { .. } => Seen::Other,
    };
    let elaborated = match &seen {
      Seen::Bash(Ok(segments)) => segments
        .commands
        .iter()
        .map(|command| declarations.elaborate(command))
        .collect(),
      _ => Vec::new(),
    };
    Context {
      input,
      seen,
      elaborated,
    }
  }
}

string_id! {
  /// A rule's name from the file, or `parse-error` and `uninspected` for
  /// the two answers the engine gives on its own.
  RuleName
}

string_id! {
  /// The subject that fired, as written in the file: `[git -... stash ...]`
  /// or `(write ?path)`.
  PatternText
}

/// The engine's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
  pub rule: RuleName,
  /// `None` for the engine's own answers, which have no row.
  pub pattern: Option<PatternText>,
  /// What the row's binders captured. Empty when the row has none.
  pub bindings: Bindings,
  pub decision: Decision,
}

/// A loaded rule table, ready to evaluate.
pub struct Ruleset {
  pub source: Source,
  /// Every command declaration in force. The matcher starts using them
  /// in claude-guard-1ma.3.
  pub declarations: Declarations,
  rules: Vec<Rule>,
}

impl Ruleset {
  /// The rules in force, per [`load`].
  pub fn load() -> Result<Ruleset, LoadError> {
    load::from_env().map(Ruleset::from)
  }

  /// The rules shipped with the binary, for tests. The hook goes through
  /// [`Ruleset::load`] so a user file can replace them.
  #[cfg(test)]
  pub fn builtin() -> Ruleset {
    load::load_text(Source::Builtin, load::BUILTIN)
      .map(Ruleset::from)
      .expect("the built-in rule file type-checks; a test guards this")
  }

  /// A table from text, for tests.
  #[cfg(test)]
  pub fn from_text(text: &str) -> Result<Ruleset, LoadError> {
    load::load_text(Source::Builtin, text).map(Ruleset::from)
  }

  pub fn rules(&self) -> &[Rule] {
    &self.rules
  }

  /// First opinion wins. `None` means the call proceeds untouched.
  pub fn evaluate(
    &self,
    ctx: &Context,
    fs: &dyn Fs,
  ) -> Option<Verdict> {
    if let Seen::Bash(Err(e)) = &ctx.seen {
      return Some(Verdict {
        rule: RuleName::from("parse-error"),
        pattern: None,
        bindings: Bindings::new(),
        decision: Decision::Ask {
          reason: format!("claude-guard could not parse this command: {e}"),
        },
      });
    }

    let env = Env {
      cwd: ctx.input.cwd.as_ref(),
      fs,
    };
    for rule in &self.rules {
      if let Some(when) = &rule.when
        && when.eval(&env, &Bindings::new()) != Truth::True
      {
        continue;
      }
      for row in &rule.rows {
        if let Some((what, bindings)) = find(row, ctx, &self.declarations, &env) {
          return Some(Verdict {
            rule: rule.name.clone(),
            pattern: Some(PatternText::from(row.subject.to_string())),
            decision: render(row, &what),
            bindings,
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
        bindings: Bindings::new(),
        decision: Decision::Warn {
          context: format!("claude-guard did not inspect: {list}"),
        },
      });
    }

    None
  }
}

impl From<Loaded> for Ruleset {
  fn from(loaded: Loaded) -> Ruleset {
    Ruleset {
      source: loaded.source,
      declarations: loaded.declarations,
      rules: loaded.file.rules,
    }
  }
}

/// How far into wrappers and scripts a row looks: `ssh` carrying
/// `bash -c` carrying `sudo` is three.
const INNER_DEPTH: usize = 8;

/// The text of what matched and the bindings that satisfied the row's
/// condition, or `None`.
fn find(
  row: &Row,
  ctx: &Context,
  declarations: &Declarations,
  env: &Env<'_>,
) -> Option<(String, Bindings)> {
  match (&row.subject, &ctx.seen) {
    (Subject::Command(pattern), Seen::Bash(Ok(_))) => ctx
      .elaborated
      .iter()
      .find_map(|command| find_in(row, pattern, command, declarations, env, 0)),
    (Subject::Tool(wanted), Seen::File { tool, path }) if wanted.tool == *tool => {
      let text = path.to_string_lossy().into_owned();
      let candidate = match &wanted.path {
        PathArg::Literal(literal) => {
          if pattern::normalize_path(Path::new(literal)) != pattern::normalize_path(path) {
            return None;
          }
          Bindings::new()
        }
        PathArg::Var(var) => Bindings::from([(var.clone(), text.clone())]),
      };
      let chosen = cond::choose(row.when.as_ref(), vec![candidate], env)?;
      Some((text, chosen))
    }
    _ => None,
  }
}

/// Match `pattern` against one elaborated command, then against whatever
/// it carries: an inner command as is, an inner script segmented and
/// elaborated first. The first hit wins, outermost first.
fn find_in(
  row: &Row,
  pattern: &Pattern,
  command: &Elaborated,
  declarations: &Declarations,
  env: &Env<'_>,
  depth: usize,
) -> Option<(String, Bindings)> {
  let candidates = pattern.bindings_in(&command.units(), &command.redirects);
  if let Some(chosen) = cond::choose(row.when.as_ref(), candidates, env) {
    return Some((describe(command), chosen));
  }
  if depth >= INNER_DEPTH {
    return None;
  }
  match &command.inner {
    Some(Inner::Command(inner)) => find_in(row, pattern, inner, declarations, env, depth + 1),
    Some(Inner::Script(text)) => {
      let segments = segment::segment(text).ok()?;
      segments.commands.iter().find_map(|simple| {
        let inner = declarations.elaborate(simple);
        find_in(row, pattern, &inner, declarations, env, depth + 1)
      })
    }
    None => None,
  }
}

fn render(
  row: &Row,
  what: &str,
) -> Decision {
  let reason = &row.reason;
  let instead = match &row.instead {
    Some(instead) => format!(" Instead: {instead}"),
    None => String::new(),
  };
  match row.decision {
    Kind::Deny => Decision::Deny {
      reason: format!("claude-guard denied `{what}`: {reason}{instead}"),
    },
    Kind::Ask => Decision::Ask {
      reason: format!("claude-guard asks about `{what}`: {reason}{instead}"),
    },
    Kind::Warn => Decision::Warn {
      context: format!("claude-guard noted `{what}`: {reason}{instead}"),
    },
  }
}

/// A command as one line: words as given, then redirects.
fn describe(command: &Elaborated) -> String {
  let mut parts: Vec<String> = command
    .flatten()
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
pub mod testing {
  use super::*;

  /// A filesystem that answers `ancestor-has?` from a fixed list.
  pub struct FakeFs(pub Vec<&'static str>);

  impl Fs for FakeFs {
    fn ancestor_has(
      &self,
      _cwd: &Path,
      name: &str,
    ) -> Truth {
      self.0.contains(&name).into()
    }
  }

  /// A filesystem with a `.jj` above cwd, or without one.
  pub fn repo(jj: bool) -> FakeFs {
    FakeFs(if jj { vec![".jj"] } else { vec![] })
  }
}

#[cfg(test)]
mod tests {
  use super::testing::repo;
  use super::*;
  use crate::pattern::Var;

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

  fn run(
    tool: Tool,
    in_jj_repo: bool,
  ) -> Option<Verdict> {
    let rules = Ruleset::builtin();
    rules.evaluate(
      &Context::new(input(tool), &rules.declarations),
      &repo(in_jj_repo),
    )
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
  fn a_verdict_names_the_row_that_fired_and_its_bindings() {
    let verdict = run(bash("git stash"), true).unwrap();
    assert_eq!(
      verdict.pattern,
      Some(PatternText::from("[git -... stash ...]"))
    );
    assert!(verdict.bindings.is_empty());

    let verdict = run(bash("cp -r dist /tmp/dist"), false).unwrap();
    assert_eq!(verdict.pattern, Some(PatternText::from("[cp ... ?dst]")));
    assert_eq!(
      verdict.bindings,
      Bindings::from([(Var::from("dst"), "/tmp/dist".to_string())])
    );

    let verdict = run(
      Tool::Write {
        path: "/tmp/x".into(),
      },
      false,
    )
    .unwrap();
    assert_eq!(verdict.pattern, Some(PatternText::from("(write ?path)")));
    assert_eq!(
      verdict.bindings,
      Bindings::from([(Var::from("path"), "/tmp/x".to_string())])
    );

    let verdict = run(bash("git stash &&"), false).unwrap();
    assert_eq!(verdict.pattern, None);
  }

  // --- the shipped file ---

  #[test]
  fn the_builtin_file_loads_with_its_four_rules() {
    let rules = Ruleset::builtin();
    assert_eq!(rules.source, Source::Builtin);
    let names: Vec<_> = rules.rules().iter().map(|r| r.name.to_string()).collect();
    assert_eq!(
      names,
      [
        "hard-denies",
        "ask-first",
        "git-in-jj",
        "tmp-writes",
        "tool-nudges"
      ]
    );
  }

  #[test]
  fn warn_rows_come_after_every_deny_and_ask() {
    let rows: Vec<Kind> = Ruleset::builtin()
      .rules()
      .iter()
      .flat_map(|r| r.rows.iter().map(|row| row.decision))
      .collect();
    let last_stop = rows.iter().rposition(|k| *k != Kind::Warn).unwrap_or(0);
    if let Some(first_warn) = rows.iter().position(|k| *k == Kind::Warn) {
      assert!(first_warn > last_stop, "a warn row precedes a deny row");
    }
  }

  #[test]
  fn every_guidance_ends_with_a_period() {
    for rule in Ruleset::builtin().rules() {
      for row in &rule.rows {
        assert!(row.reason.ends_with('.'), "{}: {:?}", rule.name, row.reason);
        if let Some(instead) = &row.instead {
          assert!(instead.ends_with('.'), "{}: {instead:?}", rule.name);
        }
      }
    }
  }

  // --- hard denies ---

  #[test]
  fn git_checkout_is_denied_everywhere_with_the_alternative() {
    let reason = deny_reason(run(bash("git checkout main"), false));
    assert_eq!(
      reason,
      "claude-guard denied `git checkout main`: git checkout overwrites working files and can lose \
       uncommitted work. Instead: ask the user; in a jj repo, `jj edit <rev>` or `jj new <rev>`."
    );
  }

  #[test]
  fn git_stash_is_denied_only_in_a_jj_repo() {
    assert_eq!(run(bash("git stash"), false), None);
    let reason = deny_reason(run(bash("git stash"), true));
    assert_eq!(
      reason,
      "claude-guard denied `git stash`: this repo is managed by jj, and jj has no dirty tree to \
       stash. Instead: use `jj new` to park the current change or `jj describe` to name it."
    );
  }

  #[test]
  fn destructive_git_commands_are_denied_everywhere() {
    for (command, expect) in [
      ("git reset --hard HEAD~1", "reset --hard"),
      ("git reset HEAD~1 --hard", "reset --hard"),
      ("git clean -fxd", "git clean deletes"),
      ("git clean -n", "git clean deletes"),
      ("git branch -D feature", "branch -D"),
      ("git branch feature -D", "branch -D"),
    ] {
      let reason = deny_reason(run(bash(command), false));
      assert!(reason.contains(expect), "{command}: {reason}");
      assert_eq!(
        rule_name(run(bash(command), false)),
        "hard-denies",
        "{command}"
      );
    }
    // The safe forms pass outside a jj repo.
    assert_eq!(run(bash("git reset --soft HEAD~1"), false), None);
    assert_eq!(run(bash("git reset HEAD file"), false), None);
    assert_eq!(run(bash("git branch -d feature"), false), None);
    assert_eq!(run(bash("git clean"), false), None);
  }

  // --- ask first ---

  #[test]
  fn jj_abandon_asks_and_tells_the_model_to_make_the_case() {
    let verdict = run(bash("jj abandon xyz"), true).unwrap();
    assert_eq!(verdict.rule, RuleName::from("ask-first"));
    let Decision::Ask { reason } = &verdict.decision else {
      panic!("{:?}", verdict.decision);
    };
    assert!(
      reason.starts_with("claude-guard asks about `jj abandon xyz`:"),
      "{reason}"
    );
    assert!(reason.contains("why dropping it is safe"), "{reason}");
    assert!(reason.contains("A bare request is not enough."), "{reason}");
    assert_eq!(run(bash("jj describe -m x"), true), None);
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
    let reason = deny_reason(run(bash("ls && git checkout main"), false));
    assert!(
      reason.starts_with("claude-guard denied `git checkout main`:"),
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
    // git ships with no declaration, so `-...` stops at `-C`'s value and
    // the generic row catches it. `claude-guard commands add git` fixes
    // that; see a_declared_flag_value_no_longer_hides_the_subcommand.
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
    assert_eq!(
      rule_name(run(bash("git checkout main"), true)),
      "hard-denies"
    );
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
    assert_eq!(run(bash("cp a $TMPDIR/x"), false), None);
  }

  #[test]
  fn a_relative_target_counts_when_cwd_is_under_tmp() {
    let mut input = input(bash("echo hi > out.txt"));
    input.cwd = "/tmp/work".into();
    let rules = Ruleset::builtin();
    let verdict = rules.evaluate(&Context::new(input, &rules.declarations), &repo(false));
    assert_eq!(rule_name(verdict), "tmp-writes");
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
    assert!(
      deny_reason(run(bash("awk 'NR>=128 && NR<=150' justfile"), false))
        .contains("use `bat -r 128:150 -n file`")
    );
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

  // --- rows from a file of one's own ---

  fn run_with(
    text: &str,
    tool: Tool,
  ) -> Option<Verdict> {
    let rules = Ruleset::from_text(text).unwrap_or_else(|e| panic!("{e}"));
    rules.evaluate(
      &Context::new(input(tool), &rules.declarations),
      &repo(false),
    )
  }

  // --- elaboration: declared commands and inner commands ---

  const GIT_DECLARED: &str = r#"
    (command git (option "-C" :value) (option "-P" "--no-pager")
      (subcommand clean (option "-f") (option "-x") (option "-d")))
    (rule git
      (deny [git -... push ...] :reason "push." :instead "jj git push.")
      (deny [git -... clean -f ...] :reason "clean." :instead "jj.")
      (deny [git -C ?dir ...] :when (under? ?dir "/tmp") :reason "tmp git." :instead "no.")
      (deny [git ...] :reason "generic." :instead "jj."))
  "#;

  #[test]
  fn a_declared_flag_value_no_longer_hides_the_subcommand() {
    let reason = deny_reason(run_with(GIT_DECLARED, bash("git -C . push origin main")));
    assert!(reason.ends_with("push. Instead: jj git push."), "{reason}");
    assert!(
      reason.starts_with("claude-guard denied `git -C . push origin main`:"),
      "{reason}"
    );
    // The same command with no declaration still falls to the generic row.
    let undeclared = GIT_DECLARED.replacen("(command git", "(command gut", 1);
    let reason = deny_reason(run_with(&undeclared, bash("git -C . push origin main")));
    assert!(reason.ends_with("generic. Instead: jj."), "{reason}");
  }

  #[test]
  fn a_literal_finds_a_flag_inside_a_cluster() {
    let reason = deny_reason(run_with(GIT_DECLARED, bash("git clean -fxd")));
    assert!(reason.ends_with("clean. Instead: jj."), "{reason}");
    let reason = deny_reason(run_with(GIT_DECLARED, bash("git clean -xd")));
    assert!(reason.ends_with("generic. Instead: jj."), "{reason}");
  }

  #[test]
  fn a_binder_takes_an_option_value_under_either_spelling() {
    for command in ["git -C /tmp/x log", "git -C/tmp/x log"] {
      let verdict = run_with(GIT_DECLARED, bash(command)).unwrap();
      assert_eq!(
        verdict.pattern,
        Some(PatternText::from("[git -C ?dir ...]")),
        "{command}"
      );
      assert_eq!(
        verdict.bindings,
        Bindings::from([(Var::from("dir"), "/tmp/x".to_string())])
      );
    }
  }

  #[test]
  fn a_rule_holds_through_wrappers_and_scripts() {
    for command in [
      "sudo sed -i s/a/b/ file",
      "sudo -u root env FOO=1 sed -i s/a/b/ file",
      "ssh nas 'sed -i s/a/b/ file'",
      "ssh -o ConnectTimeout=10 nas sed -i s/a/b/ file",
      "bash -c 'cd /x && sed -i s/a/b/ file'",
      "nu -c 'sed -i s/a/b/ file'",
      "timeout 5s sed -i s/a/b/ file",
      "ssh nas \"bash -c 'sudo sed -i s/a/b/ file'\"",
    ] {
      let reason = deny_reason(run(bash(command), false));
      assert!(
        reason.starts_with("claude-guard denied `sed -i s/a/b/ file`:"),
        "{command}: {reason}"
      );
    }
    // The wrapper's own options never reach the inner command.
    assert_eq!(run(bash("sudo -u sed ls"), false), None);
    assert_eq!(run(bash("bash script-that-mentions-sed.sh"), false), None);
  }

  #[test]
  fn the_outermost_match_wins() {
    let reason = deny_reason(run(bash("sudo grep foo $(cat x)"), false));
    assert!(
      reason.starts_with("claude-guard denied `grep foo $(cat x)`:"),
      "{reason}"
    );
    let reason = deny_reason(run(bash("sudo -n git stash"), true));
    assert!(
      reason.starts_with("claude-guard denied `git stash`:"),
      "{reason}"
    );
  }

  #[test]
  fn ask_and_warn_rows_render_their_own_prefix() {
    let text = "(rule r (ask [jj -... abandon ...] :reason \"look first.\" :instead \"run `jj status`.\") (warn [cargo -... clean ...] :reason \"slow.\"))";
    assert_eq!(
      run_with(text, bash("jj abandon")).unwrap().decision,
      Decision::Ask {
        reason: "claude-guard asks about `jj abandon`: look first. Instead: run `jj status`."
          .into()
      }
    );
    assert_eq!(
      run_with(text, bash("cargo clean")).unwrap().decision,
      Decision::Warn {
        context: "claude-guard noted `cargo clean`: slow.".into()
      }
    );
  }

  #[test]
  fn a_literal_tool_path_matches_by_normalized_path() {
    let text = "(rule r (deny (read \"/tmp/secret\") :reason \"no.\" :instead \"ask.\"))";
    assert!(
      run_with(
        text,
        Tool::Read {
          path: "/private/tmp/secret".into()
        }
      )
      .is_some()
    );
    assert!(
      run_with(
        text,
        Tool::Read {
          path: "/tmp/other".into()
        }
      )
      .is_none()
    );
    assert!(
      run_with(
        text,
        Tool::Write {
          path: "/tmp/secret".into()
        }
      )
      .is_none()
    );
  }

  #[test]
  fn a_row_fires_on_the_first_binding_set_that_holds() {
    let text = "(rule r (deny [cp ... ?x ...] :when (under? ?x \"/tmp\") :reason \"no.\" :instead \"ask.\"))";
    let verdict = run_with(text, bash("cp a /tmp/b /tmp/c")).unwrap();
    assert_eq!(
      verdict.bindings,
      Bindings::from([(Var::from("x"), "/tmp/b".to_string())])
    );
  }

  #[test]
  fn describe_renders_words_and_redirects() {
    let segments = segment::segment("echo \"hi there\" > /tmp/x 2>> err").unwrap();
    let elaborated = Declarations::new().elaborate(&segments.commands[0]);
    assert_eq!(describe(&elaborated), "echo hi there > /tmp/x >> err");
  }
}
