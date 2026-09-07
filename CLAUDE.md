# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->


## Build & Test

```bash
cargo build
cargo nextest run              # unit, integration, and the executable spec under spec/
cargo clippy --all-targets     # must be clean
cargo fmt                      # rustfmt config: 2 spaces, vertical fn params
cargo install --path .         # a dev build into ~/.cargo/bin; users get `cargo install claude-guard`
```

Tests never touch the real state or config directories: set `CLAUDE_GUARD_STATE_DIR`, `CLAUDE_GUARD_RULES`, and `CLAUDE_GUARD_COMMANDS_DIR` to temp dirs, as `tests/cli.rs` does. `spec/*.scm` holds `check` forms run by `spec::tests::the_spec_passes`; every matcher, condition, and elaborator behavior has a line there. `tests/fixtures/commands.jsonl` is a corpus of real logged commands for the elaborator's round-trip test.

## Architecture Overview

One binary, flat modules, no `mod.rs`. A hook call flows top to bottom:

- `main.rs`: subcommand dispatch, the fail-open wrapper (exit 0 always, one stderr line on failure), tracing setup.
- `input.rs`: the hook payload as typed structs; the `string_id!` newtypes (`SessionId`, `ToolUseId`, `RuleName`, ...); tool inputs as a `Tool` enum.
- `segment.rs`: a Bash command to simple commands via brush-parser: words after quote removal, literal or dynamic, plus file redirects; `$(...)` text is reported as uninspected.
- `elaborate.rs`: a simple command classified under a declaration: name, options with values, args, subcommand path, inner command or script. A partition of the words; `flatten` returns the input.
- `sexp.rs`, `syntax.rs`, `cond.rs`, `load.rs`: the rule language. Reader with positions; forms to a typed table (`rule`, rows, patterns, `command` declarations); conditions with three-valued evaluation and reasons; which files load and how declarations merge.
- `facts.rs` and `facts/*.rs`: every fact a condition can name, one type per fact behind the `Fact` trait, in a `Facts` registry the conditions consult by name. Built-ins are `ancestor_has.rs` and `under.rs`; `stubs.rs` stands in by name for the spec and the tests, so nothing there touches the disk. Extern facts will be one more type here.
- `pattern.rs`: the matcher over elaborated units. Tokens keep their meaning; a declaration changes what they see.
- `rules.rs`: the engine. Parse error asks; rules in order, first row wins, through inner commands to depth 8; uninspected substitutions warn.
- `output.rs`: the Claude Code wire format for deny, ask, and warn.
- `log.rs`: one typed JSONL record per hook call, schema v1, written before stdout, never changing the decision.
- `commands.rs`: the `commands` survey over the logs and `commands add` from carapace.
- `spec.rs`: the `check` runner.

Conditions are three-valued: true, false, unknown, combined by Kleene's tables (false AND unknown is false, true OR unknown is true, NOT leaves unknown alone, everything else with an unknown stays unknown). Every answer can carry a reason; an unknown always does. Nothing in the binary produces unknown yet; extern facts and cwd tracking will, and the design turns unknown on a matched pattern into an ask (D14, ADR 0001). `docs/explanation/why-a-policy-language.md` has the tables and the reasons.

Design decisions and their reasons are in `docs/design/*/01-decisions.md`, in the user's words, with the principles distilled next to them. Read those before changing the engine or the language. Choices made while building that need review on their own go in `docs/adrs/NNNN-title.md`: status and date, decision first, then context, options, and consequences. Each ADR names the decision-log entries it extends.

## Conventions & Patterns

- Plan, then implement. Each step is proposed with numbered decisions and a recommendation, reviewed, then built in full with tests. Nothing is left as a stub for the user to fill in.
- A decision made while implementing is written into the decision log the same day, with its reasons.
- Every behavior of the matcher, conditions, and elaborator gets a `check` line in `spec/` as well as a Rust test. A behavior without a check line does not exist.
- Newtypes over strings via `string_id!`; no bare `String` for an id or a name that crosses a module boundary.
- Errors carry positions. Anything read from a file reports `source:line:col: message`, and one load reports every problem.
- The guard fails open. Any failure is one stderr line and exit 0; the decision log is written before stdout and never changes the decision.
- Reasons in rule files name the danger, not the workflow: "git checkout overwrites working files", with the alternative in `:instead`.
- Conventional Commits, `jj` not `git`, no attribution trailers in commits.
