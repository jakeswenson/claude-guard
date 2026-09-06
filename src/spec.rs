//! The executable spec: `check` forms in the rule language, run as tests.
//!
//! ```text
//! check := (check [pattern] matches "command")
//!        | (check [pattern] misses  "command")
//!        | (check [pattern] binds   "command" <set>)
//!        | (check (cond) holds|fails|unknown [:with <pairs>] [:cwd "path"] [:ancestors <fs>])
//! set   := (?name "word")*            ; one binding set, as pairs
//!        | ((?name "word")*)+         ; several sets, each in its own list
//! fs    := ("name" ...)               ; entries an ancestor of cwd has; the rest do not
//!        | unknown                    ; the filesystem answers unknown
//! ```
//!
//! A command must segment to one simple command. `binds` passes when the
//! matcher's binding sets are exactly the ones written, in order. A
//! condition check parses its condition with the binders `:with` gives
//! it, so an unbound binder is a failure of the check, not a panic.
//!
//! Files under `spec/` are the spec. Every failing check prints as
//! `file:line:col: message`, and the test fails once at the end with the
//! count, so one run shows everything.

// The spec runner is used by its own test until a subcommand exposes it.
#![allow(dead_code)]

use std::fmt;
use std::path::{Path, PathBuf};

use crate::cond::{self, Cond, Env, Fs, Scope, Truth};
use crate::pattern::{Bindings, Pattern, Var};
use crate::segment;
use crate::sexp::{self, Kind as Sx, Node, Span};
use crate::syntax::{self, Subject, TypeError};

/// One check that did not pass, or could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
  pub file: PathBuf,
  pub span: Span,
  pub message: String,
}

impl fmt::Display for Failure {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    write!(f, "{}:{}: {}", self.file.display(), self.span, self.message)
  }
}

/// What a run of one or more files found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
  pub passed: usize,
  pub failures: Vec<Failure>,
}

/// Run every `.scm` file under `dir`, in name order.
pub fn run_dir(dir: &Path) -> Outcome {
  let mut outcome = Outcome::default();
  let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
    Ok(entries) => entries
      .filter_map(|e| e.ok().map(|e| e.path()))
      .filter(|p| p.extension().is_some_and(|ext| ext == "scm"))
      .collect(),
    Err(e) => {
      outcome.failures.push(Failure {
        file: dir.to_path_buf(),
        span: Span { line: 0, col: 0 },
        message: format!("cannot read spec directory: {e}"),
      });
      return outcome;
    }
  };
  files.sort();
  for file in files {
    let text = match std::fs::read_to_string(&file) {
      Ok(text) => text,
      Err(e) => {
        outcome.failures.push(Failure {
          file,
          span: Span { line: 0, col: 0 },
          message: format!("cannot read: {e}"),
        });
        continue;
      }
    };
    let one = run_text(&file, &text);
    outcome.passed += one.passed;
    outcome.failures.extend(one.failures);
  }
  outcome
}

/// Run the checks in `text`, attributing failures to `file`.
pub fn run_text(
  file: &Path,
  text: &str,
) -> Outcome {
  let mut outcome = Outcome::default();
  let forms = match sexp::read_all(text) {
    Ok(forms) => forms,
    Err(e) => {
      outcome.failures.push(Failure {
        file: file.to_path_buf(),
        span: e.span,
        message: e.message,
      });
      return outcome;
    }
  };
  for form in &forms {
    match run_check(form) {
      Ok(()) => outcome.passed += 1,
      Err(e) => outcome.failures.push(Failure {
        file: file.to_path_buf(),
        span: e.span,
        message: e.message,
      }),
    }
  }
  outcome
}

fn err<T>(
  span: Span,
  message: impl Into<String>,
) -> Result<T, TypeError> {
  Err(TypeError {
    span,
    message: message.into(),
  })
}

/// Parse and run one `(check ...)` form. `Err` is a failure of any kind:
/// a malformed check or a check whose claim is false.
fn run_check(form: &Node) -> Result<(), TypeError> {
  let Sx::List(items) = &form.kind else {
    return err(form.span, "expected (check ...)");
  };
  let [head, subject, verb, args @ ..] = items.as_slice() else {
    return err(form.span, "expected (check <subject> <verb> ...)");
  };
  match &head.kind {
    Sx::Symbol(s) if s == "check" => {}
    _ => return err(head.span, "expected (check ...)"),
  }
  let Sx::Symbol(verb_name) = &verb.kind else {
    return err(
      verb.span,
      "expected a verb: matches, misses, binds, holds, fails, unknown",
    );
  };
  match &subject.kind {
    Sx::Pattern(_) => {
      let Subject::Command(pattern) = syntax::parse_subject(subject)? else {
        unreachable!("a bracket subject is a command pattern");
      };
      check_pattern(form, &pattern, verb, verb_name, args)
    }
    Sx::List(_) => check_cond(form, subject, verb, verb_name, args),
    _ => err(subject.span, "expected a [pattern] or a (condition)"),
  }
}

// --- pattern checks ---

fn check_pattern(
  form: &Node,
  pattern: &Pattern,
  verb: &Node,
  verb_name: &str,
  args: &[Node],
) -> Result<(), TypeError> {
  let Some((command_node, rest)) = args.split_first() else {
    return err(form.span, format!("`{verb_name}` needs a command string"));
  };
  let Sx::Str(command) = &command_node.kind else {
    return err(command_node.span, "expected a command string");
  };
  let mut segments = segment::segment(command).map_err(|e| TypeError {
    span: command_node.span,
    message: format!("command does not parse: {e}"),
  })?;
  if segments.commands.len() != 1 {
    return err(
      command_node.span,
      format!(
        "command must be one simple command, found {}",
        segments.commands.len()
      ),
    );
  }
  let found = pattern.bindings(&segments.commands.remove(0));

  match verb_name {
    "matches" => {
      no_extra(rest)?;
      if found.is_empty() {
        return err(form.span, "expected a match, got none");
      }
    }
    "misses" => {
      no_extra(rest)?;
      if !found.is_empty() {
        return err(
          form.span,
          format!("expected no match, got {}", show_sets(&found)),
        );
      }
    }
    "binds" => {
      let expected = binding_sets(rest)?;
      if found != expected {
        return err(
          form.span,
          format!(
            "expected {}, got {}",
            show_sets(&expected),
            show_sets(&found)
          ),
        );
      }
    }
    other => return err(verb.span, format!("unknown pattern verb `{other}`")),
  }
  Ok(())
}

fn no_extra(rest: &[Node]) -> Result<(), TypeError> {
  match rest.first() {
    Some(extra) => err(extra.span, format!("unexpected `{extra}`")),
    None => Ok(()),
  }
}

/// `(?x "a") (?y "b")` is one set; `((?x "a")) ((?x "b"))` is two.
fn binding_sets(nodes: &[Node]) -> Result<Vec<Bindings>, TypeError> {
  let is_pair = |node: &Node| match &node.kind {
    Sx::List(items) => {
      matches!(items.first().map(|n| &n.kind), Some(Sx::Symbol(s)) if s.starts_with('?'))
    }
    _ => false,
  };
  if nodes.iter().all(is_pair) {
    return Ok(vec![pairs(nodes)?]);
  }
  nodes
    .iter()
    .map(|node| match &node.kind {
      Sx::List(items) if items.iter().all(is_pair) => pairs(items),
      _ => err(
        node.span,
        "expected a binding set: (?name \"word\") pairs, or a list of them",
      ),
    })
    .collect()
}

/// `(?x "a") (?y "b")` to a binding set.
fn pairs(nodes: &[Node]) -> Result<Bindings, TypeError> {
  let mut bindings = Bindings::new();
  for node in nodes {
    let Sx::List(items) = &node.kind else {
      return err(node.span, "expected (?name \"word\")");
    };
    let [name, value] = items.as_slice() else {
      return err(node.span, "expected (?name \"word\")");
    };
    let var = match &name.kind {
      Sx::Symbol(s) => match s.strip_prefix('?') {
        Some(n) if !n.is_empty() => Var::from(n),
        _ => return err(name.span, "expected a ?binder"),
      },
      _ => return err(name.span, "expected a ?binder"),
    };
    let Sx::Str(word) = &value.kind else {
      return err(value.span, "expected a string");
    };
    if bindings.insert(var, word.clone()).is_some() {
      return err(name.span, format!("`{name}` given twice"));
    }
  }
  Ok(bindings)
}

fn show_sets(sets: &[Bindings]) -> String {
  if sets.is_empty() {
    return "no match".into();
  }
  sets
    .iter()
    .map(|set| {
      let pairs: Vec<String> = set.iter().map(|(k, v)| format!("(?{k} {v:?})")).collect();
      format!("({})", pairs.join(" "))
    })
    .collect::<Vec<_>>()
    .join(" ")
}

// --- condition checks ---

/// The filesystem a check describes with `:ancestors`.
struct SpecFs {
  present: Vec<String>,
  unknown: bool,
}

impl Fs for SpecFs {
  fn ancestor_has(
    &self,
    _cwd: &Path,
    name: &str,
  ) -> Truth {
    if self.unknown {
      Truth::Unknown
    } else {
      self.present.iter().any(|p| p == name).into()
    }
  }
}

fn check_cond(
  form: &Node,
  subject: &Node,
  verb: &Node,
  verb_name: &str,
  args: &[Node],
) -> Result<(), TypeError> {
  let expected = match verb_name {
    "holds" => Truth::True,
    "fails" => Truth::False,
    "unknown" => Truth::Unknown,
    other => return err(verb.span, format!("unknown condition verb `{other}`")),
  };

  let mut bindings = Bindings::new();
  let mut cwd = PathBuf::from("/spec");
  let mut fs = SpecFs {
    present: vec![],
    unknown: false,
  };
  let mut i = 0;
  while i < args.len() {
    let key = &args[i];
    let Sx::Keyword(name) = &key.kind else {
      return err(
        key.span,
        format!("unexpected `{key}`; expected :with, :cwd, or :ancestors"),
      );
    };
    let Some(value) = args.get(i + 1) else {
      return err(key.span, format!("`:{name}` needs a value"));
    };
    match name.as_str() {
      "with" => {
        let Sx::List(items) = &value.kind else {
          return err(value.span, "`:with` takes a list of (?name \"word\") pairs");
        };
        bindings = pairs(items)?;
      }
      "cwd" => {
        let Sx::Str(path) = &value.kind else {
          return err(value.span, "`:cwd` takes a string");
        };
        cwd = PathBuf::from(path);
      }
      "ancestors" => match &value.kind {
        Sx::Symbol(s) if s == "unknown" => fs.unknown = true,
        Sx::List(items) => {
          fs.present = items
            .iter()
            .map(|item| match &item.kind {
              Sx::Str(s) => Ok(s.clone()),
              _ => err(item.span, "`:ancestors` takes strings"),
            })
            .collect::<Result<_, _>>()?;
        }
        _ => {
          return err(
            value.span,
            "`:ancestors` takes a list of strings or `unknown`",
          );
        }
      },
      other => return err(key.span, format!("unknown keyword `:{other}`")),
    }
    i += 2;
  }

  let scope = Scope::Row(bindings.keys().cloned().collect());
  let condition: Cond = cond::parse(subject, &scope)?;
  let env = Env { cwd: &cwd, fs: &fs };
  let got = condition.eval(&env, &bindings);
  if got != expected {
    return err(
      form.span,
      format!("expected {expected:?}, got {got:?}").to_lowercase(),
    );
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn spec_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("spec")
  }

  /// The spec itself. Every failure prints on its own line.
  #[test]
  fn the_spec_passes() {
    let outcome = run_dir(&spec_dir());
    let report: Vec<String> = outcome.failures.iter().map(ToString::to_string).collect();
    assert!(
      outcome.failures.is_empty(),
      "{} of {} checks failed:\n{}",
      outcome.failures.len(),
      outcome.passed + outcome.failures.len(),
      report.join("\n")
    );
    assert!(outcome.passed > 0, "no checks ran");
  }

  fn failures(text: &str) -> Vec<String> {
    run_text(Path::new("t.scm"), text)
      .failures
      .iter()
      .map(ToString::to_string)
      .collect()
  }

  fn passes(text: &str) {
    let found = failures(text);
    assert!(found.is_empty(), "{}", found.join("\n"));
  }

  // --- the runner reports what a false claim looks like ---

  #[test]
  fn a_true_claim_passes_and_counts() {
    let outcome = run_text(
      Path::new("t.scm"),
      "(check [git stash] matches \"git stash\")\n(check [git stash] misses \"git log\")",
    );
    assert_eq!(outcome.passed, 2);
    assert!(outcome.failures.is_empty());
  }

  #[test]
  fn a_false_pattern_claim_says_what_it_got() {
    assert_eq!(
      failures("(check [git stash] matches \"git log\")"),
      ["t.scm:1:1: expected a match, got none"]
    );
    assert_eq!(
      failures("(check [git ?x] misses \"git log\")"),
      ["t.scm:1:1: expected no match, got ((?x \"log\"))"]
    );
    assert_eq!(
      failures("(check [cp ... ?dst] binds \"cp a b\" (?dst \"a\"))"),
      ["t.scm:1:1: expected ((?dst \"a\")), got ((?dst \"b\"))"]
    );
    assert_eq!(
      failures("(check [cp ... ?dst] binds \"cp\" (?dst \"a\"))"),
      ["t.scm:1:1: expected ((?dst \"a\")), got no match"]
    );
  }

  #[test]
  fn binding_sets_are_written_as_pairs_or_lists_of_pairs() {
    passes("(check [cp ... ?dst] binds \"cp a b\" (?dst \"b\"))");
    passes("(check [git ...] binds \"git a\" ())");
    passes("(check [f ... ?x ...] binds \"f a b\" ((?x \"a\")) ((?x \"b\")))");
    passes("(check [cp ?a ?b] binds \"cp x y\" (?a \"x\") (?b \"y\"))");
    assert_eq!(
      failures("(check [cp ?a] binds \"cp x\" (?a \"x\") (?a \"y\"))"),
      ["t.scm:1:39: `?a` given twice"]
    );
  }

  #[test]
  fn a_false_condition_claim_says_what_it_got() {
    assert_eq!(
      failures("(check (under? ?p \"/tmp\") holds :with ((?p \"/var/x\")))"),
      ["t.scm:1:1: expected true, got false"]
    );
    assert_eq!(
      failures("(check (ancestor-has? \".jj\") fails :ancestors unknown)"),
      ["t.scm:1:1: expected false, got unknown"]
    );
  }

  #[test]
  fn a_condition_check_defaults_to_no_bindings_an_empty_fs_and_a_cwd() {
    passes("(check (ancestor-has? \".jj\") fails)");
    passes("(check (under? \"x\" \"/spec\") holds)");
  }

  // --- malformed checks are failures with a position ---

  #[test]
  fn malformed_checks_fail_with_a_position() {
    assert_eq!(
      failures("(chek [a] matches \"a\")"),
      ["t.scm:1:2: expected (check ...)"]
    );
    assert_eq!(
      failures("(check [a])"),
      ["t.scm:1:1: expected (check <subject> <verb> ...)"]
    );
    assert_eq!(
      failures("(check [a] eats \"a\")"),
      ["t.scm:1:12: unknown pattern verb `eats`"]
    );
    assert_eq!(
      failures("(check (ancestor-has? \"x\") eats)"),
      ["t.scm:1:28: unknown condition verb `eats`"]
    );
    assert_eq!(
      failures("(check \"a\" matches \"a\")"),
      ["t.scm:1:8: expected a [pattern] or a (condition)"]
    );
    assert_eq!(
      failures("(check [a] matches a)"),
      ["t.scm:1:20: expected a command string"]
    );
    assert_eq!(
      failures("(check [a] matches \"a; b\")"),
      ["t.scm:1:20: command must be one simple command, found 2"]
    );
    assert_eq!(
      failures("(check [a] matches \"a &&\")"),
      ["t.scm:1:20: command does not parse: syntax error at end of input"]
    );
    assert_eq!(
      failures("(check [a] matches \"a\" extra)"),
      ["t.scm:1:24: unexpected `extra`"]
    );
    assert_eq!(
      failures("(check [a] binds \"a\" (x \"1\"))"),
      ["t.scm:1:22: expected a binding set: (?name \"word\") pairs, or a list of them"]
    );
    assert_eq!(
      failures("(check (under? ?p \"/tmp\") holds :with ((?q \"x\")))"),
      ["t.scm:1:16: `?p` is not bound by this pattern"]
    );
    assert_eq!(
      failures("(check (ancestor-has? \"x\") holds :fs ())"),
      ["t.scm:1:34: unknown keyword `:fs`"]
    );
    assert_eq!(
      failures("(check (ancestor-has? \"x\") holds :cwd)"),
      ["t.scm:1:34: `:cwd` needs a value"]
    );
    assert_eq!(
      failures("(check (ancestor-has? \"x\") holds :ancestors maybe)"),
      ["t.scm:1:45: `:ancestors` takes a list of strings or `unknown`"]
    );
    assert_eq!(
      failures("(check [a /**] matches \"a\")"),
      ["t.scm:1:11: path prefixes go in the condition: (under? ?p \"\")"]
    );
  }

  #[test]
  fn a_file_that_does_not_read_is_one_failure() {
    assert_eq!(
      // The reader names the innermost unterminated form.
      failures("(check [a] matches \"a\"\n(check"),
      ["t.scm:2:1: unterminated `(`"]
    );
  }

  #[test]
  fn every_failing_check_is_reported() {
    let found =
      failures("(check [a] matches \"b\")\n(check [a] matches \"a\")\n(check [a] misses \"a\")");
    assert_eq!(
      found,
      [
        "t.scm:1:1: expected a match, got none",
        "t.scm:3:1: expected no match, got ()"
      ]
    );
  }

  #[test]
  fn a_missing_directory_is_one_failure() {
    let outcome = run_dir(Path::new("/definitely/not/here"));
    assert_eq!(outcome.passed, 0);
    assert_eq!(outcome.failures.len(), 1);
    assert!(
      outcome.failures[0]
        .message
        .starts_with("cannot read spec directory")
    );
  }
}
