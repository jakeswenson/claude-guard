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
//! once, before its rows: false skips the rule, unknown lets its rows be
//! tried. A matched deny or ask row whose condition, or whose rule's
//! `:when`, is unknown asks, with the reason in parentheses as evidence
//! (D14, ADR 0001). A warn row in that position skips.

use std::path::{Path, PathBuf};

use crate::cond::{self, Choice};
use crate::elaborate::{Declarations, Elaborated, Inner};
use crate::facts::{Asked, Call, Facts, Truth};
use crate::input::{HookInput, Tool, string_id};
use crate::load::{self, LoadError, Loaded, Source};
use crate::log;
use crate::output::Decision;
use crate::pattern::{self, Bindings, Pattern};
use crate::segment::{self, RedirectKind, SegmentError, Segments, Word};
use crate::syntax::{FactDecl, FileTool, PathArg, Row, Rule, Subject};

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
  /// Why the row's condition, or its rule's `:when`, could not be
  /// settled, when the decision is an ask for that reason (D14).
  pub unknown: Option<String>,
  /// What the facts said when the condition held: the rule's `:when`
  /// reasons, then the row's, joined (D10). Rendered in parentheses
  /// after the row's text.
  pub evidence: Option<String>,
}

/// A loaded rule table, ready to evaluate.
pub struct Ruleset {
  pub source: Source,
  /// Every command declaration in force.
  pub declarations: Declarations,
  /// Every fact the rules may name. The built-ins today; the file's own
  /// declarations join them when extern facts land.
  pub facts: Facts,
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

  /// A table from text whose conditions may name the given facts, which
  /// the table then evaluates with. For tests.
  #[cfg(test)]
  pub fn from_text_with(
    text: &str,
    facts: Facts,
  ) -> Result<Ruleset, LoadError> {
    let loaded = load::load_text_with(Source::Builtin, text, &facts)?;
    Ok(Ruleset::assemble(
      loaded.source,
      loaded.declarations,
      registry(facts, &loaded.facts),
      loaded.file.rules,
    ))
  }

  pub fn rules(&self) -> &[Rule] {
    &self.rules
  }

  /// A table from its parts, for the spec runner.
  pub fn assemble(
    source: Source,
    declarations: Declarations,
    facts: Facts,
    rules: Vec<Rule>,
  ) -> Ruleset {
    Ruleset {
      source,
      declarations,
      facts,
      rules,
    }
  }

  /// The facts the last [`Ruleset::evaluate`] asked, in order, with
  /// their answers and timings, for the log record. Clears the list.
  pub fn facts_asked(&self) -> Vec<Asked> {
    self.facts.take_asked()
  }

  /// What a fact sees for this call: the cwd, the session, the tool, and
  /// the term, which is the log's subject as JSON. The hook and
  /// `claude-guard extern` build it the same way.
  pub fn call_for(ctx: &Context) -> Call<'_> {
    Call {
      cwd: ctx.input.cwd.as_ref(),
      session_id: ctx.input.session_id.clone(),
      tool: ctx.input.tool.name(),
      term: serde_json::to_value(log::Subject::of(ctx)).unwrap_or(serde_json::Value::Null),
    }
  }

  /// First opinion wins. `None` means the call proceeds untouched.
  pub fn evaluate(
    &self,
    ctx: &Context,
  ) -> Option<Verdict> {
    // What this evaluation asks starts from nothing.
    let _ = self.facts.take_asked();
    if let Seen::Bash(Err(e)) = &ctx.seen {
      return Some(Verdict {
        rule: RuleName::from("parse-error"),
        pattern: None,
        bindings: Bindings::new(),
        decision: Decision::Ask {
          reason: format!("claude-guard could not parse this command: {e}"),
        },
        unknown: None,
        evidence: None,
      });
    }

    let call = Ruleset::call_for(ctx);
    for rule in &self.rules {
      // The rule's `:when`: an unknown to carry to a matching row, or
      // the reasons it held with.
      let (rule_unknown, rule_evidence) = match &rule.when {
        None => (None, None),
        Some(when) => {
          let answer = when.eval(&self.facts, &call, &Bindings::new());
          match answer.truth {
            Truth::True => (None, answer.reason),
            Truth::False => continue,
            Truth::Unknown => (
              Some(answer.reason.unwrap_or_else(|| "unknown".into())),
              None,
            ),
          }
        }
      };
      for row in &rule.rows {
        let Some(found) = find(row, ctx, &self.declarations, &self.facts, &call) else {
          continue;
        };
        // The rule's `:when` ran first, so its unknown is the evidence.
        let unknown = rule_unknown.clone().or(found.unknown);
        if unknown.is_some() && row.decision == Kind::Warn {
          continue;
        }
        let evidence = join_reasons([rule_evidence.clone(), found.evidence]);
        return Some(Verdict {
          rule: rule.name.clone(),
          pattern: Some(PatternText::from(row.subject.to_string())),
          decision: render(row, &found.what, unknown.as_deref(), evidence.as_deref()),
          bindings: found.bindings,
          unknown,
          evidence,
        });
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
        unknown: None,
        evidence: None,
      });
    }

    None
  }
}

/// The reasons present, joined with `; `; `None` when there are none.
fn join_reasons(reasons: impl IntoIterator<Item = Option<String>>) -> Option<String> {
  let present: Vec<String> = reasons.into_iter().flatten().collect();
  if present.is_empty() {
    None
  } else {
    Some(present.join("; "))
  }
}

impl From<Loaded> for Ruleset {
  fn from(loaded: Loaded) -> Ruleset {
    Ruleset {
      source: loaded.source,
      declarations: loaded.declarations,
      facts: registry(Facts::builtin(), &loaded.facts),
      rules: loaded.file.rules,
    }
  }
}

/// `base` plus every declared fact, later declarations winning by name.
fn registry(
  mut base: Facts,
  declared: &[FactDecl],
) -> Facts {
  for decl in declared {
    base.declare(decl.name.clone(), decl.fact());
  }
  base
}

/// How far into wrappers and scripts a row looks: `ssh` carrying
/// `bash -c` carrying `sudo` is three.
const INNER_DEPTH: usize = 8;

/// A row whose subject matched: the text of what matched, the bindings
/// the condition was judged under, and either the reason the condition
/// came back unknown or the reasons it held with.
struct Found {
  what: String,
  bindings: Bindings,
  unknown: Option<String>,
  evidence: Option<String>,
}

impl Found {
  fn from_choice(
    what: impl FnOnce() -> String,
    choice: Choice,
  ) -> Option<Found> {
    match choice {
      Choice::Holds { bindings, evidence } => Some(Found {
        what: what(),
        bindings,
        unknown: None,
        evidence,
      }),
      Choice::Unknown { bindings, reason } => Some(Found {
        what: what(),
        bindings,
        unknown: Some(reason),
        evidence: None,
      }),
      Choice::NoMatch => None,
    }
  }
}

/// The first place the row's subject matches, outermost first, whether
/// its condition held or was unknown there.
fn find(
  row: &Row,
  ctx: &Context,
  declarations: &Declarations,
  facts: &Facts,
  call: &Call<'_>,
) -> Option<Found> {
  match (&row.subject, &ctx.seen) {
    (Subject::Command(pattern), Seen::Bash(Ok(_))) => ctx
      .elaborated
      .iter()
      .find_map(|command| find_in(row, pattern, command, declarations, facts, call, 0)),
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
      let choice = cond::choose(row.when.as_ref(), vec![candidate], facts, call);
      Found::from_choice(|| text, choice)
    }
    _ => None,
  }
}

/// Match `pattern` against one elaborated command, then against whatever
/// it carries: an inner command as is, an inner script segmented and
/// elaborated first. The first hit wins, outermost first; a hit is a
/// match whose condition held or was unknown.
fn find_in(
  row: &Row,
  pattern: &Pattern,
  command: &Elaborated,
  declarations: &Declarations,
  facts: &Facts,
  call: &Call<'_>,
  depth: usize,
) -> Option<Found> {
  let candidates = pattern.bindings_in(&command.units(), &command.redirects);
  let choice = cond::choose(row.when.as_ref(), candidates, facts, call);
  if let Some(found) = Found::from_choice(|| describe(command), choice) {
    return Some(found);
  }
  if depth >= INNER_DEPTH {
    return None;
  }
  match &command.inner {
    Some(Inner::Command(inner)) => {
      find_in(row, pattern, inner, declarations, facts, call, depth + 1)
    }
    Some(Inner::Script(text)) => {
      let segments = segment::segment(text).ok()?;
      segments.commands.iter().find_map(|simple| {
        let inner = declarations.elaborate(simple);
        find_in(row, pattern, &inner, declarations, facts, call, depth + 1)
      })
    }
    None => None,
  }
}

/// The row's decision as text, with the facts' evidence, when there is
/// any, last in parentheses. With `unknown`, a deny or ask row renders
/// as an ask and the evidence is the unknown's reason; the caller never
/// passes `unknown` for a warn row.
fn render(
  row: &Row,
  what: &str,
  unknown: Option<&str>,
  evidence: Option<&str>,
) -> Decision {
  let reason = &row.reason;
  let instead = match &row.instead {
    Some(instead) => format!(" Instead: {instead}"),
    None => String::new(),
  };
  let tail = match unknown.or(evidence) {
    Some(text) => format!(" ({text})"),
    None => String::new(),
  };
  if unknown.is_some() {
    return Decision::Ask {
      reason: format!("claude-guard asks about `{what}`: {reason}{instead}{tail}"),
    };
  }
  match row.decision {
    Kind::Deny => Decision::Deny {
      reason: format!("claude-guard denied `{what}`: {reason}{instead}{tail}"),
    },
    Kind::Ask => Decision::Ask {
      reason: format!("claude-guard asks about `{what}`: {reason}{instead}{tail}"),
    },
    Kind::Warn => Decision::Warn {
      context: format!("claude-guard noted `{what}`: {reason}{instead}{tail}"),
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
  use crate::facts::Ancestors;

  /// `rules` with `ancestor-has?` answering from a list: a `.jj` above
  /// cwd, or nothing at all. The disk is never consulted.
  pub fn in_repo(
    mut rules: Ruleset,
    jj: bool,
  ) -> Ruleset {
    rules.facts.declare(
      "ancestor-has?",
      Ancestors {
        present: if jj { vec![".jj".into()] } else { vec![] },
        unknown: false,
      },
    );
    rules
  }

  /// The shipped rules, in or out of a jj repo.
  pub fn builtin_in_repo(jj: bool) -> Ruleset {
    in_repo(Ruleset::builtin(), jj)
  }
}

#[cfg(test)]
mod tests {
  use super::testing::{builtin_in_repo, in_repo};
  use super::*;
  use crate::facts::{Ancestors, Answer, Stub};
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
    let rules = builtin_in_repo(in_jj_repo);
    rules.evaluate(&Context::new(input(tool), &rules.declarations))
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
    let rules = builtin_in_repo(false);
    let verdict = rules.evaluate(&Context::new(input, &rules.declarations));
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
    let rules = in_repo(
      Ruleset::from_text(text).unwrap_or_else(|e| panic!("{e}")),
      false,
    );
    rules.evaluate(&Context::new(input(tool), &rules.declarations))
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

  // --- unknown asks (D14, ADR 0001) ---

  fn run_with_facts(
    text: &str,
    tool: Tool,
    stubs: &[(&str, Answer)],
  ) -> Option<Verdict> {
    let mut facts = Facts::builtin();
    facts.declare(
      "ancestor-has?",
      Ancestors {
        present: vec![],
        unknown: false,
      },
    );
    for (name, answer) in stubs {
      facts.declare(*name, Stub(answer.clone()));
    }
    let rules = Ruleset::from_text_with(text, facts).unwrap_or_else(|e| panic!("{e}"));
    rules.evaluate(&Context::new(input(tool), &rules.declarations))
  }

  #[test]
  fn a_matched_row_with_an_unknown_condition_asks_with_the_evidence() {
    let stubs = [("in-jj-repo?", Answer::unknown("timed out after 1s"))];
    let text = "(rule r (deny [git -... stash ...] :when (in-jj-repo?) :reason \"no stash.\" :instead \"jj new.\"))";
    let verdict = run_with_facts(text, bash("git stash"), &stubs).unwrap();
    assert_eq!(
      verdict.decision,
      Decision::Ask {
        reason: "claude-guard asks about `git stash`: no stash. Instead: jj new. \
                 (in-jj-repo? is unknown: timed out after 1s)"
          .into()
      }
    );
    assert_eq!(
      verdict.unknown.as_deref(),
      Some("in-jj-repo? is unknown: timed out after 1s")
    );
    assert_eq!(verdict.rule, RuleName::from("r"));
    assert_eq!(
      verdict.pattern,
      Some(PatternText::from("[git -... stash ...]"))
    );
    // A pattern that did not match is not an ask.
    assert_eq!(run_with_facts(text, bash("git log"), &stubs), None);
    // A warn row skips, and a later row still decides.
    let warn = "(rule r (warn [git -... stash ...] :when (in-jj-repo?) :reason \"hm.\"))";
    assert_eq!(run_with_facts(warn, bash("git stash"), &stubs), None);
    let then_deny = "(rule r (warn [git ...] :when (in-jj-repo?) :reason \"hm.\") (deny [git ...] :reason \"no.\" :instead \"i.\"))";
    let verdict = run_with_facts(then_deny, bash("git stash"), &stubs).unwrap();
    assert!(matches!(verdict.decision, Decision::Deny { .. }));
    assert_eq!(verdict.unknown, None);
  }

  #[test]
  fn an_unknown_rule_when_asks_on_a_matching_row() {
    let stubs = [("in-jj-repo?", Answer::unknown("timed out"))];
    let text = "(rule r :when (in-jj-repo?) (deny [git ...] :reason \"jj.\" :instead \"use jj.\") (warn [cargo ...] :reason \"slow.\"))";
    let verdict = run_with_facts(text, bash("git log"), &stubs).unwrap();
    assert_eq!(
      verdict.decision,
      Decision::Ask {
        reason: "claude-guard asks about `git log`: jj. Instead: use jj. (in-jj-repo? is unknown: timed out)".into()
      }
    );
    assert_eq!(run_with_facts(text, bash("cargo build"), &stubs), None);
    assert_eq!(run_with_facts(text, bash("ls"), &stubs), None);
    // The rule's `:when` ran first, so its reason is the evidence even
    // when the row's condition is unknown too.
    let both = [
      ("a?", Answer::unknown("a out")),
      ("b?", Answer::unknown("b out")),
    ];
    let text = "(rule r :when (a?) (deny [x] :when (b?) :reason \"r.\" :instead \"i.\"))";
    assert_eq!(
      run_with_facts(text, bash("x"), &both)
        .unwrap()
        .unknown
        .as_deref(),
      Some("a? is unknown: a out")
    );
    // A false rule `:when` still skips the rule whole.
    let off = [("a?", Answer::fails()), ("b?", Answer::unknown("b out"))];
    assert_eq!(run_with_facts(text, bash("x"), &off), None);
  }

  #[test]
  fn a_condition_that_held_puts_the_facts_reasons_in_parentheses() {
    let stubs = [
      ("managed?", Answer::holds().with_reason("jj root is /x")),
      ("quiet?", Answer::holds()),
      ("tidy?", Answer::holds().with_reason("no dirty files")),
    ];
    let text = "(rule r :when (managed?) (deny [git -... stash ...] :when (and (quiet?) (tidy?)) :reason \"no stash.\" :instead \"jj new.\"))";
    let verdict = run_with_facts(text, bash("git stash"), &stubs).unwrap();
    assert_eq!(
      verdict.decision,
      Decision::Deny {
        reason: "claude-guard denied `git stash`: no stash. Instead: jj new. \
                 (jj root is /x; no dirty files)"
          .into()
      }
    );
    assert_eq!(
      verdict.evidence.as_deref(),
      Some("jj root is /x; no dirty files")
    );
    assert_eq!(verdict.unknown, None);
    // No reasons, no parentheses.
    let text = "(rule r (warn [git -... stash ...] :when (quiet?) :reason \"hm.\"))";
    let verdict = run_with_facts(text, bash("git stash"), &stubs).unwrap();
    assert_eq!(
      verdict.decision,
      Decision::Warn {
        context: "claude-guard noted `git stash`: hm.".into()
      }
    );
    assert_eq!(verdict.evidence, None);
  }

  #[test]
  fn a_declared_fact_runs_its_program_and_its_answer_decides() {
    let dir = tempfile::tempdir().unwrap();
    let text = r#"
      (fact managed? (exec "sh" "-c" "echo '{\"holds\": true, \"reason\": \"the script said so\"}'")
        :lifetime fresh :timeout "5s")
      (fact slow? (exec "sh" "-c" "sleep 5") :lifetime fresh :timeout "100ms")
      (fact arg? (exec "sh" "-c" "test \"$1\" = /tmp/x && echo '{\"holds\": true}' || echo '{\"holds\": false}'" "script")
        :lifetime fresh :timeout "5s")
      (rule r
        (deny [git -... stash ...] :when (managed?) :reason "no stash." :instead "jj new.")
        (deny [cargo clean] :when (slow?) :reason "slow." :instead "wait.")
        (deny [cp ... ?dst] :when (arg? ?dst) :reason "no." :instead "elsewhere."))
    "#;
    let rules = Ruleset::from_text(text).unwrap_or_else(|e| panic!("{e}"));
    let run = |command: &str| {
      let mut input = input(bash(command));
      input.cwd = dir.path().to_path_buf().into();
      rules.evaluate(&Context::new(input, &rules.declarations))
    };
    assert_eq!(
      run("git stash").unwrap().decision,
      Decision::Deny {
        reason: "claude-guard denied `git stash`: no stash. Instead: jj new. (the script said so)"
          .into()
      }
    );
    assert_eq!(
      run("cargo clean").unwrap().decision,
      Decision::Ask {
        reason: "claude-guard asks about `cargo clean`: slow. Instead: wait. \
                 (slow? is unknown: timed out after 100ms)"
          .into()
      }
    );
    assert!(matches!(
      run("cp a /tmp/x").unwrap().decision,
      Decision::Deny { .. }
    ));
    assert_eq!(run("cp a /var/x"), None);
  }

  #[test]
  fn a_file_tool_row_with_an_unknown_condition_asks() {
    let stubs = [("fresh?", Answer::unknown("no read recorded"))];
    let text = "(rule r (deny (write ?p) :when (fresh?) :reason \"stale.\" :instead \"read it.\"))";
    let verdict = run_with_facts(
      text,
      Tool::Write {
        path: "/x/a.rs".into(),
      },
      &stubs,
    )
    .unwrap();
    assert_eq!(
      verdict.decision,
      Decision::Ask {
        reason: "claude-guard asks about `/x/a.rs`: stale. Instead: read it. (fresh? is unknown: no read recorded)".into()
      }
    );
    assert_eq!(
      verdict.bindings,
      Bindings::from([(Var::from("p"), "/x/a.rs".to_string())])
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
