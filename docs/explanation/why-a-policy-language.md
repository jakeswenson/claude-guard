# Why a policy language

The rules could have been Rust. They were, for the first two days. The reason they are a file in a small language of their own, rather than Rust or an embedded Scheme, comes down to one choice: rules are policies, not programs.

## Policies, not programs

A rule answers one question about one tool call: should this run? Everything you want from the answer follows from keeping the language closed. It always terminates. It can be type-checked when the file loads, so a mistake is a line and a column rather than a surprise mid-session. Its meaning fits on [one page](../reference/rule-language.md). And a rule file with no extension declarations is provably unable to do anything but match and decide.

That is the bargain [Cedar](https://www.cedarpolicy.com/) makes for authorization policies and [Rego](https://www.openpolicyagent.org/docs/latest/policy-language/) makes for admission control, and it is why neither is Turing-complete. An embedded Scheme would have given the rules the power to compute anything, and with it the loss of every property above. The design considered Steel, Rego, Cedar, and Prolog before settling; the [decision log](../design/rule-language/01-decisions.md) has the comparison.

## It is a Datalog

Strip Prolog of compound terms, unbounded recursion, and cut, and what remains is Datalog: facts, predicates, rules, all decidable. That is the shape here. `(ancestor-has? ".jj")` is a fact about the world; `(under? ?p "/tmp")` is a fact over a value the pattern bound; a rule row is a Horn clause whose head is a decision. Datalog would call the named thing a predicate and its application a fact; the guard says fact for both, and every fact, built in or declared, is one type behind one trait in a registry the conditions consult by name. The vocabulary is borrowed on purpose, so the design does not have to invent names for things that have them.

One departure: Datalog rules form a set, and these form a list. The first deny, ask, or warn wins. "The first row that matches" is a semantics you can hold in your head, and it is what the engine had before the language existed.

## Three truth values

A condition is true, false, or unknown. Nothing produces unknown today, but the seam is there because the next facts will: one that asks an external program can time out, and one that depends on the working directory can find it ambiguous after `cd $DIR`. Every answer can carry a reason, so when an unknown reaches the model it says what was unknown.

Combining unknowns follows Kleene's tables, the same ones SQL uses for `NULL`:

| and | true | false | unknown |
|---|---|---|---|
| true | true | false | unknown |
| false | false | false | false |
| unknown | unknown | false | unknown |

| or | true | false | unknown |
|---|---|---|---|
| true | true | true | true |
| false | true | false | unknown |
| unknown | true | unknown | unknown |

`not` swaps true and false and leaves unknown alone. The two rows that matter for a guard: `false and unknown` is false, because one failed condition settles an `and` whatever the other says, and `true or unknown` is true for the same reason. Everything else that touches an unknown stays unknown, so a timed-out fact inside an `and` never turns into a silent pass. The design's next step turns "unknown on a matched pattern" into an ask that tells the model what was unknown, so it can remove the ambiguity itself.

## S-expressions

The syntax is s-expressions because they are the cheapest thing that satisfies every requirement at once. The reader is a hundred lines. One syntax covers rules, patterns, conditions, and declarations, so a value never has to be smuggled inside a string. Editors already highlight it. And `rules --export` is trivial, since the data prints the way it was written.

Square brackets mark a term pattern, so `[git -... stash ...]` reads as the shell line it matches and looks different from a fact. Binders take a leading `?`, the Datalog convention; facts take a trailing `?`, the Scheme convention for predicates. The two never collide.

## The spec is executable

Every rule the matcher, the conditions, and the elaborator follow has a `check` line under `spec/`, in the language itself, run by the tests:

```scheme
(check [git -... stash ...] misses  "git -C . stash")
(check [git -... stash ...] matches "git -C . stash" :commands ((command git (option "-C" :value))))
(check (and (ancestor-has? "t") (ancestor-has? "u")) unknown :ancestors unknown)
(check (and (a?) (u?)) unknown "u timed out" :facts ((a? holds) (u? unknown "u timed out")))
```

A condition check never touches the disk: `:ancestors` stands in for `ancestor-has?` with a list, and `:facts` stands in any fact by name with a fixed answer, which is how the spec states what a timed-out fact does before one exists.

A behavior without a check line does not exist. Changing the logic means adding a line, watching it fail, and making it pass, which is the same loop as the code, and the spec cannot go stale because it is what the tests run.
