# Getting started

By the end of this page the guard is running in your Claude Code sessions, you have watched it deny a command and read the log line it wrote, and you have added one rule and one command declaration of your own. Allow twenty minutes.

You need Rust and Claude Code. [carapace](https://github.com/carapace-sh/carapace-bin) is optional; the last step uses it.

## 1. Install the binary

```
cargo install claude-guard
```

From a clone of the repository, `cargo install --path .` does the same. Either puts `claude-guard` in `~/.cargo/bin`. Check it:

```
claude-guard rules
```

You should see:

```
built-in rules: 5 rules, 34 rows, 11 commands declared
```

The guard has no rules file of yours yet, so it is reporting the rules embedded in the binary.

## 2. Wire the hooks

Open `~/.claude/settings.json` and add a `hooks` block. If you already have one, add these entries to it.

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

Claude Code may not see your shell's PATH. If the guard does not fire in the next step, replace `claude-guard` with the full path, `/Users/you/.cargo/bin/claude-guard`.

Only `PreToolUse` produces decisions. The other five are recorded so the session log holds every event; nothing prints for them.

Restart Claude Code so it reads the settings.

## 3. Watch a deny

In a Claude Code session, ask for something the built-in rules refuse:

> run `grep -n main src/main.rs`

The command does not run. The model sees, and will usually repeat to you:

```
claude-guard denied `grep -n main src/main.rs`: grep is not the search tool here. Instead: use `rg`.
```

The model then reaches for `rg`, which passes. That is the whole interaction model: a deny carries a reason and an alternative, and the model reroutes.

## 4. Read the log line

Every hook call writes one JSON line to a file named after the session:

```
ls ~/.local/state/claude-guard/sessions/
```

Open the newest file and find the line with `"outcome":"deny"`. It carries the command as text, the command segmented into words, the elaborated form the matcher saw, the rule and the pattern row that fired, and the reason exactly as the model read it. A pass is a line too, with `"outcome":"pass"` and null rule and reason. Nothing that happened is missing from the log.

## 5. Add a rule of your own

Print the built-in rules as a file:

```
mkdir -p ~/.config/claude-guard
claude-guard rules --export > ~/.config/claude-guard/rules.scm
```

That file now replaces the built-in rules whole. Open it and add a rule at the end:

```scheme
(rule mine
  (warn [cargo -... clean ...]
    :reason "cargo clean throws away the whole build cache; a full rebuild follows."))
```

Check it:

```
claude-guard rules
```

It should now say your file's path and `6 rules, 35 rows`. A mistake prints the file, line, and column instead, and the guard fails open on that call until you fix it.

Ask Claude Code to run `cargo clean`. The command runs, and the model sees the note as context. A `deny` row would have stopped it; an `ask` row would have prompted you.

## 6. Declare a command

The guard matches `[git -... push ...]` against `git -C . push` only if it knows that `-C` takes a value. Declarations carry that knowledge, and carapace already has it for most programs:

```
claude-guard commands
```

prints a table of every program your sessions have run, sorted so the ones an elaborator would help most come first. Pick one carapace knows:

```
claude-guard commands add git
```

It writes `~/.config/claude-guard/commands/git.scm` and says how many options and subcommands it found. `claude-guard rules` now counts one more declared command. Run the round-trip check over your log to be sure nothing the guard has seen is misread under the new declaration:

```
claude-guard elaborate --check-log
```

The last line says how many commands were checked and that zero failed to round-trip.

## Where to go next

- [A first external fact](a-first-external-fact.md) has a script of yours answer a question a rule needs, in fifteen minutes.
- [Write a rule](../how-to/write-a-rule.md) covers patterns, binders, and conditions.
- [Declare a command](../how-to/declare-a-command.md) covers writing declarations by hand, including programs that wrap other commands.
- [The rule language](../reference/rule-language.md) is the complete grammar and its meaning on one page.
