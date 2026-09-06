# Principles: claude-guard rule language

Proposed 2026-09-05 by Claude, amended in the same day's interview. Status
of each line is tracked in `01-decisions.md`. The loader, the evaluator,
and the extern runner must obey every line here.

1. **Policies, not programs.** A rule file cannot loop, recurse, or define
   a predicate in terms of itself. Every evaluation terminates, and the
   worst case is bounded by the size of the file and the call.
2. **One syntax.** Rules, predicates, and term patterns are s-expressions.
   Term patterns are written in square brackets with bare words as
   literals: `[git -... stash ...]`. No string mini-language survives
   inside the file.
3. **Typed at load, silent at call.** The file is parsed and type-checked
   when the hook starts. A type error reports the file, line, and column,
   and the guard fails open for that call. A file that loaded never fails
   at evaluation time.
4. **The term is the contract.** The language sees one typed value per
   call: the tool, the subject as a tree (commands, words, redirects,
   nested scripts for `ssh` and `bash -c`), and the context (cwd, session,
   agent). That value is defined once in Rust, published as a JSON schema,
   and is exactly what an extern predicate reads on stdin.
5. **It is a Datalog.** Facts, predicates, derived predicates, rules. A
   fact holds, does not hold, or is unknown; there are no string-valued
   facts. The one departure: rules are an ordered list, and the first
   deny, ask, or warn wins.
6. **Unknown on a matched pattern asks.** Term patterns always decide;
   only conditions can be unknown. A deny or ask rule whose pattern
   matched and whose condition is unknown asks, with the reason for the
   unknown as evidence, so the agent can remove the ambiguity. Warn rules
   skip.
7. **The evaluator knows where a command runs.** A cwd is tracked through
   each sequence: known, or ambiguous with a reason. `cd` with a literal
   target that exists keeps it known. Every cwd predicate evaluates
   against the tracked cwd, and `(cwd-known?)` exposes the state.
8. **Predicates are primitives with arguments.** In-jj-repo is
   `(ancestor-has? ".jj")`, not a built-in rule. The language ships a small
   fixed set of primitives over the filesystem near cwd, the environment,
   the session log, and time. Adding a capability means adding a
   primitive, not adding syntax.
9. **The log is a database.** History predicates query the session log:
   what ran, when, what was denied, what the user answered, what was read
   and at which mtime. Escalation and the dialog memory are rules over
   these predicates, not engine code. Conditions attach to a row.
10. **Facts have lifetimes.** `session`, `fresh`, or a duration. Session
    facts are computed once at SessionStart and stored in the session log
    as their own record kind. PreToolUse never forks for a session fact.
11. **Extension is a program over stdio, declared by name.** Arguments as
    argv, basics as `CLAUDE_GUARD_*` environment variables, the term as
    JSON on stdin, one JSON object `{"holds": bool, "reason": string?}` on
    stdout. Non-zero exit, timeout, or unparseable stdout is unknown. An
    extension adds a relation, never a value and never a verdict. A file
    with no `extern` declarations is closed by inspection.
12. **A deny explains itself.** Every deny and ask carries a reason and an
    instead. Extension evidence is appended in parentheses. The log record
    names the rule, the pattern, the facts used, and any predicate that
    was unknown.
13. **Builtins are a file too.** The rules shipped with the binary are a
    rule file embedded at build time, read by the same loader. There is no
    second rule representation in Rust. `claude-guard rules --export`
    writes the embedded file out as a starting point.
14. **Every extension is runnable by hand.** `claude-guard extern <name>
    [args]` reads a call from stdin and runs the extension the way the
    hook would, printing the parsed result and the time it took.
15. **The spec is executable.** Every rule of the evaluator has a `check`
    form in the language, run by the test suite and rendered into the
    semantics page. A rule without a check line does not exist.
16. **The language has a written semantics.** One page: the grammar, the
    input type, the evaluation order, the three truth values, and the
    meaning of every primitive. A rule's behavior is decidable from that
    page without reading Rust.
