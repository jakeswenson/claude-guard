# Decision Log: claude-guard decision log

Held 2026-09-04. The baseline principles were proposed before the
decisions below were made; each is marked at the end as confirmed,
amended, or still open.

## Problem

Every hook invocation writes one durable, typed record of what the guard
saw and decided. The record is written by a dedicated trait and writer,
never by tracing. Two readers: the escalation logic inside the hook, and a
`review` web UI that searches across sessions and follows one session
live. Success: from anywhere, in under a minute, the user can see why a
call was denied, passed, or asked, and can spot rules that should change.

## Decisions

- **D1** — The log is read through a third subcommand, `claude-guard
  review`, runnable from any directory. Nobody opens the sessions
  directory by hand.
- **D2** — `review` opens an embedded web UI rather than printing text: an
  axum server with a utoipa-described API and a SvelteKit or Astro SPA
  baked into the binary. Two uses: search across session logs, and follow
  one session as it runs.
- **D3** — The JSONL files stay the only source of truth. The hook never
  talks to a running process; the UI tails or watches the files.
- **D4** — Three kinds of navigation, not one search. Free text searches
  the command and the repo/session space. Rule names are known facets,
  shown as filters or buttons. Time is the axis of a timeline view over
  the event stream, not a search term.
- **D5** — Repo attribution uses the cwd Claude Code reports for the call,
  nothing more. A `cd` inside a command does not move the event. Deferred:
  telling "commands run in a folder" apart from the session root.
- **D6** — A manual decision from the dialog is stored with time decay. A
  repeat right after the user's answer gets that answer applied: a user
  deny becomes a deny, with no dialog. As time passes and the session
  moves on, the stored answer expires and the rule returns to deny-first,
  then ask. Depends on the guard learning what the user clicked; see
  Constraints.
- **D7** — Passes are full records, same shape as denies. The UI folds them
  by default so denies stand out, but they exist because passes and asks
  get reviewed now and then to find rules that should change. Confirms A1.
- **D8** — A deny record names the pattern row that fired, as pattern text,
  next to the rule name. Generic-row hits are the candidates for new rows.
- **D9** — A manual dialog answer applies for 5 minutes and never crosses a
  session boundary. Deferred: a per-project (cwd-keyed) memory that would
  outlive the session.
- **D10** — Commands are logged verbatim, secrets included. Mitigation is
  retention, not redaction: session files older than a week are deleted.
  Who deletes them is open.

## Constraints Surfaced

- Tracing never writes the log. A well-defined trait and writer that
  follow the principles below do, and that comes before any log code.
- Everything typed. The record is one Rust type with serde on it, both
  directions.
- Spec: path `~/.local/state/claude-guard/sessions/<session_id>.jsonl`, exit
  code always 0, log failure goes to stderr and the decision still prints.
- Claude Code hook events, checked against
  https://code.claude.com/docs/en/hooks-guide.md on 2026-09-04: after
  PreToolUse returns `ask`, a `PermissionRequest` hook fires and can decide
  with `hookSpecificOutput.decision.behavior` (allow/deny). Then either
  `PostToolUse` (tool ran) or `PermissionDenied` (denied) fires. So D6
  requires the guard to register on more events than PreToolUse and write
  records from them. The input schema of `PermissionDenied` is not
  documented, so "user clicked deny" versus "denied for another reason" is
  unconfirmed. Whether subagent calls share the parent `session_id` is not
  documented; subagent transcripts live under
  `<sessionId>/subagents/agent-<agentId>.jsonl`. Whether the transcript
  records dialog answers is not documented.

- Live capture, 2026-09-05, closes the riskiest assumption the wrong way:
  `PermissionDenied` does not fire when the user clicks deny in the
  dialog. Only `PermissionRequest` fires, and it has no `tool_use_id`. The
  click is recorded in Claude Code's transcript at `transcript_path`, as a
  user line whose `tool_result` has `is_error: true` and whose
  `toolUseResult` is "User rejected tool use", keyed by `tool_use_id`. So D6
  and D9 are feasible by reading the transcript for the tool use ids in the
  guard's own records. The transcript format is undocumented; the reader
  must parse leniently and fail open. (claude-guard-7dn closed,
  claude-guard-2mb carries the design.)

## Tradeoffs Chosen

- Chose files-as-truth with a watching UI over a daemon the hook reports to,
  because the hook stays a short-lived process with no peer to fail against
  (D3).
- Chose full pass records over a pass counter, because rule review needs the
  passes, and the UI can fold them (D7).
- Chose retention over redaction for secrets in commands, because the
  verbatim command is what escalation compares and what review reads (D10).
- Chose a session-scoped 5 minute memory for dialog answers over a project
  memory, because "doing much different things" is the signal that the
  answer is stale (D9).

## Assumptions

Baseline principles proposed before the decisions, with their status
after them:

- A1. One record per invocation, including passes. **Confirmed** by D7.
- A2. The record stands alone: timestamp, session, tool use id, agent id,
  cwd, tool, subject text, rule name, decision kind, rendered reason.
  **Amended** by D8: plus the pattern row that fired.
- A3. Typed in, typed out: one serde struct both ways, schema version field
  from line one. **Unchallenged**; carried forward.
- A4. Append only, one file per session, one write call per line, no
  pruning inside the hook. **Partly open**: D10 wants week-old files gone
  and has not said who deletes them.
- A5. Log before print; a log failure changes nothing about the decision or
  exit code. **Unchallenged**; matches the spec.
- A6. Writer is a trait with a file implementation and an in-memory one for
  tests. **Unchallenged**; carried forward.
- A7. The reader lives next to the writer; escalation asks the log module.
  **Amended** by D2: the `review` UI is a second reader, and D6 adds records
  from events other than PreToolUse.
- A8. The log is not the diagnostics; tracing stays on stderr. **Confirmed**
  by the standing constraint.

Riskiest assumption: that a `PermissionDenied` hook can tell the guard what
the user clicked. D6 and D9 fall apart without it, and only a live payload
capture settles it.

## Open Questions

- Do subagents share the parent's session id? Needs a live payload check.
  (claude-guard-6o4)
- What does the `PermissionDenied` input carry, and does it distinguish the
  user's click from other denials? (claude-guard-7dn)
- Who deletes week-old session files: the hook, `review`, or a separate
  subcommand? (claude-guard-c8n)
- Is the verbatim command capped in length for pathological heredocs?
  (claude-guard-a0v)
- Deferred (D5): attribute commands to a folder versus the session root.
  (claude-guard-9jh)
- Deferred (D9): per-project memory of dialog answers that outlives a
  session. (claude-guard-033)
