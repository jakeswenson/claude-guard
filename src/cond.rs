//! Conditions: what a row or rule is guarded by.
//!
//! ```text
//! cond := (<fact> arg*)                  ; a fact the registry knows
//!       | (and cond+) | (or cond+) | (not cond)
//! arg  := ?name | "text"
//! ```
//!
//! The facts are in [`crate::facts`]; a condition names one and the fact
//! checks its own arguments at load time. A condition is parsed against
//! a scope, the binders its pattern declares, so an unbound `?name` is a
//! load error and never a runtime one. A rule's `:when` has an empty
//! scope: it runs before any pattern matches.
//!
//! Evaluation is three-valued. `and`, `or`, and `not` follow Kleene's
//! tables, so an unknown never becomes a false yes. An answer carries a
//! reason: a fact's own, the first unknown part's for an unknown `and`
//! or `or`, the deciding part's when one false settles an `and` or one
//! true settles an `or`, and otherwise every part's reasons joined.

use std::collections::BTreeSet;

use crate::facts::{Answer, Arg, Call, FactName, Facts, Truth};
use crate::pattern::{Bindings, Var};
use crate::sexp::{Kind as Sx, Node, Span};
use crate::syntax::TypeError;

/// A checked condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
  Fact { name: FactName, args: Vec<Arg> },
  And(Vec<Cond>),
  Or(Vec<Cond>),
  Not(Box<Cond>),
}

/// Where a condition sits, and so which binders it may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
  /// A rule's `:when`: no pattern has matched yet, so no binders.
  Rule,
  /// A row's `:when`: the binders its subject declares.
  Row(BTreeSet<Var>),
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

/// Check `node` as a condition over `scope`, naming only facts in
/// `facts`.
pub fn parse(
  node: &Node,
  scope: &Scope,
  facts: &Facts,
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
    "and" | "or" => {
      if args.is_empty() {
        return err(
          node.span,
          format!("`({name} ...)` needs at least one condition"),
        );
      }
      let parts = args
        .iter()
        .map(|arg| parse(arg, scope, facts))
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
      Ok(Cond::Not(Box::new(parse(arg, scope, facts)?)))
    }
    other => {
      let fact_name = FactName::from(other);
      let Some(fact) = facts.get(&fact_name) else {
        return err(head.span, format!("unknown fact `{other}`"));
      };
      let args = args
        .iter()
        .map(|arg| parse_arg(arg, scope).map(|parsed| (parsed, arg.span)))
        .collect::<Result<Vec<_>, _>>()?;
      fact.check(&args, node.span)?;
      Ok(Cond::Fact {
        name: fact_name,
        args: args.into_iter().map(|(arg, _)| arg).collect(),
      })
    }
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
    facts: &Facts,
    call: &Call<'_>,
    bindings: &Bindings,
  ) -> Answer {
    match self {
      Cond::Fact { name, args } => {
        let mut resolved = Vec::with_capacity(args.len());
        for arg in args {
          match arg.resolve(bindings) {
            Some(text) => resolved.push(text),
            None => {
              let Arg::Var(var) = arg else {
                unreachable!("a literal always resolves");
              };
              return Answer::unknown(format!("`?{var}` is not bound"));
            }
          }
        }
        facts.ask(name, &resolved, call)
      }
      Cond::And(parts) => {
        let mut unknown = None;
        let mut reasons = Vec::new();
        for part in parts {
          let answer = part.eval(facts, call, bindings);
          match answer.truth {
            Truth::False => return answer,
            Truth::Unknown => {
              if unknown.is_none() {
                unknown = Some(answer);
              }
            }
            Truth::True => reasons.extend(answer.reason),
          }
        }
        unknown.unwrap_or_else(|| settled(Truth::True, reasons))
      }
      Cond::Or(parts) => {
        let mut unknown = None;
        let mut reasons = Vec::new();
        for part in parts {
          let answer = part.eval(facts, call, bindings);
          match answer.truth {
            Truth::True => return answer,
            Truth::Unknown => {
              if unknown.is_none() {
                unknown = Some(answer);
              }
            }
            Truth::False => reasons.extend(answer.reason),
          }
        }
        unknown.unwrap_or_else(|| settled(Truth::False, reasons))
      }
      Cond::Not(inner) => {
        let answer = inner.eval(facts, call, bindings);
        Answer {
          truth: match answer.truth {
            Truth::True => Truth::False,
            Truth::False => Truth::True,
            Truth::Unknown => Truth::Unknown,
          },
          reason: answer.reason,
        }
      }
    }
  }
}

/// A settled `and` or `or`: every part agreed, and their reasons, if
/// any, travel together.
fn settled(
  truth: Truth,
  reasons: Vec<String>,
) -> Answer {
  Answer {
    truth,
    reason: if reasons.is_empty() {
      None
    } else {
      Some(reasons.join("; "))
    },
  }
}

/// What [`choose`] found among a pattern's binding sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
  /// A binding set under which the condition holds, with the reasons
  /// the facts gave, which the deny text carries as evidence (D10).
  Holds {
    bindings: Bindings,
    evidence: Option<String>,
  },
  /// No set holds, and this one came back unknown, with the reason. The
  /// engine turns this into an ask (D14).
  Unknown { bindings: Bindings, reason: String },
  /// No set holds and none is unknown, or there were no sets at all.
  NoMatch,
}

/// The first binding set under which `cond` holds; with no condition,
/// the first binding set. When none holds, the first set that came back
/// unknown, so a matched pattern with an unsettled condition is told
/// apart from a pattern that did not match.
pub fn choose(
  cond: Option<&Cond>,
  candidates: Vec<Bindings>,
  facts: &Facts,
  call: &Call<'_>,
) -> Choice {
  let Some(cond) = cond else {
    return match candidates.into_iter().next() {
      Some(bindings) => Choice::Holds {
        bindings,
        evidence: None,
      },
      None => Choice::NoMatch,
    };
  };
  let mut unknown = None;
  for bindings in candidates {
    let answer = cond.eval(facts, call, &bindings);
    match answer.truth {
      Truth::True => {
        return Choice::Holds {
          bindings,
          evidence: answer.reason,
        };
      }
      Truth::Unknown if unknown.is_none() => {
        unknown = Some(Choice::Unknown {
          bindings,
          reason: answer.reason.unwrap_or_else(|| "unknown".into()),
        });
      }
      Truth::Unknown | Truth::False => {}
    }
  }
  unknown.unwrap_or(Choice::NoMatch)
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use super::*;
  use crate::facts::{Ancestors, Stub};
  use crate::sexp;

  /// The built-ins with `ancestor-has?` answering from `present`, or
  /// answering unknown when `unknown` is set.
  fn facts_with(
    present: &[&str],
    unknown: bool,
  ) -> Facts {
    let mut facts = Facts::builtin();
    facts.declare(
      "ancestor-has?",
      Ancestors {
        present: present.iter().map(|s| s.to_string()).collect(),
        unknown,
      },
    );
    facts
  }

  fn scope(names: &[&str]) -> Scope {
    Scope::Row(names.iter().map(|n| Var::from(*n)).collect())
  }

  fn parse_in(
    source: &str,
    names: &[&str],
  ) -> Result<Cond, TypeError> {
    parse(
      &sexp::read_one(source).unwrap(),
      &scope(names),
      &Facts::builtin(),
    )
  }

  fn rule_level_error(source: &str) -> String {
    match parse(
      &sexp::read_one(source).unwrap(),
      &Scope::Rule,
      &Facts::builtin(),
    ) {
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

  fn fact(
    name: &str,
    args: &[Arg],
  ) -> Cond {
    Cond::Fact {
      name: FactName::from(name),
      args: args.to_vec(),
    }
  }

  fn lit(text: &str) -> Arg {
    Arg::Literal(text.into())
  }

  fn var(name: &str) -> Arg {
    Arg::Var(Var::from(name))
  }

  /// Parse `source` against `facts`, so stubs are known names, and
  /// evaluate it under `bindings`.
  fn eval_with(
    source: &str,
    facts: &Facts,
    bindings: &Bindings,
  ) -> Answer {
    let call = Call::at(Path::new("/Users/x/proj"));
    parse(&sexp::read_one(source).unwrap(), &scope(&["p", "q"]), facts)
      .unwrap_or_else(|e| panic!("{source}: {e}"))
      .eval(facts, &call, bindings)
  }

  fn eval(source: &str) -> Truth {
    eval_with(
      source,
      &facts_with(&[".jj"], false),
      &bound(&[("p", "/tmp/x"), ("q", "src/main.rs")]),
    )
    .truth
  }

  // --- parsing ---

  #[test]
  fn every_form_parses() {
    assert_eq!(
      cond("(ancestor-has? \".jj\")"),
      fact("ancestor-has?", &[lit(".jj")])
    );
    assert_eq!(
      cond("(under? ?p \"/tmp\")"),
      fact("under?", &[var("p"), lit("/tmp")])
    );
    assert_eq!(
      cond("(under? \"/tmp/x\" \"/tmp\")"),
      fact("under?", &[lit("/tmp/x"), lit("/tmp")])
    );
    assert_eq!(
      cond("(and (ancestor-has? \".jj\") (not (or (under? ?p \"/tmp\"))))"),
      Cond::And(vec![
        fact("ancestor-has?", &[lit(".jj")]),
        Cond::Not(Box::new(Cond::Or(vec![fact(
          "under?",
          &[var("p"), lit("/tmp")]
        )]))),
      ])
    );
  }

  #[test]
  fn a_condition_is_a_list_headed_by_a_fact() {
    let expected = "expected a condition such as (ancestor-has? \".jj\")";
    assert_eq!(error("foo", &[]), format!("1:1: {expected}"));
    assert_eq!(error("\"foo\"", &[]), format!("1:1: {expected}"));
    assert_eq!(error("()", &[]), format!("1:1: {expected}"));
    assert_eq!(error("(\"x\" 1)", &[]), format!("1:2: {expected}"));
    assert_eq!(error("(exists? \"x\")", &[]), "1:2: unknown fact `exists?`");
  }

  #[test]
  fn a_declared_fact_is_a_known_name() {
    let mut facts = Facts::builtin();
    facts.declare("exists?", Stub(Answer::holds()));
    let parsed = parse(
      &sexp::read_one("(exists? \"x\" ?p)").unwrap(),
      &scope(&["p"]),
      &facts,
    )
    .unwrap();
    assert_eq!(parsed, fact("exists?", &[lit("x"), var("p")]));
  }

  #[test]
  fn arities_and_argument_kinds_are_checked_by_the_fact() {
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
      "1:16: expected a `?binder` or a string"
    );
    assert_eq!(
      error("(ancestor-has? ?p)", &["p"]),
      "1:16: `(ancestor-has? ...)` takes a string, not a binder"
    );
    assert_eq!(
      error("(under? ?p)", &["p"]),
      "1:1: `(under? ...)` takes a path and a prefix"
    );
    assert_eq!(
      error("(under? ?p /tmp)", &["p"]),
      "1:12: expected a `?binder` or a string"
    );
    assert_eq!(
      error("(under? ?p ?q)", &["p", "q"]),
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
      "1:29: unknown fact `nope`"
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
  fn a_fact_answers_through_the_registry() {
    assert_eq!(eval("(ancestor-has? \".jj\")"), Truth::True);
    assert_eq!(eval("(ancestor-has? \".git\")"), Truth::False);
    assert_eq!(eval("(under? ?p \"/tmp\")"), Truth::True);
    assert_eq!(eval("(under? ?q \"/Users/x/proj\")"), Truth::True);
    assert_eq!(eval("(under? ?q \"/tmp\")"), Truth::False);
  }

  #[test]
  fn a_missing_binding_is_unknown_with_the_binder_named() {
    assert_eq!(
      eval_with(
        "(under? ?p \"/tmp\")",
        &facts_with(&[], false),
        &Bindings::new()
      ),
      Answer::unknown("`?p` is not bound")
    );
  }

  #[test]
  fn and_or_not_follow_kleene() {
    let mut facts = Facts::builtin();
    facts.declare("u?", Stub(Answer::unknown("stubbed")));
    let b = Bindings::new();
    let t = "(under? \"/a\" \"/a\")";
    let f = "(under? \"/a\" \"/b\")";
    let u = "(u?)";
    let go = |src: &str| eval_with(src, &facts, &b).truth;

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

  #[test]
  fn reasons_travel_with_the_answer() {
    let mut facts = Facts::builtin();
    facts.declare("a?", Stub(Answer::holds().with_reason("a held")));
    facts.declare("b?", Stub(Answer::holds().with_reason("b held")));
    facts.declare("f?", Stub(Answer::fails().with_reason("f failed")));
    facts.declare("u?", Stub(Answer::unknown("u timed out")));
    facts.declare("v?", Stub(Answer::unknown("v timed out")));
    facts.declare("t?", Stub(Answer::holds()));
    let go = |src: &str| eval_with(src, &facts, &Bindings::new());

    // A fact's own reason; an unknown's names the fact.
    assert_eq!(go("(a?)"), Answer::holds().with_reason("a held"));
    assert_eq!(go("(u?)"), Answer::unknown("u? is unknown: u timed out"));
    // The first unknown, in evaluation order.
    assert_eq!(
      go("(and (a?) (u?) (v?))"),
      Answer::unknown("u? is unknown: u timed out")
    );
    assert_eq!(
      go("(or (f?) (v?) (u?))"),
      Answer::unknown("v? is unknown: v timed out")
    );
    // The deciding part.
    assert_eq!(
      go("(and (a?) (u?) (f?))"),
      Answer::fails().with_reason("f failed")
    );
    assert_eq!(
      go("(or (f?) (u?) (a?))"),
      Answer::holds().with_reason("a held")
    );
    // Every part, joined, when all agree.
    assert_eq!(
      go("(and (a?) (t?) (b?))"),
      Answer::holds().with_reason("a held; b held")
    );
    assert_eq!(go("(and (t?) (t?))"), Answer::holds());
    assert_eq!(
      go("(or (f?) (f?))"),
      Answer::fails().with_reason("f failed; f failed")
    );
    // `not` flips the truth and keeps the reason.
    assert_eq!(go("(not (f?))"), Answer::holds().with_reason("f failed"));
    assert_eq!(
      go("(not (u?))"),
      Answer::unknown("u? is unknown: u timed out")
    );
  }

  // --- choosing a binding set ---

  #[test]
  fn choose_takes_the_first_binding_set_that_holds() {
    let facts = facts_with(&[], false);
    let call = Call::at(Path::new("/x"));
    let candidates = vec![
      bound(&[("p", "/var/a")]),
      bound(&[("p", "/tmp/a")]),
      bound(&[("p", "/tmp/b")]),
    ];
    let under_tmp = cond("(under? ?p \"/tmp\")");
    assert_eq!(
      choose(Some(&under_tmp), candidates.clone(), &facts, &call),
      Choice::Holds {
        bindings: bound(&[("p", "/tmp/a")]),
        evidence: None
      }
    );
    assert_eq!(
      choose(None, candidates.clone(), &facts, &call),
      Choice::Holds {
        bindings: bound(&[("p", "/var/a")]),
        evidence: None
      }
    );
    let under_etc = cond("(under? ?p \"/etc\")");
    assert_eq!(
      choose(Some(&under_etc), candidates, &facts, &call),
      Choice::NoMatch
    );
    assert_eq!(choose(None, vec![], &facts, &call), Choice::NoMatch);
  }

  #[test]
  fn choose_reports_the_first_unknown_when_no_set_holds() {
    let mut facts = Facts::builtin();
    facts.declare("u?", Stub(Answer::unknown("timed out")));
    let call = Call::at(Path::new("/x"));
    let parse_with =
      |src: &str| parse(&sexp::read_one(src).unwrap(), &scope(&["p"]), &facts).unwrap();
    // Unknown alone.
    let unknown = parse_with("(u?)");
    assert_eq!(
      choose(Some(&unknown), vec![Bindings::new()], &facts, &call),
      Choice::Unknown {
        bindings: Bindings::new(),
        reason: "u? is unknown: timed out".into()
      }
    );
    // A false set, then two unknown sets: the first unknown is reported.
    let mixed = parse_with("(and (under? ?p \"/tmp\") (u?))");
    let candidates = vec![
      bound(&[("p", "/var/a")]),
      bound(&[("p", "/tmp/a")]),
      bound(&[("p", "/tmp/b")]),
    ];
    assert_eq!(
      choose(Some(&mixed), candidates, &facts, &call),
      Choice::Unknown {
        bindings: bound(&[("p", "/tmp/a")]),
        reason: "u? is unknown: timed out".into()
      }
    );
    // A set that holds wins over every unknown, wherever it sits.
    let either = parse_with("(or (u?) (under? ?p \"/tmp\"))");
    let candidates = vec![bound(&[("p", "/var/a")]), bound(&[("p", "/tmp/a")])];
    assert_eq!(
      choose(Some(&either), candidates, &facts, &call),
      Choice::Holds {
        bindings: bound(&[("p", "/tmp/a")]),
        evidence: None
      }
    );
  }

  #[test]
  fn choose_carries_the_reasons_of_a_condition_that_held() {
    let mut facts = Facts::builtin();
    facts.declare("a?", Stub(Answer::holds().with_reason("a held")));
    facts.declare("b?", Stub(Answer::holds().with_reason("b held")));
    let call = Call::at(Path::new("/x"));
    let both = parse(
      &sexp::read_one("(and (a?) (b?))").unwrap(),
      &scope(&[]),
      &facts,
    )
    .unwrap();
    assert_eq!(
      choose(Some(&both), vec![Bindings::new()], &facts, &call),
      Choice::Holds {
        bindings: Bindings::new(),
        evidence: Some("a held; b held".into())
      }
    );
  }
}
