//! Conditions: the predicate language a row or rule is guarded by.
//!
//! ```text
//! cond := (ancestor-has? "<name>")     ; some ancestor of cwd contains <name>
//!       | (under? <arg> "<prefix>")     ; <arg>'s path is <prefix> or below it
//!       | (and cond+) | (or cond+) | (not cond)
//! arg  := ?name | "<path>"
//! ```
//!
//! A condition is parsed against a scope, the binders its pattern declares,
//! so an unbound `?name` is a load error and never a runtime one. A rule's
//! `:when` has an empty scope: it runs before any pattern matches.
//!
//! Evaluation is three-valued. Nothing here produces `Unknown` yet; the
//! seam is [`Fs`], which extern predicates and cwd tracking will share.
//! `and`, `or`, and `not` follow Kleene's tables, so an unknown never
//! becomes a false yes.
//!
//! `under?` resolves a relative path against the call's cwd before
//! comparing, and `/private/tmp` counts as `/tmp`, as in the matcher.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::pattern::{self, Bindings, Var};
use crate::sexp::{Kind as Sx, Node, Span};
use crate::syntax::TypeError;

/// A checked condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
  AncestorHas(String),
  Under(Arg, PathBuf),
  And(Vec<Cond>),
  Or(Vec<Cond>),
  Not(Box<Cond>),
}

/// A path-valued argument: a binder's capture or a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
  Var(Var),
  Literal(String),
}

/// Where a condition sits, and so which binders it may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
  /// A rule's `:when`: no pattern has matched yet, so no binders.
  Rule,
  /// A row's `:when`: the binders its subject declares.
  Row(BTreeSet<Var>),
}

/// What a condition evaluates to.
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

/// The filesystem questions conditions ask, behind a trait so tests need
/// no disk and so a later extern can answer `Unknown`.
pub trait Fs {
  /// Does `cwd` or any ancestor of it contain an entry named `name`?
  fn ancestor_has(
    &self,
    cwd: &Path,
    name: &str,
  ) -> Truth;
}

/// The real filesystem.
pub struct RealFs;

impl Fs for RealFs {
  fn ancestor_has(
    &self,
    cwd: &Path,
    name: &str,
  ) -> Truth {
    cwd.ancestors().any(|dir| dir.join(name).exists()).into()
  }
}

/// What a condition sees besides its bindings.
pub struct Env<'a> {
  pub cwd: &'a Path,
  pub fs: &'a dyn Fs,
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

const EXPECTED: &str = "expected a condition such as (ancestor-has? \".jj\")";

/// Check `node` as a condition over `scope`.
pub fn parse(
  node: &Node,
  scope: &Scope,
) -> Result<Cond, TypeError> {
  let Sx::List(items) = &node.kind else {
    return err(node.span, EXPECTED);
  };
  let Some((head, args)) = items.split_first() else {
    return err(node.span, EXPECTED);
  };
  let Sx::Symbol(name) = &head.kind else {
    return err(head.span, EXPECTED);
  };
  match name.as_str() {
    "ancestor-has?" => {
      let [arg] = args else {
        return err(node.span, "`(ancestor-has? ...)` takes one name");
      };
      let Sx::Str(entry) = &arg.kind else {
        return err(arg.span, "`(ancestor-has? ...)` takes a string");
      };
      Ok(Cond::AncestorHas(entry.clone()))
    }
    "under?" => {
      let [arg, prefix] = args else {
        return err(node.span, "`(under? ...)` takes a path and a prefix");
      };
      let Sx::Str(prefix_text) = &prefix.kind else {
        return err(prefix.span, "`(under? ...)` takes a string prefix");
      };
      Ok(Cond::Under(
        parse_arg(arg, scope)?,
        PathBuf::from(prefix_text),
      ))
    }
    "and" | "or" => {
      if args.is_empty() {
        return err(
          node.span,
          format!("`({name} ...)` needs at least one condition"),
        );
      }
      let parts = args
        .iter()
        .map(|arg| parse(arg, scope))
        .collect::<Result<Vec<_>, _>>()?;
      Ok(if name == "and" {
        Cond::And(parts)
      } else {
        Cond::Or(parts)
      })
    }
    "not" => {
      let [arg] = args else {
        return err(node.span, "`(not ...)` takes one condition");
      };
      Ok(Cond::Not(Box::new(parse(arg, scope)?)))
    }
    other => err(head.span, format!("unknown predicate `{other}`")),
  }
}

fn parse_arg(
  node: &Node,
  scope: &Scope,
) -> Result<Arg, TypeError> {
  match &node.kind {
    Sx::Str(text) => Ok(Arg::Literal(text.clone())),
    Sx::Symbol(text) if text.starts_with('?') => {
      let var = match text.strip_prefix('?') {
        Some("") | None => return err(node.span, "binder has no name"),
        Some(name) => Var::from(name),
      };
      match scope {
        Scope::Rule => err(
          node.span,
          format!("`{text}` is not bound: a rule `:when` runs before any pattern matches"),
        ),
        Scope::Row(binders) if !binders.contains(&var) => {
          err(node.span, format!("`{text}` is not bound by this pattern"))
        }
        Scope::Row(_) => Ok(Arg::Var(var)),
      }
    }
    _ => err(node.span, "expected a `?binder` or a string"),
  }
}

impl Cond {
  pub fn eval(
    &self,
    env: &Env<'_>,
    bindings: &Bindings,
  ) -> Truth {
    match self {
      Cond::AncestorHas(name) => env.fs.ancestor_has(env.cwd, name),
      Cond::Under(arg, prefix) => match arg.resolve(bindings) {
        Some(path) => {
          let full = pattern::normalize_path(&env.cwd.join(path));
          full.starts_with(pattern::normalize_path(prefix)).into()
        }
        None => Truth::Unknown,
      },
      Cond::And(parts) => {
        let mut result = Truth::True;
        for part in parts {
          match part.eval(env, bindings) {
            Truth::False => return Truth::False,
            Truth::Unknown => result = Truth::Unknown,
            Truth::True => {}
          }
        }
        result
      }
      Cond::Or(parts) => {
        let mut result = Truth::False;
        for part in parts {
          match part.eval(env, bindings) {
            Truth::True => return Truth::True,
            Truth::Unknown => result = Truth::Unknown,
            Truth::False => {}
          }
        }
        result
      }
      Cond::Not(inner) => match inner.eval(env, bindings) {
        Truth::True => Truth::False,
        Truth::False => Truth::True,
        Truth::Unknown => Truth::Unknown,
      },
    }
  }
}

impl Arg {
  fn resolve<'b>(
    &'b self,
    bindings: &'b Bindings,
  ) -> Option<&'b str> {
    match self {
      Arg::Literal(text) => Some(text),
      Arg::Var(var) => bindings.get(var).map(String::as_str),
    }
  }
}

/// The first binding set under which `cond` holds. With no condition,
/// the first binding set. `None` when no candidate satisfies it, which
/// includes every candidate coming back `Unknown`.
pub fn choose(
  cond: Option<&Cond>,
  candidates: Vec<Bindings>,
  env: &Env<'_>,
) -> Option<Bindings> {
  candidates.into_iter().find(|bindings| match cond {
    None => true,
    Some(cond) => cond.eval(env, bindings) == Truth::True,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::sexp;

  /// Answers `ancestor-has?` from a fixed list; anything else is unknown
  /// when `unknown` is set, false otherwise.
  struct FakeFs {
    present: Vec<&'static str>,
    unknown: bool,
  }

  impl Fs for FakeFs {
    fn ancestor_has(
      &self,
      _cwd: &Path,
      name: &str,
    ) -> Truth {
      if self.present.contains(&name) {
        Truth::True
      } else if self.unknown {
        Truth::Unknown
      } else {
        Truth::False
      }
    }
  }

  fn scope(names: &[&str]) -> Scope {
    Scope::Row(names.iter().map(|n| Var::from(*n)).collect())
  }

  fn parse_in(
    source: &str,
    names: &[&str],
  ) -> Result<Cond, TypeError> {
    parse(&sexp::read_one(source).unwrap(), &scope(names))
  }

  fn rule_level_error(source: &str) -> String {
    match parse(&sexp::read_one(source).unwrap(), &Scope::Rule) {
      Ok(c) => panic!("{source} parsed: {c:?}"),
      Err(e) => e.to_string(),
    }
  }

  fn cond(source: &str) -> Cond {
    parse_in(source, &["p", "q"]).unwrap_or_else(|e| panic!("{source}: {e}"))
  }

  fn error(
    source: &str,
    names: &[&str],
  ) -> String {
    match parse_in(source, names) {
      Ok(c) => panic!("{source} parsed: {c:?}"),
      Err(e) => e.to_string(),
    }
  }

  fn bound(pairs: &[(&str, &str)]) -> Bindings {
    pairs
      .iter()
      .map(|(k, v)| (Var::from(*k), v.to_string()))
      .collect()
  }

  fn eval_with(
    source: &str,
    fs: &FakeFs,
    bindings: &Bindings,
  ) -> Truth {
    let env = Env {
      cwd: Path::new("/Users/x/proj"),
      fs,
    };
    cond(source).eval(&env, bindings)
  }

  fn eval(source: &str) -> Truth {
    let fs = FakeFs {
      present: vec![".jj"],
      unknown: false,
    };
    eval_with(
      source,
      &fs,
      &bound(&[("p", "/tmp/x"), ("q", "src/main.rs")]),
    )
  }

  // --- parsing ---

  #[test]
  fn every_form_parses() {
    assert_eq!(
      cond("(ancestor-has? \".jj\")"),
      Cond::AncestorHas(".jj".into())
    );
    assert_eq!(
      cond("(under? ?p \"/tmp\")"),
      Cond::Under(Arg::Var(Var::from("p")), PathBuf::from("/tmp"))
    );
    assert_eq!(
      cond("(under? \"/tmp/x\" \"/tmp\")"),
      Cond::Under(Arg::Literal("/tmp/x".into()), PathBuf::from("/tmp"))
    );
    assert_eq!(
      cond("(and (ancestor-has? \".jj\") (not (or (under? ?p \"/tmp\"))))"),
      Cond::And(vec![
        Cond::AncestorHas(".jj".into()),
        Cond::Not(Box::new(Cond::Or(vec![Cond::Under(
          Arg::Var(Var::from("p")),
          PathBuf::from("/tmp")
        )]))),
      ])
    );
  }

  #[test]
  fn a_condition_is_a_list_headed_by_a_predicate() {
    let expected = "expected a condition such as (ancestor-has? \".jj\")";
    assert_eq!(error("foo", &[]), format!("1:1: {expected}"));
    assert_eq!(error("\"foo\"", &[]), format!("1:1: {expected}"));
    assert_eq!(error("()", &[]), format!("1:1: {expected}"));
    assert_eq!(error("(\"x\" 1)", &[]), format!("1:2: {expected}"));
    assert_eq!(
      error("(exists? \"x\")", &[]),
      "1:2: unknown predicate `exists?`"
    );
  }

  #[test]
  fn arities_and_argument_types_are_checked() {
    assert_eq!(
      error("(ancestor-has?)", &[]),
      "1:1: `(ancestor-has? ...)` takes one name"
    );
    assert_eq!(
      error("(ancestor-has? \"a\" \"b\")", &[]),
      "1:1: `(ancestor-has? ...)` takes one name"
    );
    assert_eq!(
      error("(ancestor-has? jj)", &[]),
      "1:16: `(ancestor-has? ...)` takes a string"
    );
    assert_eq!(
      error("(under? ?p)", &["p"]),
      "1:1: `(under? ...)` takes a path and a prefix"
    );
    assert_eq!(
      error("(under? ?p /tmp)", &["p"]),
      "1:12: `(under? ...)` takes a string prefix"
    );
    assert_eq!(
      error("(under? p \"/tmp\")", &["p"]),
      "1:9: expected a `?binder` or a string"
    );
    assert_eq!(
      error("(under? ? \"/tmp\")", &["p"]),
      "1:9: binder has no name"
    );
    assert_eq!(
      error("(and)", &[]),
      "1:1: `(and ...)` needs at least one condition"
    );
    assert_eq!(
      error("(or)", &[]),
      "1:1: `(or ...)` needs at least one condition"
    );
    assert_eq!(error("(not)", &[]), "1:1: `(not ...)` takes one condition");
    assert_eq!(
      error(
        "(not (ancestor-has? \".jj\") (ancestor-has? \".git\"))",
        &[]
      ),
      "1:1: `(not ...)` takes one condition"
    );
  }

  #[test]
  fn errors_inside_nested_conditions_point_at_the_inner_node() {
    assert_eq!(
      error("(and (ancestor-has? \".jj\") (nope))", &[]),
      "1:29: unknown predicate `nope`"
    );
  }

  #[test]
  fn a_binder_must_be_in_scope() {
    assert_eq!(
      error("(under? ?dst \"/tmp\")", &["src"]),
      "1:9: `?dst` is not bound by this pattern"
    );
    assert_eq!(
      error("(under? ?dst \"/tmp\")", &[]),
      "1:9: `?dst` is not bound by this pattern"
    );
    assert_eq!(
      rule_level_error("(under? ?dst \"/tmp\")"),
      "1:9: `?dst` is not bound: a rule `:when` runs before any pattern matches"
    );
  }

  // --- evaluation ---

  #[test]
  fn ancestor_has_asks_the_filesystem() {
    assert_eq!(eval("(ancestor-has? \".jj\")"), Truth::True);
    assert_eq!(eval("(ancestor-has? \".git\")"), Truth::False);
  }

  #[test]
  fn the_real_fs_walks_up_from_cwd() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".jj")).unwrap();
    let deep = dir.path().join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(RealFs.ancestor_has(&deep, ".jj"), Truth::True);
    assert_eq!(
      RealFs.ancestor_has(&deep, ".definitely-not-here"),
      Truth::False
    );
  }

  #[test]
  fn under_compares_normalized_paths() {
    assert_eq!(eval("(under? ?p \"/tmp\")"), Truth::True);
    assert_eq!(eval("(under? \"/private/tmp/y\" \"/tmp\")"), Truth::True);
    assert_eq!(eval("(under? \"/tmp\" \"/tmp\")"), Truth::True);
    assert_eq!(eval("(under? \"/tmpfoo\" \"/tmp\")"), Truth::False);
    assert_eq!(eval("(under? \"/var/tmp/x\" \"/tmp\")"), Truth::False);
    assert_eq!(eval("(under? ?p \"/private/tmp\")"), Truth::True);
  }

  #[test]
  fn under_resolves_a_relative_path_against_cwd() {
    assert_eq!(eval("(under? ?q \"/Users/x/proj\")"), Truth::True);
    assert_eq!(eval("(under? ?q \"/Users/x/proj/src\")"), Truth::True);
    assert_eq!(eval("(under? ?q \"/tmp\")"), Truth::False);
    assert_eq!(eval("(under? \"../other\" \"/Users/x/proj\")"), Truth::True);
  }

  #[test]
  fn a_missing_binding_is_unknown() {
    let fs = FakeFs {
      present: vec![],
      unknown: false,
    };
    assert_eq!(
      eval_with("(under? ?p \"/tmp\")", &fs, &Bindings::new()),
      Truth::Unknown
    );
  }

  #[test]
  fn and_or_not_follow_kleene() {
    let fs = FakeFs {
      present: vec!["t"],
      unknown: true,
    };
    let b = Bindings::new();
    let t = "(ancestor-has? \"t\")";
    let f = "(under? \"/a\" \"/b\")";
    let u = "(ancestor-has? \"u\")";
    let go = |src: &str| eval_with(src, &fs, &b);

    assert_eq!(go(&format!("(and {t} {t})")), Truth::True);
    assert_eq!(go(&format!("(and {t} {f})")), Truth::False);
    assert_eq!(go(&format!("(and {t} {u})")), Truth::Unknown);
    assert_eq!(go(&format!("(and {u} {f})")), Truth::False);

    assert_eq!(go(&format!("(or {f} {f})")), Truth::False);
    assert_eq!(go(&format!("(or {f} {t})")), Truth::True);
    assert_eq!(go(&format!("(or {f} {u})")), Truth::Unknown);
    assert_eq!(go(&format!("(or {u} {t})")), Truth::True);

    assert_eq!(go(&format!("(not {t})")), Truth::False);
    assert_eq!(go(&format!("(not {f})")), Truth::True);
    assert_eq!(go(&format!("(not {u})")), Truth::Unknown);
  }

  // --- choosing a binding set ---

  #[test]
  fn choose_takes_the_first_binding_set_that_holds() {
    let fs = FakeFs {
      present: vec![],
      unknown: false,
    };
    let env = Env {
      cwd: Path::new("/x"),
      fs: &fs,
    };
    let candidates = vec![
      bound(&[("p", "/var/a")]),
      bound(&[("p", "/tmp/a")]),
      bound(&[("p", "/tmp/b")]),
    ];
    let under_tmp = cond("(under? ?p \"/tmp\")");
    assert_eq!(
      choose(Some(&under_tmp), candidates.clone(), &env),
      Some(bound(&[("p", "/tmp/a")]))
    );
    assert_eq!(
      choose(None, candidates.clone(), &env),
      Some(bound(&[("p", "/var/a")]))
    );
    let under_etc = cond("(under? ?p \"/etc\")");
    assert_eq!(choose(Some(&under_etc), candidates, &env), None);
    assert_eq!(choose(None, vec![], &env), None);
  }

  #[test]
  fn choose_skips_unknown_candidates() {
    let fs = FakeFs {
      present: vec![],
      unknown: true,
    };
    let env = Env {
      cwd: Path::new("/x"),
      fs: &fs,
    };
    let unknown = cond("(ancestor-has? \"u\")");
    assert_eq!(choose(Some(&unknown), vec![Bindings::new()], &env), None);
  }
}
