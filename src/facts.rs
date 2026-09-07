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
//! `exec` is a fact a file declares, answered by a program over stdio.
//! `stubs` holds the stand-ins the spec and the tests use, and the spec
//! is not test-only code, so they are ordinary items. ADR 0003 has the
//! reasons.
//!
//! The registry memoizes: one fact asked twice with the same arguments
//! in the same cwd answers once, so a program declared as a fact runs at
//! most once per hook call however many rows name it. It also keeps the
//! list of what was asked, in order, with each answer and how long it
//! took, which the log record carries (principle 12).

mod ancestor_has;
mod exec;
mod stubs;
mod under;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub use ancestor_has::AncestorHas;
pub use exec::Exec;
use serde::{Deserialize, Serialize};
pub use stubs::{Ancestors, ArgStub, Stub};
pub use under::Under;

use crate::input::{SessionId, ToolName, string_id};
use crate::pattern::{Bindings, Var};
use crate::sexp::Span;
use crate::syntax::TypeError;

string_id! {
  /// A fact's name as written in a condition: `ancestor-has?`.
  FactName
}

/// What a fact evaluates to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
  /// answer other than by parsing it; `exec` reads its reason from JSON.
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

/// How long a fact's answer is good for. `fresh` is the only lifetime in
/// this version: asked on every call. `session` and durations are D8's
/// later work. No default (ADR 0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
  Fresh,
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

/// What a fact may see besides its arguments: the call's cwd, session,
/// and tool, and the term, which is the log's subject for the call as
/// JSON. An `exec` fact gets all of it on stdin.
pub struct Call<'a> {
  pub cwd: &'a Path,
  pub session_id: SessionId,
  pub tool: ToolName,
  pub term: serde_json::Value,
}

impl<'a> Call<'a> {
  /// A call with no session behind it, for checks and tests.
  pub fn at(cwd: &'a Path) -> Call<'a> {
    Call {
      cwd,
      session_id: SessionId::from("none"),
      tool: ToolName::from("none"),
      term: serde_json::Value::Null,
    }
  }
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

/// What one answer was asked under: the fact, its arguments, and the
/// cwd, which every built-in reads.
type Key = (FactName, Vec<String>, PathBuf);

/// One fact asked during a call, as the log record keeps it: the fact,
/// its arguments, what it answered, and how long it took. A memoized
/// answer is not asked again and so appears once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
  pub name: FactName,
  pub args: Vec<String>,
  pub truth: Truth,
  /// The fact's own reason, without the registry's `is unknown` prefix.
  pub reason: Option<String>,
  pub ms: u64,
}

/// Every fact a file may name. Built-ins come first; a later declaration
/// under the same name replaces the earlier one, which is how the spec
/// and the tests stand in for the disk. Cloning gives a registry with
/// the same facts, an empty memo, and nothing asked.
pub struct Facts {
  by_name: BTreeMap<FactName, Arc<dyn Fact>>,
  memo: RefCell<HashMap<Key, Answer>>,
  asked: RefCell<Vec<Asked>>,
}

impl Clone for Facts {
  fn clone(&self) -> Facts {
    Facts {
      by_name: self.by_name.clone(),
      memo: RefCell::new(HashMap::new()),
      asked: RefCell::new(Vec::new()),
    }
  }
}

impl Facts {
  /// No facts at all.
  pub fn empty() -> Facts {
    Facts {
      by_name: BTreeMap::new(),
      memo: RefCell::new(HashMap::new()),
      asked: RefCell::new(Vec::new()),
    }
  }

  /// Everything asked since the last call, in order, and clear the list.
  /// The memo stays, so a fact asked again still answers from it.
  pub fn take_asked(&self) -> Vec<Asked> {
    std::mem::take(&mut *self.asked.borrow_mut())
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
    self.by_name.insert(name.into(), Arc::new(fact));
  }

  pub fn get(
    &self,
    name: &FactName,
  ) -> Option<&dyn Fact> {
    self.by_name.get(name).map(|fact| fact.as_ref())
  }

  /// Every name the registry knows, in order.
  pub fn names(&self) -> impl Iterator<Item = &FactName> {
    self.by_name.keys()
  }

  /// Ask a fact by name, once per fact, arguments, and cwd. An unknown
  /// answer is prefixed with the fact's name, `in-jj-repo? is unknown:
  /// timed out`, so the evidence an ask carries says which fact could
  /// not be settled. The parser refuses a name the registry does not
  /// know, so that unknown is a guard, not a path a loaded file takes.
  pub fn ask(
    &self,
    name: &FactName,
    args: &[&str],
    call: &Call<'_>,
  ) -> Answer {
    let key: Key = (
      name.clone(),
      args.iter().map(|a| a.to_string()).collect(),
      call.cwd.to_path_buf(),
    );
    if let Some(answer) = self.memo.borrow().get(&key) {
      return answer.clone();
    }
    let Some(fact) = self.get(name) else {
      return Answer::unknown(format!("no fact named `{name}`"));
    };
    let started = Instant::now();
    let answer = fact.ask(args, call);
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    self.asked.borrow_mut().push(Asked {
      name: name.clone(),
      args: key.1.clone(),
      truth: answer.truth,
      reason: answer.reason.clone(),
      ms,
    });
    let answer = match answer.truth {
      Truth::Unknown => Answer::unknown(format!(
        "{name} is unknown: {}",
        answer.reason.as_deref().unwrap_or("no reason given")
      )),
      Truth::True | Truth::False => answer,
    };
    self.memo.borrow_mut().insert(key, answer.clone());
    answer
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
  use std::cell::Cell;
  use std::rc::Rc;

  use super::*;

  fn at(
    line: u32,
    col: u32,
  ) -> Span {
    Span { line, col }
  }

  fn call<'a>(cwd: &'a Path) -> Call<'a> {
    Call::at(cwd)
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

  /// Counts how often it is asked.
  struct Counting(Rc<Cell<u32>>);

  impl Fact for Counting {
    fn check(
      &self,
      _args: &[(Arg, Span)],
      _span: Span,
    ) -> Result<(), TypeError> {
      Ok(())
    }

    fn ask(
      &self,
      _args: &[&str],
      _call: &Call<'_>,
    ) -> Answer {
      self.0.set(self.0.get() + 1);
      Answer::holds()
    }
  }

  #[test]
  fn an_answer_is_memoized_by_fact_arguments_and_cwd() {
    let count = Rc::new(Cell::new(0));
    let mut facts = Facts::builtin();
    facts.declare("n?", Counting(count.clone()));
    let name = FactName::from("n?");
    let here = call(Path::new("/here"));
    facts.ask(&name, &["a"], &here);
    facts.ask(&name, &["a"], &here);
    assert_eq!(count.get(), 1);
    facts.ask(&name, &["b"], &here);
    assert_eq!(count.get(), 2);
    facts.ask(&name, &["a"], &call(Path::new("/there")));
    assert_eq!(count.get(), 3);
    // A clone starts with an empty memo and the same facts.
    facts.clone().ask(&name, &["a"], &here);
    assert_eq!(count.get(), 4);
  }

  #[test]
  fn what_was_asked_is_kept_in_order_with_answers_and_timings() {
    let mut facts = Facts::builtin();
    facts.declare("slow?", Stub(Answer::unknown("timed out")));
    facts.declare("yes?", Stub(Answer::holds().with_reason("as is")));
    let cwd = Path::new("/x");
    facts.ask(&FactName::from("yes?"), &["a", "b"], &call(cwd));
    facts.ask(&FactName::from("slow?"), &[], &call(cwd));
    facts.ask(&FactName::from("under?"), &["/x/y", "/x"], &call(cwd));
    // A memoized answer is not asked again.
    facts.ask(&FactName::from("yes?"), &["a", "b"], &call(cwd));
    let asked = facts.take_asked();
    let seen: Vec<(String, Vec<String>, Truth, Option<String>)> = asked
      .iter()
      .map(|a| {
        (
          a.name.to_string(),
          a.args.clone(),
          a.truth,
          a.reason.clone(),
        )
      })
      .collect();
    assert_eq!(
      seen,
      [
        (
          "yes?".to_string(),
          vec!["a".to_string(), "b".to_string()],
          Truth::True,
          Some("as is".to_string())
        ),
        (
          "slow?".to_string(),
          vec![],
          Truth::Unknown,
          Some("timed out".to_string())
        ),
        (
          "under?".to_string(),
          vec!["/x/y".to_string(), "/x".to_string()],
          Truth::True,
          None
        ),
      ]
    );
    assert!(asked.iter().all(|a| a.ms < 1000));
    // Taking clears the list and leaves the memo.
    assert!(facts.take_asked().is_empty());
    facts.ask(&FactName::from("yes?"), &["a", "b"], &call(cwd));
    assert!(facts.take_asked().is_empty());
    // The record's shape.
    assert_eq!(
      serde_json::to_string(&asked[1]).unwrap(),
      r#"{"name":"slow?","args":[],"truth":"unknown","reason":"timed out","ms":0}"#
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
