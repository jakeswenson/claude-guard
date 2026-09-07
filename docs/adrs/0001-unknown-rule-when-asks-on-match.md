# ADR 0001: An unknown rule-level `:when` asks on a matching row

Status: accepted, 2026-09-07. Extends D14 in
[the rule-language decision log](../design/rule-language/01-decisions.md).

## Decision

A rule whose `:when` evaluates to unknown is not skipped. Its rows are
tried as if the `:when` had held. A deny or ask row that matches then
asks, with the reason for the unknown in parentheses as evidence. A warn
row skips. A rule whose `:when` is false is skipped, as today.

This is the safe starting point, not the final word. The ADR exists so the
choice is visible and can be reversed with a one-line change and a spec
line.

## Context

D14 settles the row case: a matched pattern with an unknown condition
asks. It does not say what a rule-level `:when` does, because a rule
`:when` runs before any pattern matches and so has no "matched pattern"
to attach the ask to.

Today the engine skips a rule whose `:when` is anything but true, at
`src/rules.rs:177`. Nothing produces unknown yet, so the choice has had
no effect. Extern facts change that: `(rule jj-only :when (in-jj-repo?) ...)`
with a fact that times out is the first real case.

## Options

Skip the rule. The rule has no opinion, so the guard moves on. This reads
"unknown" as "absent", which is the reading D14 rejected for rows: a
timeout on a deny rule would let the command through with no trace in
the reply. The log would show a pass. The agent learns nothing and the
user finds out later.

Ask on a matching row. The rule's opinion is unresolved, not absent. If
one of its rows would have fired, the guard says so and names the fact it
could not settle. The agent can fix the ambiguity, or the user can answer.
The cost is a false ask when the `:when` would have been false, which is
noise, not a hole.

Ask on every call. Too broad: a rule whose rows never match the call has
nothing to say about it.

## Consequences

- An unknown never becomes a silent pass, at either level. That matches
  principle 6 as written for rows and extends it upward.
- A rule that guards many rows with one extern fact will ask on every
  matching row while the fact is unknown. That is loud on purpose. If it
  proves too loud, the fix is a `:lifetime session` fact that is computed
  once, not a return to skipping.
- The evidence for a rule-level unknown is the same text as for a row:
  the fact name and the reason the fact gave.
- Spec lines: `(check ...)` forms for a rule with an unknown `:when` and a
  matching deny row, a matching warn row, and no matching row.
