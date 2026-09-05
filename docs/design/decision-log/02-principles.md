# Principles: claude-guard decision log

Accepted 2026-09-05. Distilled from `01-decisions.md`. The trait, writers,
and readers in `log.rs` must obey every line here.

1. **Every call leaves a record.** Pass, deny, ask, warn, parse error. No
   record means the guard was never called.
2. **A record explains itself.** Time, session, tool use id, agent id, cwd,
   tool, the subject verbatim, the rule, the pattern row, the decision, and
   the reason text as rendered. No lookup is needed to read it.
3. **One struct, both directions.** Serde on a single Rust type. The writer
   serializes it, every reader deserializes it. A schema version is on every
   line from day one. Readers skip lines they cannot parse rather than
   failing, so one torn line never hides a session.
4. **Files are the truth.** One JSONL file per session under
   `~/.local/state/claude-guard/sessions`. Append only, one write call per
   line so parallel agents never split a line. The hook never talks to a
   running process.
5. **The log never changes the decision.** Written before stdout. A write
   failure is one stderr line, then the decision prints and the exit code
   stays zero.
6. **Writers and readers are traits.** A file writer for the binary, an
   in-memory one for tests. The session reader that escalation uses is a
   trait too, so the 5 minute memory is testable without a clock or a disk.
7. **Tracing is not the log.** Nothing in the record depends on the tracing
   filter, and nothing in tracing is needed to reconstruct a decision.
8. **Retention, not redaction.** Commands are stored verbatim. Session files
   older than a week are deleted, by a component still to be chosen
   (claude-guard-c8n).
