//! Integration tests for the process-level contract: the guard never exits
//! non-zero and never writes a decision to stdout when it has nothing to say.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn guard() -> Command {
  Command::cargo_bin("claude-guard").expect("binary builds")
}

/// A guard whose decision log lands under `state`, never under `$HOME`.
fn guard_logging_to(state: &Path) -> Command {
  let mut command = guard();
  command.env("CLAUDE_GUARD_STATE_DIR", state);
  command
}

fn state_dir() -> TempDir {
  tempfile::tempdir().expect("temp dir")
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
  let state = state_dir();
  guard_logging_to(state.path())
    .arg("hook")
    .write_stdin(bash_call("cargo nextest run"))
    .assert()
    .code(0)
    .stdout("")
    .stderr("");
}

#[test]
fn hook_denies_git_stash_with_the_wire_format() {
  let state = state_dir();
  guard_logging_to(state.path())
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
fn hook_writes_one_log_line_per_call_including_passes() {
  let state = state_dir();
  for command in ["git stash", "cargo build"] {
    guard_logging_to(state.path())
      .arg("hook")
      .write_stdin(bash_call(command))
      .assert()
      .code(0);
  }

  let log = fs::read_to_string(state.path().join("sessions").join("abc123.jsonl")).unwrap();
  let lines: Vec<serde_json::Value> = log
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect();
  assert_eq!(lines.len(), 2, "{log}");

  let deny = &lines[0];
  assert_eq!(deny["v"], 1);
  assert_eq!(deny["event"], "pre_tool_use");
  assert_eq!(deny["session_id"], "abc123");
  assert_eq!(deny["tool_use_id"], "toolu_01ABC123");
  assert_eq!(deny["agent_id"], serde_json::Value::Null);
  assert_eq!(deny["cwd"], "/Users/x/code/proj");
  assert_eq!(deny["tool"], "Bash");
  assert_eq!(deny["subject"]["bash"]["command"], "git stash");
  assert_eq!(
    deny["subject"]["bash"]["commands"],
    serde_json::json!([{"words":[{"literal":"git"},{"literal":"stash"}],"redirects":[]}])
  );
  assert_eq!(deny["outcome"], "deny");
  assert_eq!(deny["rule"], "hard-denies");
  assert_eq!(deny["pattern"], "git -... stash ...");
  assert!(
    deny["reason"]
      .as_str()
      .unwrap()
      .starts_with("claude-guard denied `git stash`:")
  );
  assert!(deny["ts"].as_str().unwrap().ends_with('Z'));

  let pass = &lines[1];
  assert_eq!(pass["subject"]["bash"]["command"], "cargo build");
  assert_eq!(pass["outcome"], "pass");
  assert_eq!(pass["rule"], serde_json::Value::Null);
  assert_eq!(pass["pattern"], serde_json::Value::Null);
  assert_eq!(pass["reason"], serde_json::Value::Null);
}

#[test]
fn hook_still_denies_when_the_log_cannot_be_written() {
  let state = state_dir();
  let blocked = state.path().join("blocked");
  fs::write(&blocked, "a file where a directory is needed").unwrap();

  guard_logging_to(&blocked)
    .arg("hook")
    .write_stdin(bash_call("git stash"))
    .assert()
    .code(0)
    .stdout(predicate::str::contains("\"permissionDecision\":\"deny\""))
    .stderr(predicate::str::contains("decision log write failed"));
}

#[test]
fn session_start_with_garbage_stdin_exits_zero() {
  guard()
    .arg("session-start")
    .write_stdin("this is not json {{{")
    .assert()
    .code(0);
}
