# The log format

One file per session at `<state dir>/sessions/<session id>.jsonl`, one JSON object per line, appended with one write per line so lines from parallel agents never interleave. The log is written before the decision is printed, and a log failure never changes the decision. Old lines are never rewritten; a reader must accept fields it does not know and skip lines it cannot parse.

## Record

```json
{
  "v": 1,
  "ts": "2026-09-06T14:03:11.128Z",
  "event": "pre_tool_use",
  "session_id": "4e118089-...",
  "tool_use_id": "toolu_01...",
  "agent_id": null,
  "cwd": "/Users/me/proj",
  "tool": "Bash",
  "subject": { "bash": { ... } },
  "outcome": "deny",
  "rule": "hard-denies",
  "pattern": "[git -... checkout ...]",
  "bindings": {},
  "reason": "claude-guard denied `git checkout main`: ..."
}
```

| Field | Type | Meaning |
|---|---|---|
| `v` | integer | Schema version. `1`. A reader that meets a version it does not know skips the line. |
| `ts` | RFC 3339 timestamp, UTC | When the record was made. |
| `event` | string | `pre_tool_use`, `permission_request`, `permission_denied`, `post_tool_use`, `session_start`, `session_end`. |
| `session_id` | string | The Claude Code session. Also the file name. |
| `tool_use_id` | string or null | The tool call. Null on events without one. |
| `agent_id` | string or null | The subagent making the call, when there is one. |
| `cwd` | string | The working directory Claude Code reported for the call. |
| `tool` | string or null | The tool name as Claude Code spells it. |
| `subject` | object | What the guard looked at; see below. |
| `outcome` | string | `pass`, `deny`, `ask`, `warn`, `observed`. |
| `rule` | string or null | The rule that fired, or `parse-error` and `uninspected` for the engine's own answers. |
| `pattern` | string or null | The row's subject as written in the file. Null for the engine's own answers. |
| `bindings` | object or null | What the row's binders captured, name to word. Null when no row fired. Absent on lines written before it existed. |
| `reason` | string or null | The text the model or the user saw, exactly as rendered. |

`outcome` is `observed` for every event other than `PreToolUse`, and for a `PreToolUse` call made while the rule file failed to load.

## Subject

One of six shapes, tagged by key.

`bash`, for a Bash call:

```json
{ "bash": {
  "command": "git -C . checkout main",
  "commands": [ { "words": [ {"literal": "git"}, {"literal": "-C"}, {"literal": "."}, ... ], "redirects": [] } ],
  "elaborated": [ { "parts": [ {"name": {"literal": "git"}}, {"option": {"text": {"literal": "-C"}, "flags": ["C"], "value": {"text": {"literal": "."}, "attached": false}}}, {"arg": {"literal": "checkout"}}, {"arg": {"literal": "main"}} ],
                   "declared": true, "subcommand": ["checkout"], "inner": null, "redirects": [] } ],
  "uninspected": [],
  "parse_error": null
} }
```

- `command`: the verbatim text.
- `commands`: every simple command the segmenter found, in source order, as words and redirects. A word is `{"literal": text}` after quote removal or `{"dynamic": raw}` when the shell would expand it.
- `elaborated`: each command as the matcher saw it. `parts` is the partition of the words into `name`, `option`, and `arg`; an option carries its `text` as written, the `flags` derived from it, and its `value` with whether it was attached. `declared` says whether a declaration was in force. `subcommand` is the path the arguments named. `inner` is `{"command": {...}}` or `{"script": "text"}` for a wrapper, else null. Absent on lines written before elaboration existed.
- `uninspected`: the text of every `$(...)` and backquote substitution.
- `parse_error`: the parser's message when it refused the command, in which case `commands` is empty.

`path`, a string, for Write, Edit, MultiEdit, and Read. `url`, a string, for WebFetch. `search`, `{"pattern", "path"}`, for Glob and Grep. `mcp`, `{"server", "tool", "input"}`, for an MCP tool. `raw`, the whole tool input, for any other tool and for every observed event's payload.

In an observed event's `raw` payload, every string longer than 256 bytes is replaced by `{"bytes": n, "lines": n}`. Tool output is content, and content is what bloats a log and carries secrets; the shape is what these records exist to keep.

## Retention

Nothing prunes the directory today. The design sets retention at one week and leaves the deleting to a component not yet chosen; commands are logged verbatim, secrets included, and retention rather than redaction is the mitigation.
