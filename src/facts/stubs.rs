//! Stand-ins for the spec and the tests. A stub is registered under any
//! name, so a check line can name a fact that does not exist and say
//! what it answers, and a test can put `ancestor-has?` on a list instead
//! of a disk. Neither touches the filesystem or spawns anything.

use crate::facts::{AncestorHas, Answer, Arg, Call, Fact};
use crate::sexp::Span;
use crate::syntax::TypeError;

/// One fixed answer, whatever the arguments.
pub struct Stub(pub Answer);

impl Fact for Stub {
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
    self.0.clone()
  }
}

/// `ancestor-has?` answered from a list: the entries some ancestor of
/// cwd has; everything else does not exist. `unknown` makes every answer
/// unknown, which is what a timed-out extern fact will do.
pub struct Ancestors {
  pub present: Vec<String>,
  pub unknown: bool,
}

impl Fact for Ancestors {
  fn check(
    &self,
    args: &[(Arg, Span)],
    span: Span,
  ) -> Result<(), TypeError> {
    AncestorHas.check(args, span)
  }

  fn ask(
    &self,
    args: &[&str],
    _call: &Call<'_>,
  ) -> Answer {
    if self.unknown {
      return Answer::unknown("stubbed as unknown");
    }
    let [name] = args else {
      return Answer::unknown("ancestor-has? takes one name");
    };
    self.present.iter().any(|p| p == name).into()
  }
}
