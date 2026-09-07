# ADR 0002: No defaults for `:lifetime` and `:timeout` on a fact

Status: accepted, 2026-09-07. Relates to D8 and D9 in
[the rule-language decision log](../design/rule-language/01-decisions.md).

## Decision

Every fact declaration states its `:lifetime` and its `:timeout`. A
declaration missing either is a positioned load error, and the file fails
open like any other type error. The first version accepts one lifetime,
`fresh`. No default exists for either keyword.

## Context

The decision log proposes one second as the default timeout and lists it
as an open question. Lifetimes are named in D8 with no default. Both are
numbers we have no data for: no extern fact has run yet, so no timing has
been logged.

A rule file is a contract the loader enforces. Whatever a keyword means
when absent becomes part of that contract the moment one user's file
relies on it.

## Options

Choose defaults now. One second and `fresh` are plausible. But a default
chosen without data is a guess, and changing a guess later silently
changes what every existing file means. A user whose fact took 1.5
seconds and passed under a later 2 second default would see it start
timing out if the default moved back.

Require both now, add defaults later. A required keyword added to a file
today keeps its meaning under every future version. Adding a default
later relaxes the grammar and breaks no file. The first defaults can come
from the log, which will hold the elapsed time of every fact that ran.

## Consequences

- Declarations are one line longer. The how-to and the tutorial show the
  full form, so nobody has to know the defaults do not exist.
- The open question on the default timeout closes as "not yet". It
  reopens when the log has timings from real use.
- The same rule applies to any keyword added to a fact later: required
  first, defaulted when the data says what the default should be.
- The load error names the keyword and the declaration:
  `rules.scm:12:1: (fact in-jj-repo? ...) needs :timeout`.
