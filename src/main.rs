//! claude-guard: a Claude Code PreToolUse hook that denies tool calls that
//! break house rules and logs every decision.
//!
//! Process contract, from the design spec:
//! - exit code is always 0, so a broken guard never blocks a session
//! - stdout carries a decision only when a rule speaks
//! - stderr carries diagnostics, visible with `claude --debug`

use std::io::{IsTerminal, Read};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

use color_eyre::config::{HookBuilder, Theme};
use color_eyre::eyre::{Result, WrapErr, bail};
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
        Ok(Err(report)) => {
            let chain: Vec<String> = report.chain().map(ToString::to_string).collect();
            eprintln!("claude-guard: failed open: {}", chain.join(": "));
        }
        Err(_) => eprintln!("claude-guard: failed open after a panic; tool call proceeds"),
    }
    ExitCode::SUCCESS
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

/// PreToolUse handler. Stub until the input types and rule engine exist.
fn hook(input: &str) -> Result<()> {
    tracing::debug!(bytes = input.len(), "hook invoked");
    Ok(())
}

/// SessionStart handler. Stub until the brief exists.
fn session_start(input: &str) -> Result<()> {
    tracing::debug!(bytes = input.len(), "session-start invoked");
    Ok(())
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
