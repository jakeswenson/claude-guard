# The command line

```
claude-guard hook                    hook handler for every event; reads hook JSON on stdin
claude-guard session-start           SessionStart handler; reads hook JSON on stdin
claude-guard rules                   check the rules in force and say where they came from
claude-guard rules --export          print the built-in rule file
claude-guard commands                which programs the sessions run, and what the guard knows
claude-guard commands add [--force] [--stdout] <name>...
                                     write a declaration for each program from carapace
claude-guard elaborate --check-log   round-trip every logged command through the elaborator
```

## Exit code

Always 0. The guard never blocks a session by exiting non-zero. A failure of any kind is one line on stderr, prefixed `claude-guard: failed open:`, and the tool call proceeds.

## Subcommands

### `hook`

Reads one hook payload from stdin. On `PreToolUse` it evaluates the rules and prints a decision to stdout when a row fires; otherwise stdout stays empty and Claude Code applies its own permission rules. On every other event it prints nothing. Every call writes one record to the session log before printing.

A rule file that does not load prints every problem to stderr, records the call as observed, and prints no decision.

### `session-start`

Reads the SessionStart payload and records it. Prints nothing today; the design reserves this for a rules brief.

### `rules`

Loads everything in force and prints one line: the source, rule count, row count, and declared command count. A load failure prints every problem, one per line, as `source:line:col: message`, and nothing to stdout. `--export` prints the built-in rule file verbatim.

### `commands`

Reads every session log and prints a table with one row per program name: `calls`, `generic-hits`, `flagged`, `status`, `carapace`, sorted by generic hits, then flagged, then calls. See [review what the guard is doing](../how-to/review-the-guard.md) for the columns.

### `commands add`

For each name, runs `carapace <name> export`, converts the result to a `(command ...)` declaration, checks that it loads, and writes it to the commands directory as `<name>.scm`. An existing file is skipped unless `--force` is given. `--stdout` prints the declaration instead of writing it. A program carapace does not know is reported and skipped. Results go to stderr, one line per name.

### `elaborate --check-log`

Segments and elaborates every Bash command in every session log under the declarations in force and prints each one whose words do not come back whole, then a summary line.

## Environment

| Variable | Meaning |
|---|---|
| `CLAUDE_GUARD_RULES` | Path of the rules file to use. Missing is an error, not a fallback. |
| `CLAUDE_GUARD_COMMANDS_DIR` | Directory of declaration files to merge in, and where `commands add` writes. |
| `CLAUDE_GUARD_STATE_DIR` | Where session logs go, under `sessions/`. |
| `CLAUDE_GUARD_CARAPACE` | The carapace binary to run. Default `carapace` on `PATH`. |
| `CLAUDE_GUARD_LOG` | Tracing filter for stderr diagnostics, such as `debug` or `claude_guard=trace`. Default `warn`. |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `HOME` | Used for the defaults below. |

## Files

Rules, first match wins:

1. `$CLAUDE_GUARD_RULES`
2. `$XDG_CONFIG_HOME/claude-guard/rules.scm`, else `~/.config/claude-guard/rules.scm`, when the file exists
3. the file embedded in the binary

A user rules file replaces the embedded one whole.

Command declarations merge, later winning by name:

1. the declarations embedded in the binary
2. `(command ...)` forms in the rules file in force
3. every `.scm` file in `$CLAUDE_GUARD_COMMANDS_DIR`, else `<config>/claude-guard/commands/`, in name order

Session logs: `$CLAUDE_GUARD_STATE_DIR`, else `$XDG_STATE_HOME/claude-guard`, else `~/.local/state/claude-guard`, then `sessions/<session id>.jsonl`.
