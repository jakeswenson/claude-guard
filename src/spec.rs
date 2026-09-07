//! The executable spec: `check` forms in the rule language, run as tests.
//!
//! ```text
//! check := (check [pattern] matches "command" [decls])
//!        | (check [pattern] misses  "command" [decls])
//!        | (check [pattern] binds   "command" <set> [decls])
//!        | (check "command" elaborates (name item...) [decls])
//!        | (check (cond) holds|fails|unknown ["reason"]
//!                 [:with <pairs>] [:cwd "path"] [:ancestors <fs>] [:facts (<stub>...)])
//!        | (check (rule ...) denies|asks|warns|passes "command" ["text"]
//!                 [:cwd "path"] [:ancestors <fs>] [:facts (<stub>...)] [:asked (<call>...)])
//! decls := :commands ((command ...) ...)   ; declarations in force for this check
//! set   := (?name "word")*            ; one binding set, as pairs
//!        | ((?name "word")*)+         ; several sets, each in its own list
//! fs    := ("name" ...)               ; entries an ancestor of cwd has; the rest do not
//!        | unknown                    ; ancestor-has? answers unknown
//! stub  := (<name> holds|fails|unknown ["reason"] [:args ("..." ...)])
//!                                     ; a fact and its fixed answer; with :args, that
//!                                     ; answer only for those arguments, else a fail
//! call  := (<name> "arg"...)          ; one fact the evaluation asked, with its arguments
//! ```
//!
//! A command must segment to one simple command. `binds` passes when the
//! matcher's binding sets are exactly the ones written, in order. A
//! condition check parses its condition with the binders `:with` gives
//! it, so an unbound binder is a failure of the check, not a panic; its
//! facts are the built-ins with `:ancestors` and `:facts` stood in by
//! name, so no check touches the disk. A `"reason"` after the verb must
//! equal the answer's reason. A rule check runs one rule through the
//! engine against one Bash command with the same stand-ins; its `"text"`
//! must equal the rendered decision, and its `:asked` list must equal
//! what the evaluation asked, in order, which is what the log record
//! carries. An `elaborates` check compares against the notation
//! [`show`] renders.
//!
//! Files under `spec/` are the spec. Every failing check prints as
//! `file:line:col: message`, and the test fails once at the end with the
//! count, so one run shows everything.

// The spec runner is used by its own test until a subcommand exposes it.
#![allow(dead_code)]

use std::fmt;
use std::path::{Path, PathBuf};

use crate::cond::{self, Cond, Scope};
use crate::elaborate::{Declarations, Elaborated, Inner, Part};
use crate::facts::{Ancestors, Answer, ArgStub, Call, Facts, Stub, Truth};
use crate::input::{HookInput, Tool};
use crate::load::Source;
use crate::output::Decision;
use crate::pattern::{Bindings, Pattern, Var};
use crate::rules::{Context, Kind, Ruleset};
use crate::segment::{self, RedirectKind, SimpleCommand, Word};
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
      "expected a verb: matches, misses, binds, elaborates, holds, fails, unknown, denies, asks, warns, passes",
    );
  };
  match &subject.kind {
    Sx::Pattern(_) => {
      let Subject::Command(pattern) = syntax::parse_subject(subject)? else {
        unreachable!("a bracket subject is a command pattern");
      };
      check_pattern(form, &pattern, verb, verb_name, args)
    }
    Sx::List(items) if matches!(items.first().map(|n| &n.kind), Some(Sx::Symbol(s)) if s == "rule") => {
      check_rule(form, subject, verb, verb_name, args)
    }
    Sx::List(_) => check_cond(form, subject, verb, verb_name, args),
    Sx::Str(_) => check_elaboration(form, subject, verb, verb_name, args),
    _ => err(
      subject.span,
      "expected a [pattern], a (condition), a (rule ...), or a \"command\"",
    ),
  }
}

/// Split a trailing `:commands (...)` clause off `args`, returning the
/// declarations it names and the arguments before it. No clause means
/// no declarations, so every word is a plain unit.
fn split_commands(args: &[Node]) -> Result<(&[Node], Declarations), TypeError> {
  let Some(at) = args
    .iter()
    .position(|n| matches!(&n.kind, Sx::Keyword(k) if k == "commands"))
  else {
    return Ok((args, Declarations::new()));
  };
  let Some(value) = args.get(at + 1) else {
    return err(
      args[at].span,
      "`:commands` needs a list of (command ...) forms",
    );
  };
  if let Some(extra) = args.get(at + 2) {
    return err(
      extra.span,
      format!("unexpected `{extra}` after `:commands`"),
    );
  }
  let Sx::List(forms) = &value.kind else {
    return err(
      value.span,
      "`:commands` takes a list of (command ...) forms",
    );
  };
  let file = syntax::parse(forms, &Facts::builtin()).map_err(|mut errors| errors.remove(0))?;
  if let Some(rule) = file.rules.first() {
    return err(
      rule.span,
      "`:commands` takes (command ...) forms, not rules",
    );
  }
  let mut declarations = Declarations::new();
  for command in file.commands {
    declarations.declare(&command.name, command.declaration);
  }
  Ok((&args[..at], declarations))
}

/// Segment one command string into its only simple command.
fn one_command(node: &Node) -> Result<SimpleCommand, TypeError> {
  let Sx::Str(command) = &node.kind else {
    return err(node.span, "expected a command string");
  };
  let mut segments = segment::segment(command).map_err(|e| TypeError {
    span: node.span,
    message: format!("command does not parse: {e}"),
  })?;
  if segments.commands.len() != 1 {
    return err(
      node.span,
      format!(
        "command must be one simple command, found {}",
        segments.commands.len()
      ),
    );
  }
  Ok(segments.commands.remove(0))
}

// --- pattern checks ---

fn check_pattern(
  form: &Node,
  pattern: &Pattern,
  verb: &Node,
  verb_name: &str,
  args: &[Node],
) -> Result<(), TypeError> {
  let (args, declarations) = split_commands(args)?;
  let Some((command_node, rest)) = args.split_first() else {
    return err(form.span, format!("`{verb_name}` needs a command string"));
  };
  let command = one_command(command_node)?;
  let elaborated = declarations.elaborate(&command);
  let found = pattern.bindings_in(&elaborated.units(), &elaborated.redirects);

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

// --- elaboration checks ---

/// `(check "command" elaborates (name item...) [:commands (...)])`. The
/// actual elaboration is rendered in the same form and compared as text,
/// so a failure prints both sides in the notation the spec uses.
fn check_elaboration(
  form: &Node,
  subject: &Node,
  verb: &Node,
  verb_name: &str,
  args: &[Node],
) -> Result<(), TypeError> {
  if verb_name != "elaborates" {
    return err(verb.span, format!("unknown elaboration verb `{verb_name}`"));
  }
  let (args, declarations) = split_commands(args)?;
  let [expected] = args else {
    return err(form.span, "`elaborates` takes one expected form");
  };
  let command = one_command(subject)?;
  let got = show(&declarations.elaborate(&command)).flat();
  let wanted = expected.flat();
  if got != wanted {
    return err(form.span, format!("expected {wanted}, got {got}"));
  }
  Ok(())
}

/// An elaboration in the spec's notation:
///
/// ```text
/// (git (option "-C" C ".") "clean" (option "-fxd" f x d)
///      (> "out") (subcommand clean) (inner (command (sed "-i"))))
/// ```
///
/// The name is a symbol; an argument is a string; an option is its text,
/// then its flags as symbols, then its value as a string or `(attached
/// "v")`; a dynamic word is `(dynamic "text")`; a redirect is its
/// operator and target. `subcommand` and `inner` come last when present.
fn show(e: &Elaborated) -> Node {
  fn at(kind: Sx) -> Node {
    Node {
      span: Span { line: 0, col: 0 },
      kind,
    }
  }
  fn word(w: &Word) -> Node {
    match w {
      Word::Literal(t) => at(Sx::Str(t.clone())),
      Word::Dynamic(t) => at(Sx::List(vec![
        at(Sx::Symbol("dynamic".into())),
        at(Sx::Str(t.clone())),
      ])),
    }
  }
  let mut items = Vec::new();
  let mut name = None;
  for part in &e.parts {
    match part {
      Part::Name(Word::Literal(t)) => name = Some(at(Sx::Symbol(t.clone()))),
      Part::Name(w) => name = Some(word(w)),
      Part::Arg(w) => items.push(word(w)),
      Part::Option(group) => {
        let mut option = vec![at(Sx::Symbol("option".into())), word(&group.text)];
        option.extend(group.flags.iter().map(|f| at(Sx::Symbol(f.clone()))));
        if let Some(value) = &group.value {
          option.push(if value.attached {
            at(Sx::List(vec![
              at(Sx::Symbol("attached".into())),
              word(&value.text),
            ]))
          } else {
            word(&value.text)
          });
        }
        items.push(at(Sx::List(option)));
      }
    }
  }
  for redirect in &e.redirects {
    let op = match redirect.kind {
      RedirectKind::Write => ">",
      RedirectKind::Append => ">>",
      RedirectKind::Read => "<",
    };
    items.push(at(Sx::List(vec![
      at(Sx::Symbol(op.into())),
      word(&redirect.target),
    ])));
  }
  if !e.subcommand.is_empty() {
    let mut sub = vec![at(Sx::Symbol("subcommand".into()))];
    sub.extend(e.subcommand.iter().map(|s| at(Sx::Symbol(s.clone()))));
    items.push(at(Sx::List(sub)));
  }
  if let Some(inner) = &e.inner {
    let body = match inner {
      Inner::Command(inner) => at(Sx::List(vec![
        at(Sx::Symbol("command".into())),
        show(inner),
      ])),
      Inner::Script(text) => at(Sx::List(vec![
        at(Sx::Symbol("script".into())),
        at(Sx::Str(text.clone())),
      ])),
    };
    items.push(at(Sx::List(vec![at(Sx::Symbol("inner".into())), body])));
  }
  let mut list = vec![name.unwrap_or_else(|| at(Sx::Symbol("_".into())))];
  list.extend(items);
  at(Sx::List(list))
}

// --- condition checks ---

/// `holds`, `fails`, or `unknown` as a truth value.
fn truth_verb(
  node: &Node,
  what: &str,
) -> Result<Truth, TypeError> {
  match &node.kind {
    Sx::Symbol(s) if s == "holds" => Ok(Truth::True),
    Sx::Symbol(s) if s == "fails" => Ok(Truth::False),
    Sx::Symbol(s) if s == "unknown" => Ok(Truth::Unknown),
    Sx::Symbol(other) => err(node.span, format!("unknown {what} verb `{other}`")),
    _ => err(
      node.span,
      format!("expected a {what} verb: holds, fails, unknown"),
    ),
  }
}

/// One `(<name> holds|fails|unknown ["reason"])` stub, registered in
/// `facts`.
fn declare_stub(
  node: &Node,
  facts: &mut Facts,
) -> Result<(), TypeError> {
  let Sx::List(items) = &node.kind else {
    return err(
      node.span,
      "`:facts` takes (name holds|fails|unknown [\"reason\"]) forms",
    );
  };
  let [name, verb, rest @ ..] = items.as_slice() else {
    return err(
      node.span,
      "`:facts` takes (name holds|fails|unknown [\"reason\"]) forms",
    );
  };
  let Sx::Symbol(name) = &name.kind else {
    return err(name.span, "a stubbed fact's name is a symbol");
  };
  let truth = truth_verb(verb, "stub")?;
  let (reason, rest) = match rest {
    [node, rest @ ..] if matches!(&node.kind, Sx::Str(_)) => {
      let Sx::Str(reason) = &node.kind else {
        unreachable!("matched a string");
      };
      (Some(reason.clone()), rest)
    }
    rest => (None, rest),
  };
  if truth == Truth::Unknown && reason.is_none() {
    return err(verb.span, "an unknown stub needs a reason");
  }
  let answer = Answer { truth, reason };
  match rest {
    [] => facts.declare(name.as_str(), Stub(answer)),
    [key, value] if matches!(&key.kind, Sx::Keyword(k) if k == "args") => {
      let args = string_list(value, "`:args` takes a list of strings")?;
      facts.declare(name.as_str(), ArgStub { args, answer });
    }
    [extra, ..] => {
      return err(
        extra.span,
        format!("unexpected `{extra}`; a stub ends with an optional :args (\"...\" ...)"),
      );
    }
  }
  Ok(())
}

/// A list of strings, or `message` at the offending node.
fn string_list(
  node: &Node,
  message: &str,
) -> Result<Vec<String>, TypeError> {
  let Sx::List(items) = &node.kind else {
    return err(node.span, message);
  };
  items
    .iter()
    .map(|item| match &item.kind {
      Sx::Str(s) => Ok(s.clone()),
      _ => err(item.span, message),
    })
    .collect()
}

/// `(<name> "arg"...)` forms: what a rule check expects the evaluation
/// to have asked, in order.
fn asked_list(node: &Node) -> Result<Vec<(String, Vec<String>)>, TypeError> {
  const EXPECTED: &str = "`:asked` takes a list of (name \"arg\"...) forms";
  let Sx::List(items) = &node.kind else {
    return err(node.span, EXPECTED);
  };
  items
    .iter()
    .map(|item| {
      let Sx::List(parts) = &item.kind else {
        return err(item.span, EXPECTED);
      };
      let Some((name, args)) = parts.split_first() else {
        return err(item.span, EXPECTED);
      };
      let Sx::Symbol(name) = &name.kind else {
        return err(name.span, EXPECTED);
      };
      let args = args
        .iter()
        .map(|arg| match &arg.kind {
          Sx::Str(s) => Ok(s.clone()),
          _ => err(arg.span, EXPECTED),
        })
        .collect::<Result<Vec<_>, _>>()?;
      Ok((name.clone(), args))
    })
    .collect()
}

/// The world a condition or rule check runs in: the binding set, the
/// cwd, and the facts, all from keywords and none from the disk.
struct Setup {
  bindings: Bindings,
  cwd: String,
  facts: Facts,
  /// What a rule check expects the evaluation to have asked, in order.
  asked: Option<Vec<(String, Vec<String>)>>,
}

/// An optional `"text"` right after the verb, and the arguments after it.
fn leading_string(args: &[Node]) -> (Option<&str>, &[Node]) {
  match args.split_first() {
    Some((first, rest)) => match &first.kind {
      Sx::Str(text) => (Some(text.as_str()), rest),
      _ => (None, args),
    },
    None => (None, args),
  }
}

/// Read `:with`, `:cwd`, `:ancestors`, `:facts`, and `:asked`. `:with`
/// is refused when `with_allowed` is false, since a rule check binds
/// from its own pattern; `:asked` is only for rule checks, since only an
/// evaluation asks.
fn setup(
  args: &[Node],
  with_allowed: bool,
) -> Result<Setup, TypeError> {
  let expected_keywords = if with_allowed {
    ":with, :cwd, :ancestors, or :facts"
  } else {
    ":cwd, :ancestors, :facts, or :asked"
  };
  let mut bindings = Bindings::new();
  let mut cwd = String::from("/spec");
  let mut asked = None;
  let mut facts = Facts::builtin();
  facts.declare(
    "ancestor-has?",
    Ancestors {
      present: vec![],
      unknown: false,
    },
  );
  let mut i = 0;
  while i < args.len() {
    let key = &args[i];
    let Sx::Keyword(name) = &key.kind else {
      return err(
        key.span,
        format!("unexpected `{key}`; expected {expected_keywords}"),
      );
    };
    let Some(value) = args.get(i + 1) else {
      return err(key.span, format!("`:{name}` needs a value"));
    };
    match name.as_str() {
      "with" if with_allowed => {
        let Sx::List(items) = &value.kind else {
          return err(value.span, "`:with` takes a list of (?name \"word\") pairs");
        };
        bindings = pairs(items)?;
      }
      "with" => {
        return err(
          key.span,
          "`:with` is for condition checks; a rule check binds from its own pattern",
        );
      }
      "asked" if !with_allowed => asked = Some(asked_list(value)?),
      "asked" => {
        return err(
          key.span,
          "`:asked` is for rule checks; only an evaluation asks facts",
        );
      }
      "cwd" => {
        let Sx::Str(path) = &value.kind else {
          return err(value.span, "`:cwd` takes a string");
        };
        cwd = path.clone();
      }
      "ancestors" => {
        let ancestors = match &value.kind {
          Sx::Symbol(s) if s == "unknown" => Ancestors {
            present: vec![],
            unknown: true,
          },
          Sx::List(items) => Ancestors {
            present: items
              .iter()
              .map(|item| match &item.kind {
                Sx::Str(s) => Ok(s.clone()),
                _ => err(item.span, "`:ancestors` takes strings"),
              })
              .collect::<Result<_, _>>()?,
            unknown: false,
          },
          _ => {
            return err(
              value.span,
              "`:ancestors` takes a list of strings or `unknown`",
            );
          }
        };
        facts.declare("ancestor-has?", ancestors);
      }
      "facts" => {
        let Sx::List(items) = &value.kind else {
          return err(
            value.span,
            "`:facts` takes a list of (name verb [\"reason\"]) forms",
          );
        };
        for item in items {
          declare_stub(item, &mut facts)?;
        }
      }
      other => return err(key.span, format!("unknown keyword `:{other}`")),
    }
    i += 2;
  }
  Ok(Setup {
    bindings,
    cwd,
    facts,
    asked,
  })
}

/// `(a? "x") (b?)`: asked facts in the spec's notation.
fn show_asked(asked: &[(String, Vec<String>)]) -> String {
  if asked.is_empty() {
    return "nothing".into();
  }
  asked
    .iter()
    .map(|(name, args)| {
      let mut parts = vec![name.clone()];
      parts.extend(args.iter().map(|a| format!("{a:?}")));
      format!("({})", parts.join(" "))
    })
    .collect::<Vec<_>>()
    .join(" ")
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
  let (expected_reason, args) = leading_string(args);
  let Setup {
    bindings,
    cwd,
    facts,
    asked: _,
  } = setup(args, true)?;

  let scope = Scope::Row(bindings.keys().cloned().collect());
  let condition: Cond = cond::parse(subject, &scope, &facts)?;
  let call = Call::at(Path::new(&cwd));
  let got = condition.eval(&facts, &call, &bindings);
  if got.truth != expected {
    return err(
      form.span,
      format!("expected {expected:?}, got {:?}", got.truth).to_lowercase(),
    );
  }
  if let Some(wanted) = expected_reason
    && got.reason.as_deref() != Some(wanted)
  {
    return err(
      form.span,
      match got.reason {
        Some(reason) => format!("expected reason {wanted:?}, got {reason:?}"),
        None => format!("expected reason {wanted:?}, got none"),
      },
    );
  }
  Ok(())
}

// --- rule checks ---

/// `(check (rule ...) denies|asks|warns|passes "command" ["text"] ...)`:
/// one rule run by the engine against one Bash command, with the facts
/// stood in. The optional text must equal what the model or the user
/// would see.
fn check_rule(
  form: &Node,
  subject: &Node,
  verb: &Node,
  verb_name: &str,
  args: &[Node],
) -> Result<(), TypeError> {
  let expected = match verb_name {
    "denies" => Some(Kind::Deny),
    "asks" => Some(Kind::Ask),
    "warns" => Some(Kind::Warn),
    "passes" => None,
    other => return err(verb.span, format!("unknown rule verb `{other}`")),
  };
  let Some((command_node, args)) = args.split_first() else {
    return err(form.span, format!("`{verb_name}` needs a command string"));
  };
  let Sx::Str(command) = &command_node.kind else {
    return err(command_node.span, "expected a command string");
  };
  let (expected_text, args) = leading_string(args);
  let Setup {
    cwd,
    facts,
    asked: expected_asked,
    ..
  } = setup(args, false)?;

  let file =
    syntax::parse(std::slice::from_ref(subject), &facts).map_err(|mut errors| errors.remove(0))?;
  let declarations = Declarations::new();
  let ctx = Context::new(
    HookInput {
      session_id: "spec".into(),
      cwd: PathBuf::from(cwd).into(),
      tool_use_id: "spec".into(),
      agent_id: None,
      tool: Tool::Bash {
        command: command.clone(),
      },
    },
    &declarations,
  );
  let rules = Ruleset::assemble(Source::Builtin, declarations, facts, file.rules);
  let verdict = rules.evaluate(&ctx);
  let asked: Vec<(String, Vec<String>)> = rules
    .facts_asked()
    .into_iter()
    .map(|a| (a.name.to_string(), a.args))
    .collect();

  let (got, text) = match &verdict {
    None => (None, None),
    Some(verdict) => match &verdict.decision {
      Decision::Deny { reason } => (Some(Kind::Deny), Some(reason)),
      Decision::Ask { reason } => (Some(Kind::Ask), Some(reason)),
      Decision::Warn { context } => (Some(Kind::Warn), Some(context)),
    },
  };
  let name = |kind: Option<Kind>| match kind {
    None => "pass",
    Some(Kind::Deny) => "deny",
    Some(Kind::Ask) => "ask",
    Some(Kind::Warn) => "warn",
  };
  if got != expected {
    return err(
      form.span,
      match text {
        Some(text) => format!("expected {}, got {}: {text}", name(expected), name(got)),
        None => format!("expected {}, got {}", name(expected), name(got)),
      },
    );
  }
  if let Some(wanted) = expected_text
    && text.map(String::as_str) != Some(wanted)
  {
    return err(
      form.span,
      match text {
        Some(text) => format!("expected text {wanted:?}, got {text:?}"),
        None => format!("expected text {wanted:?}, got a pass"),
      },
    );
  }
  if let Some(wanted) = expected_asked
    && asked != wanted
  {
    return err(
      form.span,
      format!(
        "expected asked {}, got {}",
        show_asked(&wanted),
        show_asked(&asked)
      ),
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
  fn a_false_rule_claim_says_what_the_engine_decided() {
    const RULE: &str = "(rule r (deny [git stash] :reason \"no.\" :instead \"jj new.\"))";
    passes(&format!("(check {RULE} denies \"git stash\")"));
    passes(&format!(
      "(check {RULE} denies \"git stash\" \"claude-guard denied `git stash`: no. Instead: jj new.\")"
    ));
    assert_eq!(
      failures(&format!("(check {RULE} passes \"git stash\")")),
      ["t.scm:1:1: expected pass, got deny: claude-guard denied `git stash`: no. Instead: jj new."]
    );
    assert_eq!(
      failures(&format!("(check {RULE} denies \"git log\")")),
      ["t.scm:1:1: expected deny, got pass"]
    );
    assert_eq!(
      failures(&format!("(check {RULE} denies \"git stash\" \"other\")")),
      [
        "t.scm:1:1: expected text \"other\", got \"claude-guard denied `git stash`: no. Instead: jj new.\""
      ]
    );
    assert_eq!(
      failures(&format!("(check {RULE} passes \"git log\" \"text\")")),
      ["t.scm:1:1: expected text \"text\", got a pass"]
    );
    assert_eq!(
      failures(&format!("(check {RULE} eats \"git log\")")),
      ["t.scm:1:69: unknown rule verb `eats`"]
    );
    assert_eq!(
      failures(&format!("(check {RULE} denies)")),
      ["t.scm:1:1: `denies` needs a command string"]
    );
    assert_eq!(
      failures(&format!("(check {RULE} denies \"git stash\" :with ())")),
      ["t.scm:1:88: `:with` is for condition checks; a rule check binds from its own pattern"]
    );
    // The rule itself is type-checked, with the stubs in scope.
    assert_eq!(
      failures(
        "(check (rule r (deny [x] :when (nope?) :reason \"r.\" :instead \"i.\")) passes \"x\")"
      ),
      ["t.scm:1:33: unknown fact `nope?`"]
    );
    passes(
      "(check (rule r (deny [x] :when (nope?) :reason \"r.\" :instead \"i.\")) denies \"x\" :facts ((nope? holds)))",
    );
  }

  #[test]
  fn a_condition_check_defaults_to_no_bindings_an_empty_fs_and_a_cwd() {
    passes("(check (ancestor-has? \".jj\") fails)");
    passes("(check (under? \"x\" \"/spec\") holds)");
  }

  #[test]
  fn a_stub_can_answer_for_one_argument_list_only() {
    passes("(check (f? \"a\" \"b\") holds :facts ((f? holds :args (\"a\" \"b\"))))");
    passes(
      "(check (f? \"a\") fails \"asked with [\\\"a\\\"], not [\\\"a\\\", \\\"b\\\"]\" :facts ((f? holds :args (\"a\" \"b\"))))",
    );
    passes("(check (f?) unknown \"f? is unknown: slow\" :facts ((f? unknown \"slow\" :args ())))");
    assert_eq!(
      failures("(check (f?) holds :facts ((f? holds :args x)))"),
      ["t.scm:1:43: `:args` takes a list of strings"]
    );
    assert_eq!(
      failures("(check (f?) holds :facts ((f? holds :args (\"a\" b))))"),
      ["t.scm:1:48: `:args` takes a list of strings"]
    );
    assert_eq!(
      failures("(check (f?) holds :facts ((f? holds :arg (\"a\"))))"),
      ["t.scm:1:37: unexpected `:arg`; a stub ends with an optional :args (\"...\" ...)"]
    );
  }

  #[test]
  fn a_rule_check_can_say_what_the_evaluation_asked() {
    const RULE: &str =
      "(rule r :when (a?) (deny [x ?p] :when (and (b? ?p) (a?)) :reason \"r.\" :instead \"i.\"))";
    let stubs = ":facts ((a? holds) (b? holds))";
    passes(&format!(
      "(check {RULE} denies \"x 1\" :asked ((a?) (b? \"1\")) {stubs})"
    ));
    // A memoized answer is not asked twice; nothing asked is `()`.
    passes(&format!(
      "(check {RULE} passes \"y\" :asked ((a?)) {stubs})"
    ));
    passes("(check (rule r (deny [x] :reason \"r.\" :instead \"i.\")) denies \"x\" :asked ())");
    assert_eq!(
      failures(&format!(
        "(check {RULE} denies \"x 1\" :asked ((b? \"1\")) {stubs})"
      )),
      ["t.scm:1:1: expected asked (b? \"1\"), got (a?) (b? \"1\")"]
    );
    assert_eq!(
      failures(
        "(check (rule r (deny [x] :reason \"r.\" :instead \"i.\")) denies \"x\" :asked ((a?)))"
      ),
      ["t.scm:1:1: expected asked (a?), got nothing"]
    );
    assert_eq!(
      failures(
        "(check (rule r (deny [x] :reason \"r.\" :instead \"i.\")) denies \"x\" :asked (a?))"
      ),
      ["t.scm:1:74: `:asked` takes a list of (name \"arg\"...) forms"]
    );
    assert_eq!(
      failures("(check (a?) holds :facts ((a? holds)) :asked ((a?)))"),
      ["t.scm:1:39: `:asked` is for rule checks; only an evaluation asks facts"]
    );
  }

  #[test]
  fn facts_are_stubbed_by_name_with_an_answer_and_a_reason() {
    passes("(check (in-git? \"x\") holds :facts ((in-git? holds)))");
    passes("(check (slow?) unknown :facts ((slow? unknown \"timed out\")))");
    passes(
      "(check (slow?) unknown \"slow? is unknown: timed out\" :facts ((slow? unknown \"timed out\")))",
    );
    passes(
      "(check (and (ancestor-has? \"t\") (slow?)) unknown \"slow? is unknown: timed out\" :ancestors (\"t\") :facts ((slow? unknown \"timed out\")))",
    );
    passes("(check (a?) holds \"a held\" :facts ((a? holds \"a held\")))");
    assert_eq!(
      failures("(check (slow?) unknown \"timed out\" :facts ((slow? unknown \"crashed\")))"),
      ["t.scm:1:1: expected reason \"timed out\", got \"slow? is unknown: crashed\""]
    );
    assert_eq!(
      failures("(check (a?) holds \"a held\" :facts ((a? holds)))"),
      ["t.scm:1:1: expected reason \"a held\", got none"]
    );
    assert_eq!(
      failures("(check (slow?) unknown)"),
      ["t.scm:1:9: unknown fact `slow?`"]
    );
    assert_eq!(
      failures("(check (slow?) unknown :facts ((slow? unknown)))"),
      ["t.scm:1:39: an unknown stub needs a reason"]
    );
    assert_eq!(
      failures("(check (slow?) unknown :facts ((slow? maybe)))"),
      ["t.scm:1:39: unknown stub verb `maybe`"]
    );
    assert_eq!(
      failures("(check (slow?) unknown :facts (slow?))"),
      ["t.scm:1:32: `:facts` takes (name holds|fails|unknown [\"reason\"]) forms"]
    );
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
      ["t.scm:1:12: unknown elaboration verb `matches`"]
    );
    assert_eq!(
      failures("(check 5 matches \"a\")"),
      ["t.scm:1:8: expected a [pattern], a (condition), a (rule ...), or a \"command\""]
    );
    assert_eq!(
      failures(r#"(check "git -C ." elaborates (git))"#),
      [r#"t.scm:1:1: expected (git), got (git "-C" ".")"#]
    );
    assert_eq!(
      failures("(check \"a\" elaborates (a) (b))"),
      ["t.scm:1:1: `elaborates` takes one expected form"]
    );
    assert_eq!(
      failures("(check [a] matches \"a\" :commands)"),
      ["t.scm:1:24: `:commands` needs a list of (command ...) forms"]
    );
    assert_eq!(
      failures("(check [a] matches \"a\" :commands x)"),
      ["t.scm:1:34: `:commands` takes a list of (command ...) forms"]
    );
    assert_eq!(
      failures(
        "(check [a] matches \"a\" :commands ((rule r (deny [a] :reason \"r\" :instead \"i\"))))"
      ),
      ["t.scm:1:35: `:commands` takes (command ...) forms, not rules"]
    );
    assert_eq!(
      failures("(check [a] matches \"a\" :commands ((command a (option \"C\"))))"),
      ["t.scm:1:54: option names look like \"-c\" or \"--long\", not \"C\""]
    );
    assert_eq!(
      failures("(check [a] matches \"a\" :commands () extra)"),
      ["t.scm:1:37: unexpected `extra` after `:commands`"]
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
      failures("(check (ancestor-has? \"x\") holds x)"),
      ["t.scm:1:34: unexpected `x`; expected :with, :cwd, :ancestors, or :facts"]
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
