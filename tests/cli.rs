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

#[test]
fn session_start_with_garbage_stdin_exits_zero() {
    guard()
        .arg("session-start")
        .write_stdin("this is not json {{{")
        .assert()
        .code(0);
}
