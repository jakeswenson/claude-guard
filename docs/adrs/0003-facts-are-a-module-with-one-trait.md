# ADR 0003: Facts are a module with one trait, built-in and extern alike

Status: accepted, 2026-09-07. Relates to D5, D6, D8, and D9 in
[the rule-language decision log](../design/rule-language/01-decisions.md)
and principle 8.

## Decision

A new module `src/facts.rs` owns every fact the language can name. One
trait, `Fact`, is implemented by each built-in fact and by the extern
runner. A registry, `Facts`, maps fact names to implementations: the
built-ins first, then the declarations the loaded file adds. The
condition language calls facts only through the registry.

The `Fs` trait in `src/cond.rs` is deleted. The `Cond` enum loses its
per-fact variants and keeps one `Fact { name, args }` variant plus the
combinators.

"Fact" is the one term. The decision log also says "predicate" for the
named thing before arguments are applied; the code, the errors, and the
reference use "fact" for both, and the reference defines it once.

## Context

Conditions today know two facts, `ancestor-has?` and `under?`, as
variants of the `Cond` enum in `src/cond.rs`. The parser is a match on
the name, the evaluator is a match on the variant, and `ancestor-has?`
reaches the disk through a trait named `Fs` with one method. That trait
was a test seam for the one filesystem question conditions asked. It was
never a facts abstraction, and its name says so.

Extern facts do not fit this shape. A declared fact is not a variant the
compiler knows. Every fact, built-in or extern, must answer with a truth
and a reason, be stubbed by name in the spec and the tests, and report
itself to the log record (principle 12). Adding a built-in fact today
means editing an enum, a parser arm, and an evaluator arm in one file,
with no shared contract between them.

## Options

Keep the enum and add an `Extern` variant. Least code. The three-place
edit for every new built-in stays, the `Fs` seam stays for one fact and
the extern runner grows a second seam, and the log gains a third path to
learn what was evaluated. The pattern the user objected to, repeated.

One trait and a registry. Every fact is one type in one file implementing
one contract: check the arguments at load time, answer at call time. The
registry is the seam: a test or a spec line registers a stub under a name
and the engine cannot tell the difference. The log records at the
registry, once, for every fact.

A trait per kind, `BuiltinFact` and `ExternFact`. Two contracts for one
concept, and the registry would need to know which is which. Rejected.

## Shape

Implemented 2026-09-07 as below, with one refinement: `check` receives
each argument paired with its position, so a fact can point an error at
the argument rather than the form.

```rust
// src/facts.rs
string_id! { FactName }

pub struct Answer {
  pub truth: Truth,
  /// Evidence: why it holds, why it fails, or why it is unknown.
  pub reason: Option<String>,
}

pub trait Fact {
  /// The load-time check: arity and argument kinds, with positions.
  fn check(&self, args: &[Arg], span: Span) -> Result<(), TypeError>;
  /// The call-time answer, with every argument resolved to text.
  fn ask(&self, args: &[&str], call: &Call<'_>) -> Answer;
}

pub struct Facts { by_name: BTreeMap<FactName, Box<dyn Fact>> }
```

Files follow the flat layout: `src/facts.rs` declares `facts/ancestor_has.rs`,
`facts/under.rs`, and `facts/exec.rs`. `Call` carries what a fact may see:
cwd, session, tool, and the term. Memoization per hook call and the log
entries live at the registry, not in any fact.

## Consequences

- One place to add a fact, one contract to read. The reference lists
  facts from the registry, and a fact without a spec line does not exist.
- `Fs`, `RealFs`, `FakeFs`, and `SpecFs` go. The spec's `:ancestors`
  keyword stays as sugar that registers a stub `ancestor-has?`. A new
  `:facts` keyword stubs any fact by name with holds, fails, or unknown
  and a reason.
- Truth stays three-valued and `Copy`. The reason lives on `Answer`, so
  a holds answer can carry evidence too, as D10 needs for the deny text.
- `Cond::parse` takes the registry, so an unknown name and a wrong arity
  stay load errors with positions.
- The error text changes from `unknown predicate` to `unknown fact`.
- This is a refactor with no behavior change. Every existing test and
  spec line passes before the extern runner is added.
