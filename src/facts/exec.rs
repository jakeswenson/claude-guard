//! `(fact name (exec "program" "arg"...) ...)`: a fact answered by a
//! program over stdio (D9).
//!
//! The program gets the declaration's arguments and then the condition's
//! as argv; `CLAUDE_GUARD_CWD`, `CLAUDE_GUARD_SESSION_ID`,
//! `CLAUDE_GUARD_TOOL`, and `CLAUDE_GUARD_PROTOCOL=1` in its environment;
//! the call's cwd as its working directory; and one JSON object on stdin:
//!
//! ```json
//! {"protocol": 1, "cwd": "...", "session_id": "...", "tool": "Bash",
//!  "subject": {...}, "args": ["..."]}
//! ```
//!
//! `subject` is the log's subject for the call, the same shape the
//! record carries. The program answers with one JSON object on stdout,
//! `{"holds": true|false, "reason": "..."}`, the reason optional. A
//! non-zero exit, a timeout, or stdout that is not that object is
//! unknown, with a reason naming which. Stderr is inherited, so a script
//! can print to the hook's stderr while it works.
//!
//! The program runs in its own process group, and a timeout kills the
//! whole group: a `sh -c` script that timed out must not leave a child
//! behind holding the hook's stderr, or Claude Code waits on it past the
//! timeout the rule promised.

use std::io::{Read as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::facts::{Answer, Arg, Call, Fact};
use crate::input::{SessionId, ToolName};
use crate::sexp::Span;
use crate::syntax::TypeError;

/// How often the runner looks for the program's exit while waiting.
const POLL: Duration = Duration::from_millis(5);

/// The largest slice of stdout an error message quotes.
const EXCERPT: usize = 80;

pub struct Exec {
  pub program: String,
  pub args: Vec<String>,
  pub timeout: Duration,
}

#[derive(Serialize)]
struct Stdin<'a> {
  protocol: u32,
  cwd: &'a Path,
  session_id: &'a SessionId,
  tool: &'a ToolName,
  subject: &'a Value,
  args: &'a [&'a str],
}

#[derive(Deserialize)]
struct Stdout {
  holds: bool,
  #[serde(default)]
  reason: Option<String>,
}

impl Fact for Exec {
  /// Any arguments: the condition parser has already checked that each
  /// is a string or a binder in scope.
  fn check(
    &self,
    _args: &[(Arg, Span)],
    _span: Span,
  ) -> Result<(), TypeError> {
    Ok(())
  }

  fn ask(
    &self,
    args: &[&str],
    call: &Call<'_>,
  ) -> Answer {
    match self.run(args, call) {
      Ok(answer) => answer,
      Err(reason) => Answer::unknown(reason),
    }
  }
}

impl Exec {
  fn run(
    &self,
    args: &[&str],
    call: &Call<'_>,
  ) -> Result<Answer, String> {
    let started = Instant::now();
    let mut child = Command::new(&self.program)
      .args(&self.args)
      .args(args)
      .env("CLAUDE_GUARD_CWD", call.cwd)
      .env("CLAUDE_GUARD_SESSION_ID", call.session_id.as_ref())
      .env("CLAUDE_GUARD_TOOL", call.tool.as_ref())
      .env("CLAUDE_GUARD_PROTOCOL", "1")
      .current_dir(call.cwd)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .process_group(0)
      .spawn()
      .map_err(|e| format!("could not start `{}`: {e}", self.program))?;

    // Read stdout on its own thread before writing stdin, so a program
    // that talks before it listens cannot fill the pipe and stall.
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let reader = thread::spawn(move || {
      let mut out = String::new();
      let _ = stdout.read_to_string(&mut out);
      out
    });
    let input = serde_json::to_vec(&Stdin {
      protocol: 1,
      cwd: call.cwd,
      session_id: &call.session_id,
      tool: &call.tool,
      subject: &call.term,
      args,
    })
    .map_err(|e| format!("could not serialize the call: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
      // A program that never reads stdin closes it early; that is its
      // business, not an error.
      let _ = stdin.write_all(&input);
    }

    let deadline = started + self.timeout;
    let status = loop {
      match child.try_wait() {
        Ok(Some(status)) => break status,
        Ok(None) if Instant::now() >= deadline => {
          kill_group(&mut child);
          // The group is gone, so the pipe closes and the reader ends.
          let _ = reader.join();
          return Err(format!("timed out after {}", show(self.timeout)));
        }
        Ok(None) => thread::sleep(POLL),
        Err(e) => return Err(format!("could not wait for `{}`: {e}", self.program)),
      }
    };
    let out = reader.join().unwrap_or_default();

    if !status.success() {
      return Err(match status.code() {
        Some(code) => format!("exited with status {code}"),
        None => "was killed by a signal".into(),
      });
    }
    let parsed: Stdout = serde_json::from_str(out.trim()).map_err(|_| {
      format!(
        "stdout was not {{\"holds\": bool, \"reason\"?: string}}: {}",
        excerpt(&out)
      )
    })?;
    Ok(Answer {
      truth: parsed.holds.into(),
      reason: parsed.reason,
    })
  }
}

/// Kill the program and everything it started, then reap it. The child
/// leads its own process group, so the group id is its pid.
fn kill_group(child: &mut Child) {
  let pgid = child.id() as libc::pid_t;
  // SAFETY: `kill` is a plain syscall on a pid we own; the negative pid
  // targets the group the child leads and nothing else.
  unsafe {
    libc::kill(-pgid, libc::SIGKILL);
  }
  let _ = child.kill();
  let _ = child.wait();
}

/// A duration the way a rule file writes it: `1s`, `500ms`, `1.5s`.
pub fn show(d: Duration) -> String {
  let ms = d.as_millis();
  if ms == 0 {
    format!("{}µs", d.as_micros())
  } else if ms.is_multiple_of(1000) {
    format!("{}s", ms / 1000)
  } else if ms > 1000 {
    format!("{}s", ms as f64 / 1000.0)
  } else {
    format!("{ms}ms")
  }
}

/// The start of `text`, one line, for an error message.
fn excerpt(text: &str) -> String {
  let trimmed = text.trim();
  if trimmed.is_empty() {
    return "nothing".into();
  }
  let one_line: String = trimmed.chars().take(EXCERPT).collect();
  let shown = one_line.replace('\n', " ");
  if trimmed.chars().count() > EXCERPT {
    format!("`{shown}...`")
  } else {
    format!("`{shown}`")
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::facts::Truth;

  fn sh(
    script: &str,
    timeout: Duration,
  ) -> Exec {
    Exec {
      program: "sh".into(),
      args: vec!["-c".into(), script.into()],
      timeout,
    }
  }

  fn ask(
    script: &str,
    args: &[&str],
  ) -> Answer {
    let dir = tempfile::tempdir().unwrap();
    sh(script, Duration::from_secs(5)).ask(args, &Call::at(dir.path()))
  }

  #[test]
  fn a_program_answers_with_one_json_object() {
    assert_eq!(ask("echo '{\"holds\": true}'", &[]), Answer::holds());
    assert_eq!(ask("echo '{\"holds\": false}'", &[]), Answer::fails());
    assert_eq!(
      ask(
        "echo '{\"holds\": true, \"reason\": \"the script said so\"}'",
        &[]
      ),
      Answer {
        truth: Truth::True,
        reason: Some("the script said so".into())
      }
    );
    // Extra fields and surrounding whitespace are tolerated.
    assert_eq!(
      ask("printf '\\n {\"holds\": false, \"extra\": 1} \\n'", &[]),
      Answer::fails()
    );
  }

  #[test]
  fn the_condition_arguments_follow_the_declared_ones() {
    let answer = ask(
      "test \"$1\" = a -a \"$2\" = b && echo '{\"holds\": true}' || echo '{\"holds\": false}'",
      &["ignored"],
    );
    // `sh -c script` names the script `$0`; the first extra word is `$1`.
    assert_eq!(answer, Answer::fails());
    let dir = tempfile::tempdir().unwrap();
    let exec = Exec {
      program: "sh".into(),
      args: vec![
        "-c".into(),
        "test \"$1\" = a -a \"$2\" = b && echo '{\"holds\": true}' || echo '{\"holds\": false}'"
          .into(),
        "script".into(),
        "a".into(),
      ],
      timeout: Duration::from_secs(5),
    };
    assert_eq!(exec.ask(&["b"], &Call::at(dir.path())), Answer::holds());
  }

  #[test]
  fn the_program_sees_the_call_in_its_environment_and_on_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let script = "test \"$CLAUDE_GUARD_PROTOCOL\" = 1 \
                  && test \"$CLAUDE_GUARD_SESSION_ID\" = s1 \
                  && test \"$CLAUDE_GUARD_TOOL\" = Bash \
                  && test \"$CLAUDE_GUARD_CWD\" = \"$PWD\" \
                  && grep -q '\"protocol\":1' \
                  && echo '{\"holds\": true}' || echo '{\"holds\": false}'";
    let call = Call {
      cwd: &cwd,
      session_id: SessionId::from("s1"),
      tool: ToolName::from("Bash"),
      term: serde_json::json!({"bash": {"command": "git stash"}}),
    };
    assert_eq!(
      sh(script, Duration::from_secs(5)).ask(&[], &call),
      Answer::holds()
    );
    // stdin carries the subject and the arguments.
    let script = "grep -q '\"subject\":{\"bash\":{\"command\":\"git stash\"}},\"args\":\\[\"x\",\"y\"\\]' \
                  && echo '{\"holds\": true}' || echo '{\"holds\": false}'";
    assert_eq!(
      sh(script, Duration::from_secs(5)).ask(&["x", "y"], &call),
      Answer::holds()
    );
  }

  #[test]
  fn every_failure_is_unknown_with_a_reason() {
    assert_eq!(ask("exit 3", &[]), Answer::unknown("exited with status 3"));
    assert_eq!(
      ask("echo not json", &[]),
      Answer::unknown("stdout was not {\"holds\": bool, \"reason\"?: string}: `not json`")
    );
    assert_eq!(
      ask("true", &[]),
      Answer::unknown("stdout was not {\"holds\": bool, \"reason\"?: string}: nothing")
    );
    assert_eq!(
      ask("echo '{\"holds\": \"yes\"}'", &[]),
      Answer::unknown(
        "stdout was not {\"holds\": bool, \"reason\"?: string}: `{\"holds\": \"yes\"}`"
      )
    );
    let dir = tempfile::tempdir().unwrap();
    let missing = Exec {
      program: "/definitely/not/a/program".into(),
      args: vec![],
      timeout: Duration::from_secs(1),
    };
    let answer = missing.ask(&[], &Call::at(dir.path()));
    assert_eq!(answer.truth, Truth::Unknown);
    assert!(
      answer
        .reason
        .as_deref()
        .unwrap()
        .starts_with("could not start `/definitely/not/a/program`:")
    );
  }

  #[test]
  fn a_slow_program_and_everything_it_started_are_killed_at_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("still-running");
    // The grandchild would write the marker once the timeout has long
    // passed; if the group kill reached it, the marker never appears.
    let script = format!(
      "sleep 1 && touch {} & sleep 5; echo '{{\"holds\": true}}'",
      marker.display()
    );
    let started = Instant::now();
    let answer = sh(&script, Duration::from_millis(100)).ask(&[], &Call::at(dir.path()));
    assert_eq!(answer, Answer::unknown("timed out after 100ms"));
    assert!(started.elapsed() < Duration::from_secs(2));
    thread::sleep(Duration::from_millis(1500));
    assert!(!marker.exists(), "the grandchild outlived the timeout");
  }

  #[test]
  fn a_program_that_ignores_stdin_still_answers() {
    assert_eq!(
      ask("exec 0<&-; echo '{\"holds\": true}'", &[]),
      Answer::holds()
    );
  }

  #[test]
  fn durations_show_the_way_a_file_writes_them() {
    assert_eq!(show(Duration::from_secs(1)), "1s");
    assert_eq!(show(Duration::from_millis(500)), "500ms");
    assert_eq!(show(Duration::from_millis(1500)), "1.5s");
    assert_eq!(show(Duration::from_micros(250)), "250µs");
  }

  #[test]
  fn an_excerpt_is_one_short_line() {
    assert_eq!(excerpt("  hi  "), "`hi`");
    assert_eq!(excerpt("a\nb"), "`a b`");
    assert_eq!(excerpt(""), "nothing");
    let long = "x".repeat(100);
    assert_eq!(excerpt(&long), format!("`{}...`", "x".repeat(80)));
  }
}
