# Review what the guard is doing

The guard writes one JSON line per hook call and never talks to a running process, so reviewing it means reading files and running two subcommands over them.

## See which programs your sessions run

```
claude-guard commands
```

```
command         calls  generic-hits  flagged  status      carapace
rg                423             0      381  undeclared  yes
jj                102             0       51  undeclared  yes
cargo             174             0       21  undeclared  yes
sudo               12             0        0  built-in    yes
```

- `calls`: simple commands with that name, across every logged session.
- `generic-hits`: times a catch-all row like `[git ...]` fired although the rules have a more specific row for that program. That is what a flag value hiding a subcommand looks like from outside, and a declaration fixes it.
- `flagged`: calls on an undeclared program where a dash word was followed by a non-dash word, so a value may be passing as an argument or the reverse.
- `status`: `built-in`, `config`, or `undeclared`.
- `carapace`: whether carapace can generate a declaration. `?` when carapace is not installed.

The table is sorted by generic hits, then flagged calls, then calls. The top row is the next `claude-guard commands add`.

## Check that every logged command still reads correctly

```
claude-guard elaborate --check-log
```

Every Bash command in every session is segmented and elaborated under the declarations in force, and each one whose words do not come back whole is printed with its session and the two word lists. The last line is the count. Run it after adding a declaration.

## Read a session file

Sessions live at `~/.local/state/claude-guard/sessions/<session id>.jsonl`, one line per hook call. Useful queries with `jq`:

```
# every deny in a session, with the rule that fired
jq -c 'select(.outcome == "deny") | {ts, rule, pattern, reason}' <file>

# every command a session ran, in order
jq -r 'select(.event == "pre_tool_use" and .tool == "Bash") | .subject.bash.command' <file>

# calls where a pattern row bound something
jq -c 'select(.bindings != null and .bindings != {})' <file>
```

The fields are described in [the log format](../reference/log-format.md).

## See the guard's own diagnostics

Claude Code shows hook stderr under `claude --debug`. The guard writes one line there when a rule file fails to load or a log write fails, and tracing at the level `CLAUDE_GUARD_LOG` sets:

```
CLAUDE_GUARD_LOG=debug
```

Tracing is not the log. Nothing in a decision depends on it.
