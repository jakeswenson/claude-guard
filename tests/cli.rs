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
  assert_eq!(deny["pattern"], "[git -... stash ...]");
  assert_eq!(deny["bindings"], serde_json::json!({}));
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
  assert_eq!(pass["bindings"], serde_json::Value::Null);
  assert_eq!(pass["reason"], serde_json::Value::Null);
}

#[test]
fn a_binding_row_records_what_it_captured() {
  let state = state_dir();
  guard_logging_to(state.path())
    .arg("hook")
    .write_stdin(bash_call("cp -r dist /tmp/dist"))
    .assert()
    .code(0)
    .stdout(predicate::str::contains("\"permissionDecision\":\"deny\""));
  let log = fs::read_to_string(state.path().join("sessions").join("abc123.jsonl")).unwrap();
  let line: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
  assert_eq!(line["rule"], "tmp-writes");
  assert_eq!(line["pattern"], "[cp ... ?dst]");
  assert_eq!(line["bindings"], serde_json::json!({"dst": "/tmp/dist"}));
}

// --- the rule file ---

#[test]
fn a_user_rule_file_replaces_the_builtin_rules() {
  let state = state_dir();
  let rules = state.path().join("rules.scm");
  fs::write(
    &rules,
    "(rule mine (deny [cargo -... clean ...] :reason \"slow.\" :instead \"do not.\"))",
  )
  .unwrap();
  let mut command = guard_logging_to(state.path());
  command.env("CLAUDE_GUARD_RULES", &rules);
  command
    .arg("hook")
    .write_stdin(bash_call("cargo clean"))
    .assert()
    .code(0)
    .stdout(predicate::str::contains(
      "claude-guard denied `cargo clean`: slow. Instead: do not.",
    ));

  // The builtin git rule is gone with the builtin file.
  let mut command = guard_logging_to(state.path());
  command.env("CLAUDE_GUARD_RULES", &rules);
  command
    .arg("hook")
    .write_stdin(bash_call("git stash"))
    .assert()
    .code(0)
    .stdout("");
}

#[test]
fn a_broken_rule_file_fails_open_and_still_leaves_a_record() {
  let state = state_dir();
  let rules = state.path().join("rules.scm");
  fs::write(&rules, "(rule a)\n(rule b (deny [x]))").unwrap();
  let mut command = guard_logging_to(state.path());
  command.env("CLAUDE_GUARD_RULES", &rules);
  command
    .arg("hook")
    .write_stdin(bash_call("git stash"))
    .assert()
    .code(0)
    .stdout("")
    .stderr(predicate::str::contains(
      "rules.scm:1:1: rule `a` has no rows",
    ))
    .stderr(predicate::str::contains(
      "rules.scm:2:9: `deny` needs a :reason",
    ));

  let log = fs::read_to_string(state.path().join("sessions").join("abc123.jsonl")).unwrap();
  let line: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
  assert_eq!(line["event"], "pre_tool_use");
  assert_eq!(line["outcome"], "observed");
}

#[test]
fn rules_reports_the_source_in_force() {
  guard()
    .env("CLAUDE_GUARD_RULES", "")
    .env_remove("CLAUDE_GUARD_RULES")
    .env("XDG_CONFIG_HOME", state_dir().path())
    .arg("rules")
    .assert()
    .code(0)
    .stdout("built-in rules: 4 rules, 29 rows, 11 commands declared\n");

  let state = state_dir();
  let rules = state.path().join("rules.scm");
  fs::write(&rules, "(rule a)").unwrap();
  guard()
    .env("CLAUDE_GUARD_RULES", &rules)
    .arg("rules")
    .assert()
    .code(0)
    .stdout("")
    .stderr(predicate::str::contains(
      "rules.scm:1:1: rule `a` has no rows",
    ));
}

#[test]
fn rules_export_prints_the_builtin_file_verbatim() {
  guard()
    .arg("rules")
    .arg("--export")
    .assert()
    .code(0)
    .stdout(predicate::str::starts_with(
      ";; claude-guard built-in rules.",
    ))
    .stdout(predicate::str::contains(
      "(rule git-in-jj :when (ancestor-has? \".jj\")",
    ));
}

#[test]
fn other_events_are_recorded_whole_and_stay_silent() {
  let state = state_dir();
  let payload = r#"{
      "session_id": "abc123",
      "cwd": "/Users/x/code/proj",
      "hook_event_name": "PermissionDenied",
      "tool_name": "Bash",
      "tool_input": {"command": "git log"},
      "tool_use_id": "toolu_02",
      "reason": "whatever Claude Code sends here"
    }"#;
  guard_logging_to(state.path())
    .arg("hook")
    .write_stdin(payload)
    .assert()
    .code(0)
    .stdout("")
    .stderr("");

  let log = fs::read_to_string(state.path().join("sessions").join("abc123.jsonl")).unwrap();
  let line: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
  assert_eq!(line["event"], "permission_denied");
  assert_eq!(line["outcome"], "observed");
  assert_eq!(line["tool"], "Bash");
  assert_eq!(line["tool_use_id"], "toolu_02");
  assert_eq!(
    line["subject"]["raw"]["reason"],
    "whatever Claude Code sends here"
  );
  assert_eq!(line["rule"], serde_json::Value::Null);
}

#[test]
fn session_start_is_recorded_and_stays_silent() {
  let state = state_dir();
  let payload = r#"{"session_id": "abc123", "cwd": "/Users/x", "hook_event_name": "SessionStart", "source": "startup"}"#;
  guard_logging_to(state.path())
    .arg("session-start")
    .write_stdin(payload)
    .assert()
    .code(0)
    .stdout("")
    .stderr("");

  let log = fs::read_to_string(state.path().join("sessions").join("abc123.jsonl")).unwrap();
  let line: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
  assert_eq!(line["event"], "session_start");
  assert_eq!(line["outcome"], "observed");
  assert_eq!(line["tool"], serde_json::Value::Null);
  assert_eq!(line["subject"]["raw"]["source"], "startup");
}

#[test]
fn an_unknown_event_fails_open() {
  let state = state_dir();
  let payload = r#"{"session_id": "abc123", "cwd": "/Users/x", "hook_event_name": "Stop"}"#;
  guard_logging_to(state.path())
    .arg("hook")
    .write_stdin(payload)
    .assert()
    .code(0)
    .stdout("")
    .stderr(predicate::str::contains("unknown hook event"));
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
fn user_command_declarations_are_counted_and_checked() {
  let state = state_dir();
  let commands = state.path().join("commands");
  fs::create_dir_all(&commands).unwrap();
  fs::write(
    commands.join("jj.scm"),
    "(command jj (option \"-R\" :value))",
  )
  .unwrap();
  guard()
    .env("XDG_CONFIG_HOME", state_dir().path())
    .env("CLAUDE_GUARD_COMMANDS_DIR", &commands)
    .arg("rules")
    .assert()
    .code(0)
    .stdout("built-in rules: 4 rules, 29 rows, 12 commands declared\n");

  fs::write(commands.join("bad.scm"), "(command git (option \"C\"))").unwrap();
  guard()
    .env("XDG_CONFIG_HOME", state_dir().path())
    .env("CLAUDE_GUARD_COMMANDS_DIR", &commands)
    .arg("rules")
    .assert()
    .code(0)
    .stdout("")
    .stderr(predicate::str::contains(
      "bad.scm:1:22: option names look like \"-c\" or \"--long\", not \"C\"",
    ));
}

#[test]
fn session_start_with_garbage_stdin_exits_zero() {
  guard()
    .arg("session-start")
    .write_stdin("this is not json {{{")
    .assert()
    .code(0);
}
