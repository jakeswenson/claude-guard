//! `(ancestor-has? "name")`: the call's working directory, or a directory
//! above it, contains an entry named `name`. `(ancestor-has? ".jj")` is
//! how a rule says "in a jj repo".

use crate::facts::{Answer, Arg, Call, Fact, err};
use crate::sexp::Span;
use crate::syntax::TypeError;

pub struct AncestorHas;

impl Fact for AncestorHas {
  fn check(
    &self,
    args: &[(Arg, Span)],
    span: Span,
  ) -> Result<(), TypeError> {
    match args {
      [(Arg::Literal(_), _)] => Ok(()),
      [(Arg::Var(_), at)] => err(*at, "`(ancestor-has? ...)` takes a string, not a binder"),
      _ => err(span, "`(ancestor-has? ...)` takes one name"),
    }
  }

  fn ask(
    &self,
    args: &[&str],
    call: &Call<'_>,
  ) -> Answer {
    let [name] = args else {
      return Answer::unknown("ancestor-has? takes one name");
    };
    call
      .cwd
      .ancestors()
      .any(|dir| dir.join(name).exists())
      .into()
  }
}
