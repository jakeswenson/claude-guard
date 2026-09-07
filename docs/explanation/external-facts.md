# External facts

The rule language is closed: a file can match and decide and nothing else. That is the property that makes it type-checkable at load and decidable from one page. It is also why the language cannot answer questions like "is this a jj repo", "does this branch have an open pull request", or "is this path owned by root" on its own. Those need the world, and the world needs a program.

External facts are the one door in the wall. This page says why the door is the shape it is.

## A relation, not a verdict

A fact adds a proposition the rules can name: `(on-main?)` holds, fails, or is unknown. It cannot say deny. The rule that names it owns the reason and the instead, and the fact's own reason is appended after them in parentheses as evidence.

That split is the point. If a program could return a decision, the rule file would no longer be the policy; the policy would be wherever the program is, in a language the loader cannot check and a reader cannot see. With facts as relations, every deny still traces to a row in the file, and every row still reads as "this pattern, under this condition, means this." A program can make a condition true. It cannot make a rule exist.

The same line keeps values out. A fact is a boolean, not a string a rule compares. "What branch is this" is a function; "is this branch main" is a relation. Functions are what separate Prolog from Datalog, and [the language is a Datalog](why-a-policy-language.md#it-is-a-datalog). A fact that needs a value takes it as an argument, `(owned-by? "root" ?dst)`, and answers whether the relation holds.

## Why stdio

The program gets argv, four environment variables, and one JSON object on stdin; it writes one JSON object to stdout. A shell script can do that in five lines, Python in ten, and a compiled program in whatever it likes. Nothing links against the guard.

The alternative considered was a WASM host. It would add a sandbox, but the sandbox would protect the user from the user, since the programs are theirs, and it would make every fact a build step. Stdio was already needed for facts like `jj root`, and one mechanism beats two. A WASM host could still speak this protocol later without a rule file changing.

The stdin object carries the call's subject in the same shape the log record uses. There is one definition of what the rules saw, in Rust, and both the record and the program get it. A fact that reads the subject is reading the same thing the review of the log reads.

## Why an unknown asks

A program can time out, crash, print garbage, or not start. The language has a truth value for that: unknown. What the engine does with an unknown was the first real decision of this design, and the answer is that a matched pattern with an unknown condition asks, with the reason in parentheses.

The rejected reading was "unknown skips the rule." Under it a timed-out fact on a deny row lets the command through with no trace in the reply, and the log shows a pass. Asking is loud, and it is meant to be: the model sees `on-main? is unknown: exited with status 3`, understands what could not be settled, and can often remove the ambiguity itself. A warn row in the same position skips, because a warning with an unsettled premise is noise. The same rule holds one level up for a rule's own `:when`; [ADR 0001](../adrs/0001-unknown-rule-when-asks-on-match.md) has the two options and why ask won.

Combining unknowns follows Kleene's tables, so an unknown inside an `and` next to a false is false, and next to a true is unknown. A timed-out fact never becomes a confident answer by accident. The [conditions spec](../../spec/conditions.scm) states every row of the tables.

## Why no defaults

Every declaration says `:lifetime fresh` and `:timeout "1s"` in full. Neither has a default, and that was decided rather than forgotten. A default chosen now would be a guess, since no fact had run when the language was designed. A guess changed later would silently change what every existing file means; a keyword required from the start keeps its meaning under every future version. The first defaults, if any, will come from the log, which now records how long every fact took. [ADR 0002](../adrs/0002-no-defaults-for-lifetime-and-timeout.md) has the reasoning.

## Why the registry, and what it does

Built-in facts and declared facts are the same kind of thing to the engine: one type each behind one trait, registered by name. A condition cannot tell whether `ancestor-has?` walks the disk or `on-main?` runs a script, and neither can the spec, which stands in any fact by name with a fixed answer and so never starts a process. [ADR 0003](../adrs/0003-facts-are-a-module-with-one-trait.md) records the choice over an enum with an extern variant.

The registry is where three things happen once for every fact rather than separately for each. An unknown is prefixed with the fact's name, so evidence always says which fact could not be settled. An answer is memoized by fact, arguments, and working directory, so a program named by three rows runs once per call. And every ask is kept, in order, with its answer and timing, which is the `facts` array on the log record.

## Why the process group

A timeout kills the program. It also kills everything the program started, because the first test that ran a `sh -c "sleep 5"` script past its timeout left `sleep` alive, holding the hook's stderr open. Claude Code waits for a hook's output streams to close, so the timeout the rule promised would have been the grandchild's five seconds, not the rule's one. The program leads its own process group and the group is killed together. A fact author does not have to think about this; it is why the how-to can say a stuck network call does not hold the session.

## What is not here yet

Every declared fact is `fresh`: asked on every call that needs it. Session facts, computed once at session start and stored in the log, are designed and not built, and they are the answer when a fact is expensive and its answer does not change within a session. A shell-native form, where exit status decides and stdout is the reason, is filed for discussion; writing JSON from a shell script is the ugliest line in this documentation and the reason that idea exists.
