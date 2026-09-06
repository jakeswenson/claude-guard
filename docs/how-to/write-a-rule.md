# Write a rule

A rule is a named group of rows. Each row says what to match, what to decide, and what the model should read.

```scheme
(rule tmp-writes
  (deny [cp ... ?dst] :when (under? ?dst "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example."))
```

Rules go in `~/.config/claude-guard/rules.scm`. See [replace the built-in rules](replace-the-builtin-rules.md) for creating that file. After every edit, run `claude-guard rules`; it prints the file, line, and column of any mistake.

## Choose the decision

- `deny`: the call does not run. The model reads the reason and the instead, and reroutes.
- `ask`: the call waits for you. Your permission dialog shows the reason. Use it when the model might be right.
- `warn`: the call runs. The model reads the reason as context. `:instead` is optional here.

Order matters. Rules run top to bottom, rows top to bottom, and the first deny, ask, or warn wins. Put specific rows before general ones, and warn rules after deny rules, so a note never shadows a stop.

## Write the pattern

A pattern is a shell command in square brackets. Bare words are literals; wildcards stand for words:

| Token | Takes |
|---|---|
| `git` | that word, exactly |
| `"hi there"` | that word, when it holds spaces or quotes |
| `*` | one word, any kind |
| `...` | zero or more words |
| `-*` | one option |
| `-...` | zero or more options |
| `?name` | one word, captured for the condition |
| `> file`, `>> file`, `< file` | a redirect, in any position |

`[git -... stash ...]` matches `git stash`, `git --no-pager stash pop`, and `git -C . stash` once git is declared, since the declaration tells the matcher that `-C .` is one option. Without a declaration `-...` stops at `.`, and the row does not match. [Declare a command](declare-a-command.md) fixes that.

A pattern matches any simple command in the call. `ls && git stash` and `cat x | sed s/a/b/` both hit the rows for `git stash` and `sed`. It also matches inside wrappers: `sudo sed`, `ssh box 'sed ...'`, and `bash -c 'sed ...'` all reach the `sed` row.

A word the shell would expand, such as `$dir` or `$(pwd)`, matches only `*` and `...`. The guard does not know what it will become.

## Match a file tool

The Write, Edit, MultiEdit, and Read tools have their own subject form:

```scheme
(deny (write ?path) :when (under? ?path "/tmp")
  :reason "no files under /tmp." :instead "write inside the project.")
(ask (read "/etc/shadow") :reason "that file is sensitive." :instead "say why it is needed.")
```

## Add a condition

A condition guards a row with `:when`, or a whole rule when placed after its name. It can use what the pattern captured:

```scheme
(rule git-in-jj :when (ancestor-has? ".jj")
  (deny [git -... log ...] :reason "this repo is managed by jj." :instead "use `jj log`."))

(deny [mv ... ?dst] :when (under? ?dst "/tmp") ...)
(deny [cp ?src ?dst] :when (and (under? ?src "/etc") (not (under? ?dst "/etc"))) ...)
```

Two predicates exist today: `(ancestor-has? "name")`, true when the working directory or any directory above it contains `name`, and `(under? path "prefix")`, true when the path is the prefix or below it. A relative path resolves against the call's working directory, and `/private/tmp` counts as `/tmp`. `and`, `or`, and `not` combine them.

A binder used in a condition must appear in the row's pattern. A rule-level `:when` runs before any pattern matches, so it cannot use binders. Both are load-time errors with a position.

## Test a rule without a session

Feed the hook a call by hand:

```
echo '{"session_id":"t","cwd":"/tmp/x","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git stash"},"tool_use_id":"t1"}' \
  | CLAUDE_GUARD_STATE_DIR=./guard-scratch claude-guard hook
```

A deny prints the decision JSON. Silence is a pass. `CLAUDE_GUARD_STATE_DIR` keeps the test's log line out of your real sessions; delete the scratch directory afterwards.

For a rule file under development, `CLAUDE_GUARD_RULES=path/to/rules.scm` makes the guard use that file, and `claude-guard rules` lints it.
