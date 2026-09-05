//! Integration tests for the process-level contract: the guard never exits
//! non-zero and never writes a decision to stdout when it has nothing to say.

use assert_cmd::Command;
use predicates::prelude::*;

fn guard() -> Command {
  Command::cargo_bin("claude-guard").expect("binary builds")
}

#[test]
fn unknown_subcommand_exits_zero_with_usage_on_stderr() {
  guard()
    .arg("definitely-not-a-subcommand")
    .write_stdin("")
    .assert()
    .code(0)
    .stdout("")
    .stderr(predicate::str::is_empty().not());
}

#[test]
fn no_subcommand_exits_zero() {
  guard().write_stdin("").assert().code(0).stdout("");
}

#[test]
fn hook_with_garbage_stdin_exits_zero_and_stays_silent() {
  guard()
    .arg("hook")
    .write_stdin("this is not json {{{")
    .assert()
    .code(0)
    .stdout("");
}

/// A PreToolUse payload as Claude Code sends it, with `command` swapped in.
fn bash_call(command: &str) -> String {
  format!(
    r#"{{
      "session_id": "abc123",
      "prompt_id": "550e8400-e29b-41d4-a716-446655440000",
      "transcript_path": "/Users/x/.claude/projects/p/transcript.jsonl",
      "cwd": "/Users/x/code/proj",
      "permission_mode": "default",
      "hook_event_name": "PreToolUse",
      "tool_name": "Bash",
      "tool_input": {{
        "command": "{command}",
        "description": "run a command",
        "timeout": 120000,
        "run_in_background": false
      }},
      "tool_use_id": "toolu_01ABC123"
    }}"#
  )
}

#[test]
fn hook_stays_silent_when_no_rule_matches() {
  guard()
    .arg("hook")
    .write_stdin(bash_call("cargo nextest run"))
    .assert()
    .code(0)
    .stdout("")
    .stderr("");
}

#[test]
fn hook_denies_git_stash_with_the_wire_format() {
  guard()
    .arg("hook")
    .write_stdin(bash_call("git stash"))
    .assert()
    .code(0)
    .stdout(
      "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\
       \"permissionDecisionReason\":\"claude-guard denied `git stash`: jj has no dirty tree, so there \
       is nothing to stash. Instead: use `jj new` to park the current change or `jj describe` to \
       name it.\"}}\n",
    )
    .stderr("");
}

#[test]
fn session_start_with_garbage_stdin_exits_zero() {
  guard()
    .arg("session-start")
    .write_stdin("this is not json {{{")
    .assert()
    .code(0);
}
