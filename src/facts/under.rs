//! `(under? <path> "prefix")`: the path is the prefix or below it, by
//! path component. A relative path resolves against the call's working
//! directory first (D21), and `/private/tmp` counts as `/tmp`, as in the
//! matcher.

use std::path::Path;

use crate::facts::{Answer, Arg, Call, Fact, err};
use crate::pattern::normalize_path;
use crate::sexp::Span;
use crate::syntax::TypeError;

pub struct Under;

impl Fact for Under {
  fn check(
    &self,
    args: &[(Arg, Span)],
    span: Span,
  ) -> Result<(), TypeError> {
    match args {
      [_, (Arg::Literal(_), _)] => Ok(()),
      [_, (Arg::Var(_), at)] => err(*at, "`(under? ...)` takes a string prefix"),
      _ => err(span, "`(under? ...)` takes a path and a prefix"),
    }
  }

  fn ask(
    &self,
    args: &[&str],
    call: &Call<'_>,
  ) -> Answer {
    let [path, prefix] = args else {
      return Answer::unknown("under? takes a path and a prefix");
    };
    let full = normalize_path(&call.cwd.join(path));
    full.starts_with(normalize_path(Path::new(prefix))).into()
  }
}
