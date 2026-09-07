//! claude-guard: a Claude Code PreToolUse hook that denies tool calls that
//! break house rules and logs every decision.
//!
//! Process contract, from the design spec:
//! - exit code is always 0, so a broken guard never blocks a session
//! - stdout carries a decision only when a rule speaks
//! - stderr carries diagnostics, visible with `claude --debug`

mod commands;
mod cond;
mod elaborate;
mod facts;
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
use commands::Carapace as _;
use log::{Reader as _, Writer as _};
use tracing_error::ErrorLayer;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Environment variable holding the tracing filter, e.g. `debug` or
/// `claude_guard=trace`. Unset means `warn`.
const LOG_ENV: &str = "CLAUDE_GUARD_LOG";

const USAGE: &str = "usage: claude-guard <hook|session-start>  (reads hook JSON on stdin)\n       \
                     claude-guard rules [--export]     (check the rules in force, or print the built-in file)\n       \
                     claude-guard commands             (which programs the sessions run, and what the guard knows)\n       \
                     claude-guard commands add [--force] [--stdout] <name>...  (declare a program from carapace)\n       \
                     claude-guard elaborate --check-log (round-trip every logged command through the elaborator)\n       \
                     claude-guard extern <fact> [arg]...  (ask one fact the way the hook would; hook JSON on stdin, or this directory)";

fn main() -> ExitCode {
  fail_open(|| {
    install_diagnostics()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args)
  })
}

/// The guard never blocks a session by accident. Whatever goes wrong
/// inside `work`, the process exits 0 and stdout is left alone, so Claude
/// Code falls through to its normal permission rules. Only a subcommand
/// that ran to completion chooses its own exit code, and the hook's is
/// always 0.
///
/// - `Ok(code)`: the subcommand's own answer.
/// - `Err(report)`: one line on stderr with the full cause chain. The
///   report's `Debug` form would add the span trace, but it spans many
///   lines and the spec asks for one.
/// - panic: color-eyre's hook has already printed its report by the time
///   the unwind reaches this frame. One more line says the call went
///   through, so a reader of the debug log is not left guessing.
fn fail_open(work: impl FnOnce() -> Result<ExitCode>) -> ExitCode {
  match panic::catch_unwind(AssertUnwindSafe(work)) {
    Ok(Ok(code)) => code,
    Ok(Err(report)) => {
      eprintln!("claude-guard: failed open: {}", chain(&report));
      ExitCode::SUCCESS
    }
    Err(_) => {
      eprintln!("claude-guard: failed open after a panic; tool call proceeds");
      ExitCode::SUCCESS
    }
  }
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
/// `rules` never touches it. Every subcommand but `extern` exits 0.
fn run(args: &[String]) -> Result<ExitCode> {
  let Some((subcommand, rest)) = args.split_first() else {
    eprintln!("{USAGE}");
    bail!("no subcommand given");
  };

  match subcommand.as_str() {
    "hook" => hook(&stdin()?),
    "session-start" => session_start(&stdin()?),
    "rules" => rules_command(rest),
    "commands" => commands_command(rest),
    "elaborate" => elaborate_command(rest),
    "extern" => return extern_command(rest),
    other => {
      eprintln!("{USAGE}");
      bail!("unknown subcommand {other:?}");
    }
  }?;
  Ok(ExitCode::SUCCESS)
}

/// `extern <fact> [arg]...` asks one fact the way the hook would and
/// prints what it answered and how long it took. The call comes from a
/// hook payload on stdin, or, when stdin is a terminal or empty, from
/// this directory with no session behind it. The exit code is the
/// answer: 0 holds, 1 fails, 2 unknown, 3 the fact could not be asked at
/// all. Stderr passes through from the program, so a script author sees
/// its own diagnostics.
fn extern_command(args: &[String]) -> Result<ExitCode> {
  let Some((name, fact_args)) = args.split_first() else {
    eprintln!("{USAGE}");
    bail!("`extern` needs a fact name");
  };
  let rules = match rules::Ruleset::load() {
    Ok(rules) => rules,
    Err(e) => {
      eprintln!("{e}");
      return Ok(ExitCode::from(3));
    }
  };
  let name = facts::FactName::from(name.as_str());
  if rules.facts.get(&name).is_none() {
    let known = rules
      .facts
      .names()
      .map(ToString::to_string)
      .collect::<Vec<_>>()
      .join(", ");
    eprintln!("claude-guard: no fact named `{name}`; the rules in force know: {known}");
    return Ok(ExitCode::from(3));
  }

  let input = match hook_input_on_stdin()? {
    Some(input) => input,
    None => input::HookInput {
      session_id: "extern".into(),
      cwd: std::env::current_dir()
        .wrap_err("find the current directory")?
        .into(),
      tool_use_id: "extern".into(),
      agent_id: None,
      tool: input::Tool::Other {
        name: "none".into(),
        input: serde_json::Value::Null,
      },
    },
  };
  let ctx = rules::Context::new(input, &rules.declarations);
  let call = rules::Ruleset::call_for(&ctx);
  let args: Vec<&str> = fact_args.iter().map(String::as_str).collect();
  let answer = rules.facts.ask(&name, &args, &call);
  let ms = rules.facts_asked().last().map_or(0, |asked| asked.ms);

  let (verdict, code) = match answer.truth {
    facts::Truth::True => (format!("{name} holds"), 0),
    facts::Truth::False => (format!("{name} fails"), 1),
    // The registry already named the fact in an unknown's reason.
    facts::Truth::Unknown => (String::new(), 2),
  };
  match (verdict.is_empty(), answer.reason) {
    (true, reason) => println!("{} ({ms}ms)", reason.unwrap_or_default()),
    (false, Some(reason)) => println!("{verdict}: {reason} ({ms}ms)"),
    (false, None) => println!("{verdict} ({ms}ms)"),
  }
  Ok(ExitCode::from(code))
}

/// A hook payload on stdin, or `None` when stdin is a terminal or holds
/// nothing, so `extern` can be typed at a prompt without a payload.
fn hook_input_on_stdin() -> Result<Option<input::HookInput>> {
  if std::io::stdin().is_terminal() {
    return Ok(None);
  }
  let text = stdin()?;
  if text.trim().is_empty() {
    return Ok(None);
  }
  input::parse(&text).map(Some)
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

/// `commands` prints the survey table; `commands add` writes declarations
/// from carapace. Both keep the exit code at zero; problems are lines on
/// stderr.
fn commands_command(args: &[String]) -> Result<()> {
  let carapace = commands::Binary::from_env();
  match args.split_first().map(|(a, rest)| (a.as_str(), rest)) {
    None => {
      let rules = match rules::Ruleset::load() {
        Ok(rules) => rules,
        Err(e) => {
          eprintln!("{e}");
          return Ok(());
        }
      };
      let store = log::Store::from_env()?;
      let mut records = Vec::new();
      for session_id in store.session_ids()? {
        records.extend(store.session(&session_id)?);
      }
      let listed = match carapace.list() {
        Ok(listed) => Some(listed),
        Err(e) => {
          eprintln!("claude-guard: carapace column unavailable: {}", chain(&e));
          None
        }
      };
      let builtin = load::builtin_command_names();
      let declared: std::collections::BTreeSet<String> =
        rules.declarations.by_name.keys().cloned().collect();
      let specific = commands::specific_programs(rules.rules());
      let rows = commands::survey(
        &records,
        &commands::Known {
          builtin: &builtin,
          declared: &declared,
          specific: &specific,
          carapace: listed.as_ref(),
        },
      );
      print!("{}", commands::render(&rows));
      Ok(())
    }
    Some(("add", rest)) => {
      let force = rest.iter().any(|a| a == "--force");
      let to_stdout = rest.iter().any(|a| a == "--stdout");
      let names: Vec<&String> = rest.iter().filter(|a| !a.starts_with("--")).collect();
      if names.is_empty() {
        eprintln!("{USAGE}");
        bail!("`commands add` needs at least one program name");
      }
      let dir = load::commands_dir_from_env();
      let today = jiff::Zoned::now().date().to_string();
      let mut print = |text: &str| print!("{text}");
      for name in names {
        match commands::add(
          name,
          &carapace,
          dir.as_deref(),
          force,
          to_stdout,
          &today,
          &mut print,
        ) {
          Ok(commands::Added::Written {
            path,
            options,
            subcommands,
          }) => eprintln!("{name}: wrote {path} ({options} options, {subcommands} subcommands)"),
          Ok(commands::Added::Printed {
            options,
            subcommands,
          }) => eprintln!("{name}: {options} options, {subcommands} subcommands"),
          Ok(commands::Added::Skipped(why)) => eprintln!("{name}: skipped, {why}"),
          Err(e) => eprintln!("{name}: {}", chain(&e)),
        }
      }
      Ok(())
    }
    Some(_) => {
      eprintln!("{USAGE}");
      bail!("unknown arguments to `commands`");
    }
  }
}

/// `elaborate --check-log` runs every Bash command in every session log
/// through the segmenter and the elaborator under the declarations in
/// force, and prints each one whose words do not come back whole. The
/// partition property (D22) is checked on live data, not only on the
/// fixture. The exit code stays zero; the output is the report.
fn elaborate_command(args: &[String]) -> Result<()> {
  let [flag] = args else {
    eprintln!("{USAGE}");
    bail!("`elaborate` takes --check-log");
  };
  if flag != "--check-log" {
    eprintln!("{USAGE}");
    bail!("unknown arguments to `elaborate`");
  }
  let rules = match rules::Ruleset::load() {
    Ok(rules) => rules,
    Err(e) => {
      eprintln!("{e}");
      return Ok(());
    }
  };
  let store = log::Store::from_env()?;
  let mut checked = 0usize;
  let mut failed = 0usize;
  let mut skipped = 0usize;
  for session_id in store.session_ids()? {
    for record in store.session(&session_id)? {
      let log::Subject::Bash {
        command,
        parse_error: None,
        ..
      } = &record.subject
      else {
        continue;
      };
      let Ok(segments) = segment::segment(command.as_str()) else {
        skipped += 1;
        continue;
      };
      for simple in &segments.commands {
        checked += 1;
        let elaborated = rules.declarations.elaborate(simple);
        let back = elaborated.flatten();
        if back != simple.words {
          failed += 1;
          let id = record
            .tool_use_id
            .as_ref()
            .map_or("-".to_string(), ToString::to_string);
          println!("{session_id} {id}: {command}");
          println!("  words:     {:?}", simple.words);
          println!("  flattened: {back:?}");
        }
      }
    }
  }
  println!("{checked} commands checked, {failed} did not round-trip, {skipped} no longer segment");
  Ok(())
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
  let verdict = rules.evaluate(&ctx);

  record(log::Record::pre_tool_use(
    &ctx,
    verdict.as_ref(),
    rules.facts_asked(),
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
  fn ok_exits_with_the_code_the_work_chose() {
    assert_eq!(fail_open(|| Ok(ExitCode::SUCCESS)), ExitCode::SUCCESS);
    assert_eq!(fail_open(|| Ok(ExitCode::from(2))), ExitCode::from(2));
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
