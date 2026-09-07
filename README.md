# claude-guard

A Claude Code hook that denies tool calls that break your house rules, tells the model why and what to do instead, and logs every decision.

Rules live in a small policy language, not in code. A rule is a pattern over a shell command, a condition on where it runs, and a reason:

```scheme
(rule git-in-jj :when (ancestor-has? ".jj")
  (deny [git -... log ...]
    :reason  "this repo is managed by jj."
    :instead "use `jj log`."))
```

In a directory with a `.jj` above it, `git log --oneline` never executes, and the model reads:

```
claude-guard denied `git log --oneline`: this repo is managed by jj. Instead: use `jj log`.
```

Anywhere else, the same command passes untouched. The rule also holds for `sudo git log`, `ssh box 'git log'`, and `bash -c 'git log'`, because the guard elaborates commands: it knows which programs wrap other commands and which flags take values, so `git -C . log` is not a different command.

## Install

```
cargo install claude-guard
```

Or from a clone, `cargo install --path .`. Then register the hooks in `~/.claude/settings.json`. The guard reads every event and decides on `PreToolUse`; the others are recorded so the log is complete.

```json
{
  "hooks": {
    "PreToolUse":        [{ "matcher": "", "hooks": [{ "type": "command", "command": "claude-guard hook" }] }],
    "PermissionRequest": [{ "matcher": "", "hooks": [{ "type": "command", "command": "claude-guard hook" }] }],
    "PermissionDenied":  [{ "matcher": "", "hooks": [{ "type": "command", "command": "claude-guard hook" }] }],
    "PostToolUse":       [{ "matcher": "", "hooks": [{ "type": "command", "command": "claude-guard hook" }] }],
    "SessionStart":      [{ "matcher": "", "hooks": [{ "type": "command", "command": "claude-guard session-start" }] }],
    "SessionEnd":        [{ "matcher": "", "hooks": [{ "type": "command", "command": "claude-guard hook" }] }]
  }
}
```

Use the full path to the binary if `~/.cargo/bin` is not on the PATH Claude Code sees. Restart Claude Code, then:

```
claude-guard rules
```

prints the rules in force. The guard exits 0 whatever happens, so a bug in it never blocks a session.

## Documentation

- Tutorials: [getting started](docs/tutorials/getting-started.md) installs the guard, wires the hooks, and writes a first rule in one sitting; [a first external fact](docs/tutorials/a-first-external-fact.md) has a script of yours decide a rule.
- How-to guides: [write a rule](docs/how-to/write-a-rule.md), [write an external fact](docs/how-to/write-an-external-fact.md), [declare a command](docs/how-to/declare-a-command.md), [replace the built-in rules](docs/how-to/replace-the-builtin-rules.md), [review what the guard is doing](docs/how-to/review-the-guard.md).
- Reference: [the rule language](docs/reference/rule-language.md), [the command line](docs/reference/command-line.md), [hooks](docs/reference/hooks.md), [the log format](docs/reference/log-format.md).
- Explanation: [why a policy language](docs/explanation/why-a-policy-language.md), [external facts](docs/explanation/external-facts.md), [elaboration](docs/explanation/elaboration.md), the [design decision logs](docs/design/) that record how the design was reached, and the [architecture decision records](docs/adrs/) for choices made while building.

## What it does today

- Denies, asks, or warns on Bash commands and on the Write, Edit, MultiEdit, and Read tools, by rules in a file you own.
- Sees through wrappers and script strings: sudo, env, nice, nohup, timeout, xargs, ssh, bash, sh, nu, python3.
- Understands a program's options once declared, and generates declarations from [carapace](https://github.com/carapace-sh/carapace-bin) with `claude-guard commands add`.
- Asks programs of yours for facts: `(fact in-git-worktree? (exec "git" "rev-parse" "--is-inside-work-tree") ...)` in the rule file, one JSON object in and one out. A fact that times out or fails turns a deny into an ask that says which fact could not be settled. `claude-guard extern <fact>` asks one by hand.
- Logs one typed JSON record per hook call to `~/.local/state/claude-guard/sessions/<session>.jsonl`.
- Ships an executable spec: every matcher and elaborator behavior is a `check` line under `spec/`, run by the tests.

## What it does not do yet

- No review UI. The log is JSONL; `claude-guard commands` and `claude-guard elaborate --check-log` are the two readers so far.
- No memory of your dialog answers, and no escalation from deny to ask on a repeat.
- No session-lifetime facts: every declared fact runs on every call that asks it. History facts and cwd tracking are designed and not built.
- No pruning of old session files.

The [design decision logs](docs/design/) say what is planned and why.

## License

Licensed under either of the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or the MIT license ([LICENSE-MIT](LICENSE-MIT)), at your option.
