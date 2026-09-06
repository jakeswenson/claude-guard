//! claude-guard: a Claude Code PreToolUse hook that denies tool calls that
//! break house rules and logs every decision.
//!
//! Process contract, from the design spec:
//! - exit code is always 0, so a broken guard never blocks a session
//! - stdout carries a decision only when a rule speaks
//! - stderr carries diagnostics, visible with `claude --debug`

mod cond;
mod input;
mod load;
mod log;
mod output;
mod pattern;
mod repo;
mod rules;
mod segment;
mod sexp;
mod syntax;

use std::io::{IsTerminal, Read};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

use color_eyre::config::{HookBuilder, Theme};
use color_eyre::eyre::{Result, WrapErr, bail};
use log::Writer as _;
use tracing_error::ErrorLayer;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Environment variable holding the tracing filter, e.g. `debug` or
/// `claude_guard=trace`. Unset means `warn`.
const LOG_ENV: &str = "CLAUDE_GUARD_LOG";

const USAGE: &str = "usage: claude-guard <hook|session-start>  (reads hook JSON on stdin)";

fn main() -> ExitCode {
  fail_open(|| {
    install_diagnostics()?;
    run(std::env::args().nth(1).as_deref())
  })
}

/// The guard never blocks a session by accident. Whatever happens inside
/// `work`, the process exits 0 and stdout is left alone, so Claude Code
/// falls through to its normal permission rules.
///
/// - `Ok(())`: nothing to add.
/// - `Err(report)`: one line on stderr with the full cause chain. The
///   report's `Debug` form would add the span trace, but it spans many
///   lines and the spec asks for one.
/// - panic: color-eyre's hook has already printed its report by the time
///   the unwind reaches this frame. One more line says the call went
///   through, so a reader of the debug log is not left guessing.
fn fail_open(work: impl FnOnce() -> Result<()>) -> ExitCode {
  match panic::catch_unwind(AssertUnwindSafe(work)) {
    Ok(Ok(())) => {}
    Ok(Err(report)) => eprintln!("claude-guard: failed open: {}", chain(&report)),
    Err(_) => eprintln!("claude-guard: failed open after a panic; tool call proceeds"),
  }
  ExitCode::SUCCESS
}

/// The full cause chain on one line, outermost first.
fn chain(report: &color_eyre::Report) -> String {
  report
    .chain()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join(": ")
}

/// Dispatch on the subcommand. Stdin is read here, once, so both
/// subcommands see the same input type later.
fn run(subcommand: Option<&str>) -> Result<()> {
  let Some(subcommand) = subcommand else {
    eprintln!("{USAGE}");
    bail!("no subcommand given");
  };

  let mut input = String::new();
  std::io::stdin()
    .read_to_string(&mut input)
    .wrap_err("read hook input from stdin")?;

  match subcommand {
    "hook" => hook(&input),
    "session-start" => session_start(&input),
    other => {
      eprintln!("{USAGE}");
      bail!("unknown subcommand {other:?}");
    }
  }
}

/// Hook handler for every event. The event name in the payload decides:
/// PreToolUse gets a decision, everything else is recorded and stays
/// silent, so Claude Code behaves as if no hook ran.
fn hook(raw: &str) -> Result<()> {
  let envelope = input::envelope(raw)?;
  tracing::debug!(
      event = ?envelope.event,
      session = %envelope.session_id,
      cwd = %envelope.cwd,
      "hook invoked"
  );
  match envelope.event {
    input::Event::PreToolUse => pre_tool_use(raw),
    _ => observe(&envelope),
  }
}

/// SessionStart handler. Records the payload until the brief exists; the
/// brief will print from here.
fn session_start(raw: &str) -> Result<()> {
  let envelope = input::envelope(raw)?;
  observe(&envelope)
}

/// Parse, build the context, evaluate, log, print.
///
/// The log is written before stdout so a crash while printing still
/// leaves the record, and a log failure is one warning on stderr that
/// changes nothing about the decision.
fn pre_tool_use(raw: &str) -> Result<()> {
  let input = input::parse(raw)?;
  let rules = rules::Ruleset::builtin().wrap_err("compile built-in rules")?;
  let ctx = rules::Context::new(input);
  let verdict = rules.evaluate(&ctx);

  record(log::Record::pre_tool_use(
    &ctx,
    verdict.as_ref(),
    jiff::Timestamp::now(),
  ));

  match verdict {
    Some(verdict) => {
      tracing::info!(rule = %verdict.rule, decision = ?verdict.decision, "verdict");
      println!("{}", verdict.decision.to_json());
    }
    None => tracing::debug!("no opinion"),
  }
  Ok(())
}

/// Record an event the guard does not decide on. Nothing goes to stdout.
fn observe(envelope: &input::Envelope) -> Result<()> {
  record(log::Record::observed(envelope, jiff::Timestamp::now()));
  Ok(())
}

/// Append one record. A failure is one warning on stderr and nothing
/// else; the caller's decision, if any, still prints.
fn record(record: log::Record) {
  if let Err(report) = log::Store::from_env().and_then(|mut store| store.write(&record)) {
    tracing::warn!(
      error = %chain(&report),
      "decision log write failed; the decision still stands"
    );
  }
}

/// color-eyre for error and panic reports, tracing to stderr behind
/// `CLAUDE_GUARD_LOG`. Color is off when stderr is not a terminal so the
/// text Claude Code captures stays clean.
fn install_diagnostics() -> Result<()> {
  let is_tty = std::io::stderr().is_terminal();

  HookBuilder::default()
    .theme(if is_tty { Theme::dark() } else { Theme::new() })
    .display_env_section(false)
    .install()?;

  let filter = EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new("warn"));
  tracing_subscriber::registry()
    .with(filter)
    .with(ErrorLayer::default())
    .with(
      fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(is_tty)
        .without_time(),
    )
    .try_init()?;

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use color_eyre::eyre::eyre;

  #[test]
  fn ok_exits_zero() {
    assert_eq!(fail_open(|| Ok(())), ExitCode::SUCCESS);
  }

  #[test]
  fn error_exits_zero() {
    assert_eq!(fail_open(|| Err(eyre!("boom"))), ExitCode::SUCCESS);
  }

  #[test]
  fn panic_exits_zero() {
    assert_eq!(fail_open(|| panic!("boom")), ExitCode::SUCCESS);
  }
}
