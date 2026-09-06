# Decision Log: claude-guard rule language

Interview started 2026-09-05. Baseline principles were proposed by Claude in
`02-principles.md` before the interview; each is marked below as confirmed,
amended, or still open.

## Problem (their words, refined as it sharpens)

Rules are Rust const data today, and a rule change is a rebuild plus
`cargo install`. Some of what is written as data is logic: in-jj-repo is a
predicate over the filesystem, and other people will want in-git-repo,
in-perforce, in-mercurial, "has a CLAUDE.md", or checks nobody has named
yet. The rules need to live in a file with a language that is formal,
strict, clean, and built for pattern matching over nested commands and
arguments. *(their words: "i want something clear, formal, clean, elegant"
and "S expressions are supposed to be, like, the bomb for pattern matching
on")*

## Decisions

- **D1** — Rules are policies, not programs. The language is closed and
  total. Capability grows through an escape hatch of libraries, functions,
  or packages, whose shape is still open. *(their words: "policies. maybe
  with escape hatch libraries/functions/packages that extend the
  capabilities?")*
- **D2** — One syntax. Term patterns are s-expressions, not strings inside
  s-expressions. *(their words: "the latter, syntax does it.")*
- **D3** — Tool calling is a key part of the predicate scope. What "tool
  calling" means here is pinned in the next interview round. *(their words:
  "i think tool calling will be key... unfortunately")*
- **D4** — The design comes before the spike: decision log, interview,
  principles. *(their words: "yeah the doc + interview + principles")*
- **D5** — "Tool calling" means a predicate may run an external program to
  get a fact, such as `jj root` or a script of the user's. Not rules over
  non-Bash tools (those come for free from the term) and not asking a
  model. *(their words: "B")*
- **D6** — Facts are propositions. No string-valued facts; a fact holds,
  does not hold, or is unknown. What a string fact would have carried
  becomes a predicate with a typed argument, such as `(ancestor-has? ".jj")`
  or `(under-repo-root? <path>)`. *(their words: "jj-root is kinda dump...
  i think i want to avoid string facts... jsut bool facts")*
- **D7** — The language is a Datalog, and the doc says so. Vocabulary:
  facts, predicates, derived predicates, rules. One departure from Datalog:
  rules are an ordered list, and the first deny, ask, or warn wins. *(their
  words: "are we inventing prolog?" then "i guess so?")*
- **D8** — Facts have a declared lifetime: `session`, `fresh`, or a
  duration. Session facts are computed once by the SessionStart hook and
  stored in the session log as their own record kind, so PreToolUse never
  forks for them and a deny record can name the facts it used. *(their
  words: "they don't need to execute every time. it might be possible to
  cache the facts per session/cwd")*
- **D9** — Extern predicates are programs over stdio. In: rule arguments as
  argv, the basics as `CLAUDE_GUARD_*` environment variables, the whole
  term as JSON on stdin. Out: one JSON object, `{"holds": bool, "reason":
  string?}`. Non-zero exit, timeout, or unparseable stdout means unknown.
  Stderr passes through. A `claude-guard extern <name> [args]` subcommand
  runs one extension the way the hook would. *(their words: "we should
  require a json stdio that is the bool + a reason ... simple enough that a
  script (bash, python, ...) (wrapping many exe calls) or a custom bin could
  work" and "i like it")*
- **D10** — The extension's reason is evidence, not a verdict. It goes in
  the log record and is appended to the deny text in parentheses. The rule
  owns the reason and the instead. *(their words: "evidence in deny text
  (paranthsis context thing)")*
- **D11** — Syntax is s-expressions. Term patterns are written in square
  brackets with bare words as literals, so `[git -... stash ...]` reads as
  the shell line it matches. Words that would confuse the reader are
  strings inside the bracket. *(their words: "i'm ok with brackets its all
  the same for s-exp right?")*
- **D12** — The escape hatch is extern predicates over stdio plus
  non-recursive derived predicates written in the language. Steel and WASM
  are rejected for now; a WASM host could speak the D9 protocol later
  without changing any rule file. *(their words: "Thinking either more
  Steel, Wasm, or some stdio/ipc extension model? ... not sure", settled by
  D5 and D9)*
- **D13** — The evaluator tracks a cwd through a command sequence. For each
  command the cwd is known (a resolved path) or ambiguous (with a reason).
  A `cd` with a literal target that exists keeps it known whichever
  separator follows. Ambiguous: a dynamic word in `cd`, `cd -`, `pushd`,
  `popd`, a literal target that does not exist followed by `;`, and any
  `cd` inside an uninspected string. A `cd` in a subshell does not leak
  out. `(cwd-known?)` is a built-in fact over this state, and every cwd
  predicate evaluates against the tracked cwd. Live evidence: the
  `.homelab` session of 2026-09-05 reported a subdirectory cwd for twelve
  records after a `cd .terraform/... && ...` call. *(their words: "is that
  like cd /foo/bar; jj desc ... because that does get fucked fast" and,
  on `;`, "i'm not sure what it should do... seems known?")*
- **D14** — A matched pattern with an unknown condition asks, and the
  reason for the unknown is the evidence in parentheses. Term patterns
  never produce unknown, so relevance is always decided; only conditions
  can be unknown. Warn rules skip instead of asking. This supersedes the
  "unknown skips the rule" wording of D6. *(their words: "when it is
  ambiguous that reason shows up in the deny predicate... this way the
  agent could make it less ambiguous")*
- **D15** — The logic is documented by an executable spec written in the
  language: one `check` form per decision, run by the test suite and
  rendered into the semantics page. A rule of the evaluator without a
  check line does not exist. *(their words: "self documenting tests is
  huge and should be a principle")*
- **D16** — Read freshness is a predicate. The guard records the mtime and
  size of every file a Read, Write, or Edit touched, at PostToolUse. Two
  predicates: `(has-read? <path>)` and `(read-fresh? <path>)`, the second
  holding only when the recorded mtime and size match the file now. Scope
  is the session: a read from an earlier session does not count, because
  the context that read it is gone. Target rule: deny a write, including
  Bash redirects and `tee`, to an existing file that is not read-fresh,
  with the last read time in the reason. *(their words: "track the modtime
  of each file read explicitly and deny write calls that don't have the
  current mod time" and, on scope, "claude reading yesterdays session
  doesn't mean the current session has the context to overwite the
  contents")*
- **D17** — Rules can span commands, in two scopes, both future work.
  Within one call: a sequence pattern matches more than one command of a
  compound command, such as `jj describe` followed by `jj new` in the same
  line, or `jj abandon` with no `jj status` earlier in the sequence. This
  is a term-pattern extension. Across calls: history predicates query the
  session log, `(ran? P)`, `(ran? P :within D)`, `(last-call? P)`,
  `(ran-since? P Q)`, where "ran" means PostToolUse arrived. Escalation and
  the dialog memory would become history primitives, `(denied-this? :within
  D)` and `(user-denied-this? :within D)`. Conditions attach to a row, not
  only to a rule. Neither scope is in the first version; the current
  limits come first. *(their words: "is it possible that rules could span
  multiple sequences of commands? like a `jj desc` followed by a `jj new`
  or `jj abandon` not preceeded by a `jj status`", then "i was even saying
  in a compound command, not cross thing ... document this and track it as
  future improvement ... p0 is the current limits we have")*
- **D18** — Patterns bind, conditions read the bindings, in that order.
  Variables carry a sigil, `?file`, so bare words stay literals. An
  unbound variable in a condition is a load-time error. A pattern that
  matches in more than one way fires if any binding satisfies the
  condition, and the record names that binding. Non-Bash tools use a
  parenthesized term form, `(write ?path)`, with the same binder. Path
  constraints such as `/tmp/**` move from the pattern into the condition,
  `(under? ?file "/tmp")`, so patterns stay word shapes. The leading `?`
  is the Datalog convention (Datomic: `[?e :person/name ?name]`).
  Predicate names end in `?`, the Scheme, Racket, and Clojure convention:
  `in-jj-repo?`, `ancestor-has?`, `under?`. The two never collide, and the
  suffix gives extern predicate authors a naming rule. Combinators stay
  bare: `and`, `or`, `not`. *(their words: "predicates might have to come
  after pattern matching the command so that you can extract out the file
  being read/written?" and "should it be like `in-jj-repo?` or isn't that
  common lisp/scheme for functions like that?")*
- **D19** — A binder captures one literal word and never a dynamic one:
  a condition needs the text, and the shell has not produced it. Rows
  that must catch dynamic words use `*` or `...`. A binder that appears
  twice must capture the same word both times, so `[cp ?x ?x]` matches
  `cp a a` and not `cp a b`, the Prolog reading. Implemented in step 2
  before it was written here; both facts get `check` lines in step 5.
  *(their words, on the missing write-up: "first decisions like that need
  to be documented")*
- **D20** — Near misses are logged. A pattern is a near miss when it
  matches with dynamic words allowed to stand in for any token and does
  not match strictly. The record for a pass lists each near miss with the
  rule, the pattern, and the dynamic words that made the difference, so
  the review UI can show why a rule did not fire and escalation can
  treat "`$cmd` could be `stash`" as a stronger signal than a bare
  `$(...)` warning. Whether a near miss speaks to the model is a rule
  decision for later. *(their words: "should a rule that doesn't bind for
  reasons like this (had a dynamic word) be logged/tracked so that the
  reason a rule didn't match is inspectable?")*
- **D21** — `under?` resolves a relative path against the call's cwd
  before comparing, so `echo hi > x` in `/tmp` is under `/tmp`. The Rust
  table's `/tmp/**` token left relative paths alone because the matcher
  has no cwd; the condition has one. When cwd tracking (D13) lands, the
  cwd here becomes the tracked one. A rule's `:when` cannot use binders,
  since it runs before any pattern matches; the checker says so in the
  error. Made in step 3 and written here the same day.
- **D22** — A command elaborator. Given one simple command it returns
  `{cmd, options: [{text, flags, value?}], args, inner?}` using
  per-command data: which flags take values, subcommands, and where an
  inner command or script sits. Elaboration is a partition of the words:
  every input word appears exactly once in the output, in order, and
  flattening the output gives the input back. `git clean -fxd` is one
  option group with text `-fxd` and flags `f`, `x`, `d`. The round-trip
  is checked over every Bash command in the session logs, and clustering,
  attached values, `--opt=value`, `--`, and `inner` get `check` lines.
  *(their words: "we need to test that it gave back the same thing it was
  given, just better classified")*
- **D23** — Elaboration does not change the meaning of `-...`, `...`, or
  `*`. It changes what they match on: for a declared command an option
  word is the flag together with its value, because the arity is known.
  `[git -... stash ...]` matches `git -C . stash` with no pattern change.
  Undeclared commands are matched as today. *(their words: "elaboration
  does not change the meaning of -... or ... ONLY what the match on. it
  just improves the resutls they're matching on (we know the arity of an
  option)")*
- **D24** — Data, not code. Declarations live in the rule language,
  `(command git (option "-C" :value) ...)`, with an extern elaborator over
  stdio as the escape hatch for a command whose grammar needs real
  parsing. Getopt rules apply to declared commands: `-fxd` splits when
  each letter is a known short flag, a value-taking short flag swallows
  the rest of its word or the next word, `--opt=value` splits at `=`,
  `--` ends options. *(their words: "i'm sorta game for the data not code
  thing")*
- **D25** — `inner` is one mechanism for wrappers and script strings:
  sudo, env, nice, nohup, timeout, xargs (the command after the options),
  ssh (the positional after the host), bash, sh, nu, python3 (`-c` then
  the first positional). The inner command or script is elaborated and
  matched like a top-level call, so a rule holds whether it sees
  `sudo sed ...` or `ssh nas 'bash -c "sed ..."'`. These declarations are
  hand-written and ship built in; no completion corpus knows them.
  *(their words: "the inner thing is nice for making rules sorta
  automatically work whether it is `ssh nas 'batch -c "sed ..."'` or
  `sudo sed ...`")*
- **D26** — Start small and grow from the log. Only the D25 set ships.
  `claude-guard commands` reads the session logs and lists every command
  seen with call count, generic-row hits (a catch-all pattern fired
  because `-...` stopped at a flag value), flagged calls (flag-shaped
  words followed by a non-dash word on an undeclared command), and
  status: built in, declared in the config dir, or undeclared, plus
  whether carapace knows it. Sorted by the two signals, not raw count.
  `claude-guard commands add <name>...` runs `carapace <name> export`,
  converts its JSON (Type bool, string, stringSlice, stringArray, count;
  NoOptDefVal; nested Commands with aliases) to
  `~/.config/claude-guard/commands/<name>.scm`, and prints what it wrote.
  Verified 2026-09-06: carapace-bin 1.7.3 exports every one of the 24
  commands the rules touch; 5,199 flags across them use only those five
  types. *(their words: "starting small and then offering a command to
  review/pick and chose your elaborators ... a subcommand to reivew the
  sesion and run carapace to generate the elaborator spec for the
  command" and "it could review your sessions for hot commands where
  elaboration could help")*
- **D27** — Command declarations load additively: the built-in set plus
  every file in the config `commands/` directory, the user's winning by
  name. Unlike rules, which replace whole, a declaration is a fact about
  a program rather than a policy.

## Scope: first version

The first version encodes the current Rust rule table and nothing else.
The guard loads the shipped file, and every existing test passes with the
same wire output. *(their words: "p0/mvp of this rules language is to be
able to encode our current custom rust rules and then later we can expand
to the more complicated features")*

In: rules with ordered deny, ask, and warn rows carrying reason and
instead; Bash patterns with the current tokens in bracket syntax;
variables; the predicates `ancestor-has?`, `under?`, `and`, `or`, `not`;
tool patterns for Write and Edit; the loader with the embedded file, an
optional user file, positioned type errors, and fail-open on a bad file;
`check` forms for the matcher and evaluator; `rules --export`; deletion of
the Rust table.

Out, with the types designed so they slot in later: extern predicates
(D9), fact lifetimes and SessionStart facts (D8), cwd tracking (D13),
unknown and evidence (D10, D14), history predicates and sequence patterns
(D17), read freshness (D16).

## Constraints Surfaced

- The current pattern language stays as the seed for term patterns: `*`,
  `...`, `-*`, `-...`, `<path>/**`, and the three redirect forms.
- The hook is a short-lived process. Whatever loads the rule file runs on
  every tool call.
- Rejected on the way here: Steel (programs, dynamic), Cedar (sets only,
  argument order is lost), Rego via regorus (works, no static types,
  syntax), Scryer Prolog (unification fits, untyped, young embed API).

## Tradeoffs Chosen

- Chose a closed language over an embedded Scheme, because strictness and a
  one-page semantics are worth more than the ability to compute anything
  (D1).
- Chose stdio over WASM for extensions, because D5 already requires
  spawning programs, and one mechanism beats two. The sandbox WASM would
  add protects the user from the user (D9, D12).
- Chose bool facts over typed values, because a value-returning fact is a
  function, and functions are what separate Prolog from Datalog (D6, D7).
- Chose s-expressions over a Cedar-style grammar, because the parser, the
  formatter, and editor support come for free and D2 wants one syntax
  (D11).
- Chose an ordered rule list over Datalog's unordered set, because "the
  first row that matches" is a semantics the reader can hold in their head
  and the current engine already has it (D7).

## Assumptions

Status of the baseline principles in `02-principles.md` after the
2026-09-05 round:

- P1 policies, not programs. **Confirmed** by D1.
- P2 one syntax. **Amended** by D11: brackets for term patterns.
- P3 typed at load, silent at call. **Unchallenged**; carried forward.
- P4 the term is the contract. **Unchallenged**; the D9 stdin JSON is that
  term serialized.
- P5 predicates are primitives with arguments. **Amended** by D6 and D8:
  bool-valued, three-valued, with lifetimes.
- P6 extension is explicit and named. **Confirmed** by D9 and D12.
- P7 first opinion wins in file order. **Confirmed** by D7.
- P8 a deny explains itself. **Amended** by D10: extension evidence is
  appended in parentheses.
- P9 builtins are a file too. **Unchallenged**; carried forward.
- P10 written semantics. **Unchallenged**; D7 gives it a name.

## Open Questions

- A session fact that fails at SessionStart: retried on the first
  PreToolUse, or unknown for the session with a line in the session brief?
- Where the file lives, and whether a project file found by walking up from
  cwd is evaluated before the user file.
- Can a rule return `allow` to short-circuit later rules? The engine has no
  such verdict today.
- Session facts for a cwd the session has not seen: computed lazily on
  first sight and kept per cwd is the proposal (follows from D13).
- Default extern timeout. One second is the proposal.
- Self-hosting structural logic such as cwd tracking as rules in the
  language needs recursion over sequence position. Deferred until a second
  piece of structural logic (ssh, wrappers) wants the same machinery.
- D16 and subagents: does a read by a subagent count for the parent
  session's write, or does each agent id carry its own reads? Waits on
  claude-guard-6o4.
