# claude-guard design

A Rust binary that runs as a Claude Code PreToolUse hook. It denies tool calls that break house rules, tells the model why, and escalates repeated attempts to a user prompt. Every decision lands in an append-only per-session log that a later UI can query.

Status: approved design, not yet implemented. Date: 2026-09-01.

## Goals

- Redirect Claude away from git in jj repos, from sed and grep, and from /tmp, with a one-line reason the model reads.
- Let the model override a redirect once by retrying the exact command, at which point the user decides.
- Never block a session by accident. A bug in the guard must fail open.
- Record every decision so session stats and a UI can be built later without changing the hook.

## Non-goals

- Prompt-based hooks, PostToolUse, or Stop hooks.
- Rewriting commands with `updatedInput`. Deny plus reason is safer than guessing flags.
- A daemon, a web UI, or any long-lived process. Those are v2 and are shaped in the last section.
- A config file. Rules and reason strings are compiled in for v1.

## Placement and installation

- Repo: `~/code/personal/jakes/claude-guard`, colocated jj and git.
- Crate name and binary name: `claude-guard`.
- Install: `cargo install --path .`, which puts the binary at `~/.cargo/bin/claude-guard`. The chezmoi cargo setup script gains one line to install it on a new machine.
- Chezmoi carries only the hook wiring in `private_dot_claude/private_settings.json.overrides.cue`.

## Subcommands

| Subcommand | Hook event | Reads | Writes |
|---|---|---|---|
| `claude-guard hook` | PreToolUse | hook JSON on stdin | decision JSON on stdout, one log record |
| `claude-guard session-start` | SessionStart | hook JSON on stdin | a rules brief on stdout, prunes old logs |

`session-end` was considered and dropped. The log is the record, so there is nothing to clean up per session.

## Hook contract

Input on stdin is the PreToolUse JSON. Fields used: `session_id`, `cwd`, `tool_name`, `tool_input`, `tool_use_id`. For Bash the command is `tool_input.command`. For Write, Edit, and MultiEdit the path is `tool_input.file_path`.

Output on stdout, only when a rule speaks:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "This is a jj repo. Use `jj log`. Re-run the exact same command to be prompted."
  }
}
```

No rule speaking means no stdout. Claude Code then applies the normal permission rules from settings.

Exit code is always 0. The guard never exits 2. A panic or parse error prints one line to stderr and exits 0, so the tool call proceeds. Hook timeout in settings is 5 seconds.

What each decision does, from the Claude Code docs:

- `deny`: the reason is shown to the model. The tool call does not run.
- `ask`: the reason is shown to the user in the permission dialog. The model does not see it.
- `allow`: skips the permission prompt. The guard does not emit `allow` in v1.

## Rule engine

Rules are a fixed ordered list. Each rule takes the parsed input and returns one of: no opinion, deny with reason, ask with reason. The first rule with an opinion wins. Hard denies sit before the escalating rule so a repeat of `git stash` never reaches ask.

Bash commands are split into segments before matching. Separators: `&&`, `||`, `;`, `|`, newlines, and the inside of `$(...)` and backticks. Each segment is word-split with shell quoting respected. A rule matches if any segment matches. So `ls && git stash` trips the stash rule.

The segmenter is hand-written and small. It is not a shell parser. Known gaps are listed as tests marked ignored: nested `$(...)` inside quotes, heredoc bodies that contain separators, and `eval` strings. A full parser is a later step if the gaps bite.

### Rule 1: hard denies

Always deny. Reasons name the replacement.

| Match | Reason |
|---|---|
| `git worktree *` | Use `jj workspace`, and ask first. |
| `git stash *` | Use `jj new` or `jj describe`; jj has no dirty tree. |
| `git checkout *` | Use `jj edit` or `jj new <rev>`. |
| `sed *` | Use `sd` for replacements, `rg` for searching. |
| `chezmoi apply *` | Never from a session. Show the diff and let the user apply. |
| `cat` segment that has a `>` or `>>` redirect | Write files with the Write tool, not cat. |

### Rule 2: git inside a jj repo

Applies when walking up from `cwd` finds a `.jj` directory. Segments starting with `jj git` are exempt. Any other segment whose first word is `git` and is not covered by rule 1:

- First occurrence in this session: deny. Reason states this is a jj repo, gives the jj equivalent for the subcommand when the table knows one, and says that re-running the exact same command will prompt the user.
- Repeat of the exact same command string in this session: ask. Reason shown to the user is "Claude retried after a jj-repo deny:" followed by the command.

"Same command" means byte-equal `tool_input.command`. The model can dodge by changing a flag, and then gets a fresh deny. The reason text tells it a retry escalates, which is what keeps the loop short.

The subcommand table starts with: log, status, diff, show, blame, add, commit, push, pull, fetch, rebase, branch. Unknown subcommands get a generic reason.

### Rule 3: tool nudges

Deny with a reason pointing at the replacement.

| Match | Replacement |
|---|---|
| `grep` | `rg` |
| `find` | `fd` |

Open item: any further pairs. The table is one Rust array so adding a row is one line.

The chezmoi permissions `allow` list currently contains `Bash(grep *)` and `Bash(find *)`. Those entries are removed when this rule lands, since a hook deny wins over an allow rule anyway and leaving them is confusing.

### Rule 4: /tmp writes

Paths are normalized so `/tmp/x` and `/private/tmp/x` are the same.

- Write, Edit, MultiEdit: deny when `file_path` is under /tmp. Reason: no temp files, use tests or examples, or a path inside the project.
- Bash: deny when a segment writes under /tmp. Detected forms: a `>` or `>>` redirect target, an argument to `tee`, any `mktemp` invocation, and the last argument of `cp` or `mv`.

Open item, deferred by the user: whether the Claude Code scratchpad under `/private/tmp/claude-<uid>/` is exempt. The rule has one constant, `EXEMPT_SCRATCHPAD`, that turns the exemption on or off. Tests cover both values. The shipped value is set when the user decides, and the implementation plan carries that as a blocking question for this rule only.

## Decision log

Every hook invocation appends one JSON line to `~/.local/state/claude-guard/sessions/<session_id>.jsonl`, including the ones where no rule spoke. Fields:

```json
{"ts":"2026-09-01T17:04:12Z","session_id":"...","tool_use_id":"...","cwd":"/Users/jakes/code/x","tool":"Bash","subject":"git log --oneline","rule":"git-in-jj","decision":"deny"}
```

`subject` is the command for Bash and the path for file tools. `rule` is null and `decision` is `"pass"` when nothing matched.

Write discipline: open with `O_APPEND` and create, serialize the record, write it with one `write` call, close. Records stay well under 4 KB. The kernel serializes the append offset across processes, and a single small write to a local filesystem lands whole. This does not hold on network filesystems, which is accepted.

The escalation state for rule 2 is derived from this file: the set of `subject` values with `rule == "git-in-jj"` and `decision == "deny"` for the current session. There is no separate state file and no read-modify-write.

Readers skip lines that fail to parse. A crash mid-write costs at most one record.

Pruning runs in `session-start` and deletes session files older than 90 days. Open item: whether to prune at all, since the files are small.

## SessionStart brief

`claude-guard session-start` prints a short brief to stdout, which Claude Code adds to the model's context. Content:

- Whether `cwd` is a jj repo, and if so, "use jj, not git; git is denied once and prompts on retry."
- The tool nudges: rg, sd, fd.
- No writes under /tmp.

The brief is generated from the same rule table that produces deny reasons, so the two cannot drift. SessionStart fires on startup, resume, clear, and compact, so the brief survives compaction.

## Chezmoi wiring

Add to `private_settings.json.overrides.cue`:

```cue
_guard_pre_tool_use: {
	matcher: "Bash|Write|Edit|MultiEdit"
	hooks: [{
		type:    "command"
		command: "\(#cz.chezmoi.homeDir)/.cargo/bin/claude-guard hook"
		timeout: 5
	}]
}
_guard_session_start: {
	matcher: ""
	hooks: [{
		type:    "command"
		command: "\(#cz.chezmoi.homeDir)/.cargo/bin/claude-guard session-start"
		timeout: 5
	}]
}
```

CUE does not merge lists, and two definitions of the same list conflict. So each list is defined exactly once, inside a branch. The `!#cz.is_work` branch sets `hooks.PreToolUse: [_guard_pre_tool_use]`. The `_work_config` list becomes `[_guard_pre_tool_use, <devbar>, <aisuite>]`. Same shape for SessionStart. If a future edit defines a list twice, `cue export` fails loudly rather than dropping a hook.

Hooks load at session start. Testing a change means exiting and restarting `claude`.

## Error handling

- Malformed stdin: log one line to stderr, exit 0, no stdout.
- Log directory missing: create it. Log write failure: stderr, still emit the decision.
- Unknown tool name: no opinion, still logged.
- Every `Result` in the hook path ends in `main` with a catch that prints and exits 0. `std::panic::set_hook` does the same for panics.

## Testing

- Unit tests per rule with fixture inputs. Each rule file owns its tests.
- Segmenter tests: a table of command strings to expected segments, including the known-gap cases marked ignored.
- Escalation test: two hook invocations with the same session id and command against a temporary state directory, asserting deny then ask.
- Integration test: build the binary, pipe fixture JSON through it, assert exact stdout.
- Runner: `cargo nextest run`.
- Live check: `claude --debug` after restart. First live run also verifies whether subagents share the parent session id, which decides whether escalation spans them. The design assumes they do.

The state directory is overridable with `CLAUDE_GUARD_STATE_DIR` so tests never touch the real log.

## Module layout

Flat modules, no `mod.rs`.

| File | Owns |
|---|---|
| `main.rs` | subcommand dispatch, fail-open wrapper |
| `input.rs` | hook input types |
| `output.rs` | decision type and JSON output |
| `segment.rs` | bash command segmenter |
| `repo.rs` | jj repo detection |
| `log.rs` | append-only log, session reader |
| `rules.rs` | `Rule` trait, ordered list, first-opinion-wins |
| `rules_hard.rs` | rule 1 |
| `rules_git.rs` | rule 2 and the jj equivalents table |
| `rules_tools.rs` | rule 3 |
| `rules_tmp.rs` | rule 4 |
| `brief.rs` | SessionStart brief from the rule tables |

## v2 notes

Not in scope. Recorded so v1 does not close doors.

- `claude-guard ui`: an on-demand command that serves a localhost page over the log and exits when closed. Never a hook-spawned daemon: hooks are parallel, timeout-bound, and must not depend on anything running. If always-on is wanted later, a launchd agent managed by chezmoi is the right home.
- Queries run with DuckDB over the glob `sessions/*.jsonl`, no import step. Per-session files give a one-session view by filename and an all-sessions view by glob.
- If years of logs make that slow, a compaction job turns closed sessions into Parquet. DuckDB reads both side by side. Hooks never write Parquet; it is not appendable.
- A per-repo allowlist of blessed git commands, if per-session escalation turns out to nag.
- A config file, if the compiled-in tables change often.

## Open items

1. Scratchpad exemption for the /tmp rule. Deferred by the user.
2. Tool-nudge pairs beyond grep and find.
3. Whether to prune session logs at all.
