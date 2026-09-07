//! Facts: the propositions a condition is made of.
//!
//! A fact is named in a condition, `(ancestor-has? ".jj")`, and answers
//! true, false, or unknown for one call, with a reason when it has one.
//! Every fact the language can name, built in or declared by a file, is
//! one type implementing [`Fact`], registered by name in [`Facts`].
//! Conditions reach a fact only through the registry, so a test or the
//! spec stands a stub under any name and the engine cannot tell.
//!
//! Built in: `ancestor-has?` and `under?`, one file each under `facts/`.
//! `stubs` holds the stand-ins the spec and the tests use, and the spec
//! is not test-only code, so they are ordinary items. The extern runner
//! (D9) will be one more type here. ADR 0003 has the reasons.
//!
//! The registry is where memoization per call and the log's list of
//! facts used will live; neither exists yet.

mod ancestor_has;
mod stubs;
mod under;

use std::collections::BTreeMap;
use std::path::Path;

pub use ancestor_has::AncestorHas;
pub use stubs::{Ancestors, Stub};
pub use under::Under;

use crate::input::string_id;
use crate::pattern::{Bindings, Var};
use crate::sexp::Span;
use crate::syntax::TypeError;

string_id! {
  /// A fact's name as written in a condition: `ancestor-has?`.
  FactName
}

/// What a fact evaluates to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truth {
  True,
  False,
  Unknown,
}

impl From<bool> for Truth {
  fn from(b: bool) -> Truth {
    if b { Truth::True } else { Truth::False }
  }
}

/// A fact's answer: the truth and, when the fact has one, the reason.
/// The reason is evidence for the log and the deny text (D10), never a
/// verdict. An unknown always carries the reason it is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
  pub truth: Truth,
  pub reason: Option<String>,
}

impl Answer {
  pub fn holds() -> Answer {
    Answer {
      truth: Truth::True,
      reason: None,
    }
  }

  pub fn fails() -> Answer {
    Answer {
      truth: Truth::False,
      reason: None,
    }
  }

  pub fn unknown(reason: impl Into<String>) -> Answer {
    Answer {
      truth: Truth::Unknown,
      reason: Some(reason.into()),
    }
  }

  /// Test-only until a fact in the binary attaches a reason to a settled
  /// answer; the extern runner will.
  #[cfg(test)]
  pub fn with_reason(
    mut self,
    reason: impl Into<String>,
  ) -> Answer {
    self.reason = Some(reason.into());
    self
  }
}

impl From<bool> for Answer {
  fn from(b: bool) -> Answer {
    if b { Answer::holds() } else { Answer::fails() }
  }
}

/// A fact's argument as written: a binder's capture or a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
  Var(Var),
  Literal(String),
}

impl Arg {
  /// The argument's text under `bindings`; `None` for an unbound binder.
  pub fn resolve<'b>(
    &'b self,
    bindings: &'b Bindings,
  ) -> Option<&'b str> {
    match self {
      Arg::Literal(text) => Some(text),
      Arg::Var(var) => bindings.get(var).map(String::as_str),
    }
  }
}

/// What a fact may see besides its arguments. Grows with the facts: the
/// extern runner adds the session, the tool, and the term.
pub struct Call<'a> {
  pub cwd: &'a Path,
}

/// One fact the language can name.
pub trait Fact {
  /// Check one use at load time: arity and argument kinds. Each argument
  /// comes with its position; `span` is the whole form's, for errors
  /// about the use as a whole.
  fn check(
    &self,
    args: &[(Arg, Span)],
    span: Span,
  ) -> Result<(), TypeError>;

  /// Answer for one call, every argument resolved to text. `check` has
  /// passed, so the arity is right.
  fn ask(
    &self,
    args: &[&str],
    call: &Call<'_>,
  ) -> Answer;
}

/// Every fact a file may name. Built-ins come first; a later declaration
/// under the same name replaces the earlier one, which is how the spec
/// and the tests stand in for the disk.
pub struct Facts {
  by_name: BTreeMap<FactName, Box<dyn Fact>>,
}

impl Facts {
  /// No facts at all.
  pub fn empty() -> Facts {
    Facts {
      by_name: BTreeMap::new(),
    }
  }

  /// The facts the binary ships: `ancestor-has?` and `under?`.
  pub fn builtin() -> Facts {
    let mut facts = Facts::empty();
    facts.declare("ancestor-has?", AncestorHas);
    facts.declare("under?", Under);
    facts
  }

  /// Register `fact` under `name`, replacing any earlier one.
  pub fn declare(
    &mut self,
    name: impl Into<FactName>,
    fact: impl Fact + 'static,
  ) {
    self.by_name.insert(name.into(), Box::new(fact));
  }

  pub fn get(
    &self,
    name: &FactName,
  ) -> Option<&dyn Fact> {
    self.by_name.get(name).map(|fact| fact.as_ref())
  }

  /// Ask a fact by name. An unknown answer is prefixed with the fact's
  /// name, `in-jj-repo? is unknown: timed out`, so the evidence an ask
  /// carries says which fact could not be settled. The parser refuses a
  /// name the registry does not know, so that unknown is a guard, not a
  /// path a loaded file takes.
  pub fn ask(
    &self,
    name: &FactName,
    args: &[&str],
    call: &Call<'_>,
  ) -> Answer {
    let Some(fact) = self.get(name) else {
      return Answer::unknown(format!("no fact named `{name}`"));
    };
    let answer = fact.ask(args, call);
    match answer.truth {
      Truth::Unknown => Answer::unknown(format!(
        "{name} is unknown: {}",
        answer.reason.as_deref().unwrap_or("no reason given")
      )),
      Truth::True | Truth::False => answer,
    }
  }
}

pub(crate) fn err<T>(
  span: Span,
  message: impl Into<String>,
) -> Result<T, TypeError> {
  Err(TypeError {
    span,
    message: message.into(),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn at(
    line: u32,
    col: u32,
  ) -> Span {
    Span { line, col }
  }

  fn call<'a>(cwd: &'a Path) -> Call<'a> {
    Call { cwd }
  }

  #[test]
  fn the_builtin_registry_knows_two_facts_and_nothing_else() {
    let facts = Facts::builtin();
    assert!(facts.get(&FactName::from("ancestor-has?")).is_some());
    assert!(facts.get(&FactName::from("under?")).is_some());
    assert!(facts.get(&FactName::from("exists?")).is_none());
    assert_eq!(
      facts.ask(&FactName::from("exists?"), &[], &call(Path::new("/x"))),
      Answer::unknown("no fact named `exists?`")
    );
  }

  #[test]
  fn the_registry_names_the_fact_in_an_unknown() {
    let mut facts = Facts::builtin();
    facts.declare("slow?", Stub(Answer::unknown("timed out after 1s")));
    facts.declare(
      "mute?",
      Stub(Answer {
        truth: Truth::Unknown,
        reason: None,
      }),
    );
    facts.declare("yes?", Stub(Answer::holds().with_reason("as is")));
    let cwd = Path::new("/x");
    assert_eq!(
      facts.ask(&FactName::from("slow?"), &[], &call(cwd)),
      Answer::unknown("slow? is unknown: timed out after 1s")
    );
    assert_eq!(
      facts.ask(&FactName::from("mute?"), &[], &call(cwd)),
      Answer::unknown("mute? is unknown: no reason given")
    );
    assert_eq!(
      facts.ask(&FactName::from("yes?"), &[], &call(cwd)),
      Answer::holds().with_reason("as is")
    );
  }

  #[test]
  fn a_later_declaration_replaces_by_name() {
    let mut facts = Facts::builtin();
    facts.declare(
      "ancestor-has?",
      Stub(Answer::holds().with_reason("stubbed")),
    );
    assert_eq!(
      facts.ask(
        &FactName::from("ancestor-has?"),
        &["anything"],
        &call(Path::new("/nowhere"))
      ),
      Answer::holds().with_reason("stubbed")
    );
  }

  #[test]
  fn an_answer_is_built_from_a_bool_or_a_reason() {
    assert_eq!(Answer::from(true), Answer::holds());
    assert_eq!(Answer::from(false), Answer::fails());
    assert_eq!(Answer::unknown("why").truth, Truth::Unknown);
    assert_eq!(Answer::unknown("why").reason.as_deref(), Some("why"));
    assert_eq!(Truth::from(true), Truth::True);
    assert_eq!(Truth::from(false), Truth::False);
  }

  #[test]
  fn an_argument_resolves_a_binder_through_the_bindings() {
    let bindings = Bindings::from([(Var::from("p"), "/tmp/x".to_string())]);
    assert_eq!(Arg::Literal("lit".into()).resolve(&bindings), Some("lit"));
    assert_eq!(Arg::Var(Var::from("p")).resolve(&bindings), Some("/tmp/x"));
    assert_eq!(Arg::Var(Var::from("q")).resolve(&bindings), None);
  }

  #[test]
  fn ancestor_has_checks_for_one_string() {
    let fact = AncestorHas;
    assert!(
      fact
        .check(&[(Arg::Literal(".jj".into()), at(1, 16))], at(1, 1))
        .is_ok()
    );
    assert_eq!(
      fact.check(&[], at(1, 1)).unwrap_err().to_string(),
      "1:1: `(ancestor-has? ...)` takes one name"
    );
    assert_eq!(
      fact
        .check(
          &[
            (Arg::Literal("a".into()), at(1, 16)),
            (Arg::Literal("b".into()), at(1, 20))
          ],
          at(1, 1)
        )
        .unwrap_err()
        .to_string(),
      "1:1: `(ancestor-has? ...)` takes one name"
    );
    assert_eq!(
      fact
        .check(&[(Arg::Var(Var::from("p")), at(1, 16))], at(1, 1))
        .unwrap_err()
        .to_string(),
      "1:16: `(ancestor-has? ...)` takes a string, not a binder"
    );
  }

  #[test]
  fn ancestor_has_walks_up_from_cwd() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".jj")).unwrap();
    let deep = dir.path().join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(AncestorHas.ask(&[".jj"], &call(&deep)), Answer::holds());
    assert_eq!(
      AncestorHas.ask(&[".definitely-not-here"], &call(&deep)),
      Answer::fails()
    );
  }

  #[test]
  fn under_checks_for_a_path_and_a_string_prefix() {
    let fact = Under;
    let p = || (Arg::Var(Var::from("p")), at(1, 9));
    assert!(
      fact
        .check(&[p(), (Arg::Literal("/tmp".into()), at(1, 12))], at(1, 1))
        .is_ok()
    );
    assert_eq!(
      fact.check(&[p()], at(1, 1)).unwrap_err().to_string(),
      "1:1: `(under? ...)` takes a path and a prefix"
    );
    assert_eq!(
      fact
        .check(&[p(), (Arg::Var(Var::from("q")), at(1, 12))], at(1, 1))
        .unwrap_err()
        .to_string(),
      "1:12: `(under? ...)` takes a string prefix"
    );
  }

  #[test]
  fn under_compares_normalized_paths_against_cwd() {
    let cwd = Path::new("/Users/x/proj");
    let ask = |path: &str, prefix: &str| Under.ask(&[path, prefix], &call(cwd)).truth;
    assert_eq!(ask("/tmp/x", "/tmp"), Truth::True);
    assert_eq!(ask("/private/tmp/y", "/tmp"), Truth::True);
    assert_eq!(ask("/tmp", "/tmp"), Truth::True);
    assert_eq!(ask("/tmpfoo", "/tmp"), Truth::False);
    assert_eq!(ask("/var/tmp/x", "/tmp"), Truth::False);
    assert_eq!(ask("/tmp/x", "/private/tmp"), Truth::True);
    assert_eq!(ask("src/main.rs", "/Users/x/proj"), Truth::True);
    assert_eq!(ask("src/main.rs", "/Users/x/proj/src"), Truth::True);
    assert_eq!(ask("src/main.rs", "/tmp"), Truth::False);
    assert_eq!(ask("../other", "/Users/x/proj"), Truth::True);
  }

  #[test]
  fn the_stubs_answer_from_data_and_never_the_disk() {
    let ancestors = Ancestors {
      present: vec![".jj".into()],
      unknown: false,
    };
    let cwd = Path::new("/nowhere/at/all");
    assert_eq!(ancestors.ask(&[".jj"], &call(cwd)), Answer::holds());
    assert_eq!(ancestors.ask(&[".git"], &call(cwd)), Answer::fails());
    let unknown = Ancestors {
      present: vec![".jj".into()],
      unknown: true,
    };
    assert_eq!(
      unknown.ask(&[".jj"], &call(cwd)),
      Answer::unknown("stubbed as unknown")
    );
    // The list stub checks arguments like the real fact.
    assert!(ancestors.check(&[], at(1, 1)).is_err());
    // A plain stub takes any arguments.
    let stub = Stub(Answer::unknown("timed out"));
    assert!(stub.check(&[], at(1, 1)).is_ok());
    assert_eq!(
      stub.ask(&["a", "b"], &call(cwd)),
      Answer::unknown("timed out")
    );
  }
}
