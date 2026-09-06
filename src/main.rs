//! claude-guard: a Claude Code PreToolUse hook that denies tool calls that
//! break house rules and logs every decision.
//!
//! Process contract, from the design spec:
//! - exit code is always 0, so a broken guard never blocks a session
//! - stdout carries a decision only when a rule speaks
//! - stderr carries diagnostics, visible with `claude --debug`

mod cond;
mod elaborate;
mod input;
mod load;
mod log;
mod output;
mod pattern;
mod rules;
mod segment;
mod sexp;
mod spec;
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

const USAGE: &str = "usage: claude-guard <hook|session-start>  (reads hook JSON on stdin)\n       \
                     claude-guard rules [--export]     (check the rules in force, or print the built-in file)";

fn main() -> ExitCode {
  fail_open(|| {
    install_diagnostics()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args)
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

/// Dispatch on the subcommand. The hook subcommands read stdin once;
/// `rules` never touches it.
fn run(args: &[String]) -> Result<()> {
  let Some((subcommand, rest)) = args.split_first() else {
    eprintln!("{USAGE}");
    bail!("no subcommand given");
  };

  match subcommand.as_str() {
    "hook" => hook(&stdin()?),
    "session-start" => session_start(&stdin()?),
    "rules" => rules_command(rest),
    other => {
      eprintln!("{USAGE}");
      bail!("unknown subcommand {other:?}");
    }
  }
}

fn stdin() -> Result<String> {
  let mut input = String::new();
  std::io::stdin()
    .read_to_string(&mut input)
    .wrap_err("read hook input from stdin")?;
  Ok(input)
}

/// `rules` checks the rules in force and says where they came from;
/// `rules --export` prints the built-in file as a starting point for a
/// user file. A file that does not load prints its problems, one per
/// line, and the exit code stays zero like everything else.
fn rules_command(args: &[String]) -> Result<()> {
  match args {
    [] => match rules::Ruleset::load() {
      Ok(rules) => {
        let rows: usize = rules.rules().iter().map(|r| r.rows.len()).sum();
        println!(
          "{}: {} rules, {} rows, {} commands declared",
          rules.source,
          rules.rules().len(),
          rows,
          rules.declarations.by_name.len()
        );
        Ok(())
      }
      Err(e) => {
        eprintln!("{e}");
        Ok(())
      }
    },
    [flag] if flag == "--export" => {
      print!("{}", load::BUILTIN);
      Ok(())
    }
    _ => {
      eprintln!("{USAGE}");
      bail!("unknown arguments to `rules`");
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

/// Load the rules, parse, build the context, evaluate, log, print.
///
/// A rule file that does not load is every problem on stderr, the call
/// recorded as observed, and no decision: the guard never decides from a
/// half-loaded table, and every call still leaves a record.
///
/// The log is written before stdout so a crash while printing still
/// leaves the record, and a log failure is one warning on stderr that
/// changes nothing about the decision.
fn pre_tool_use(raw: &str) -> Result<()> {
  let rules = match rules::Ruleset::load() {
    Ok(rules) => rules,
    Err(e) => {
      eprintln!("claude-guard: rules did not load; tool call proceeds\n{e}");
      return observe(&input::envelope(raw)?);
    }
  };
  let input = input::parse(raw)?;
  let ctx = rules::Context::new(input, &rules.declarations);
  let verdict = rules.evaluate(&ctx, &cond::RealFs);

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
