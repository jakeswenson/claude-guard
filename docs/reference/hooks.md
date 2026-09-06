# Hooks

The guard registers on six Claude Code hook events. One decides; five are recorded so the session log holds every event.

| Event | Subcommand | What the guard does |
|---|---|---|
| `PreToolUse` | `hook` | Evaluates the rules. Prints a decision when a row fires. Writes a record either way. |
| `PermissionRequest` | `hook` | Records the payload. Fires after a `PreToolUse` answer of `ask`. Carries no `tool_use_id`. |
| `PermissionDenied` | `hook` | Records the payload. Does not fire when the user clicks deny in the dialog; only for other denials. |
| `PostToolUse` | `hook` | Records the payload, including `duration_ms` and the tool's response with long strings reduced to sizes. |
| `SessionStart` | `session-start` | Records the payload. |
| `SessionEnd` | `hook` | Records the payload, including the end reason. |

## Settings

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

An empty matcher runs the hook for every tool. Use an absolute path for the command if Claude Code does not see `~/.cargo/bin`.

## Input

The guard reads `session_id`, `cwd`, `hook_event_name`, `tool_name`, `tool_input`, `tool_use_id`, and `agent_id` when present. For Bash the command is `tool_input.command`; for Write, Edit, MultiEdit, and Read the path is `tool_input.file_path`. An MCP tool arrives as `mcp__<server>__<tool>` and is recorded with its server, tool, and input. A payload that does not parse, or an event the guard does not know, is one stderr line and no decision.

## Output

Only `PreToolUse` prints, and only when a row fires:

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"claude-guard denied `git checkout main`: ..."}}
```

`deny` stops the call; the model reads the reason. `ask` sends the call to your permission dialog with the reason shown to you. A `warn` row prints `additionalContext` instead of a decision; the call runs and the model reads the text.

The guard never prints `allow`. A pass is silence, and Claude Code's own permission rules apply.

## Learning what the user clicked

`PermissionDenied` does not fire for a click in the dialog. The click is recorded in Claude Code's transcript at the `transcript_path` the payload names, as a `tool_result` with `is_error: true` for the `tool_use_id` of the call. The guard records the ids it needs to look that up; reading the transcript is designed and not built.
