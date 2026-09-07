//! `claude-guard commands`: which programs the sessions run, which of
//! them the guard understands, and where an elaborator would change a
//! decision. `commands add` asks carapace for a program's option grammar
//! and writes it as a declaration file. D26 in the design.
//!
//! The table has one row per program name seen in the logs:
//!
//! - `calls`: top-level simple commands with that name.
//! - `generic-hits`: records where the pattern that fired was the
//!   catch-all `[name ...]` although the rules in force have a more
//!   specific row for `name`. That is what `-...` stopping at a flag
//!   value looks like from the outside. A program whose only row is the
//!   catch-all, such as `grep` under the nudge rule, never counts.
//! - `flagged`: calls on an undeclared program where a dash word was
//!   followed by a non-dash word, so a value may be passing as an
//!   argument or the reverse.
//! - `status`: `built-in`, `config`, or `undeclared`.
//! - `carapace`: whether carapace lists a completer for it.
//!
//! Sorted by generic hits, then flagged calls, then calls, so the top row
//! is the next thing to add.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use color_eyre::eyre::{Result, WrapErr, bail};
use serde_json::Value;

use crate::log::{Record, Subject};
use crate::segment::Word;

/// The carapace binary, or a stand-in for tests.
pub trait Carapace {
  /// Every program carapace has a completer for.
  fn list(&self) -> Result<BTreeSet<String>>;
  /// The export JSON for `name`, or `None` when carapace has nothing.
  fn export(
    &self,
    name: &str,
  ) -> Result<Option<String>>;
}

/// Runs the real binary. `CLAUDE_GUARD_CARAPACE` names it; default
/// `carapace` on `PATH`.
pub struct Binary {
  program: String,
}

impl Binary {
  pub const ENV: &str = "CLAUDE_GUARD_CARAPACE";

  pub fn from_env() -> Binary {
    Binary {
      program: std::env::var(Binary::ENV).unwrap_or_else(|_| "carapace".into()),
    }
  }

  fn run(
    &self,
    args: &[&str],
  ) -> Result<String> {
    let output = Command::new(&self.program)
      .args(args)
      .output()
      .wrap_err_with(|| {
        format!(
          "run `{} {}`; install carapace-bin or write the declaration by hand",
          self.program,
          args.join(" ")
        )
      })?;
    if !output.status.success() {
      bail!(
        "`{} {}` failed: {}",
        self.program,
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
      );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
  }
}

impl Carapace for Binary {
  fn list(&self) -> Result<BTreeSet<String>> {
    let text = self.run(&["--list"])?;
    let value: Value = serde_json::from_str(&text).wrap_err("parse `carapace --list`")?;
    let Value::Object(map) = value else {
      bail!("`carapace --list` is not a JSON object");
    };
    Ok(map.keys().cloned().collect())
  }

  fn export(
    &self,
    name: &str,
  ) -> Result<Option<String>> {
    // carapace prints nothing, and exits 0, for a program it does not know.
    let text = self.run(&[name, "export"])?;
    Ok((!text.trim().is_empty()).then_some(text))
  }
}

// --- the table ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
  BuiltIn,
  Config,
  Undeclared,
}

impl Status {
  fn text(self) -> &'static str {
    match self {
      Status::BuiltIn => "built-in",
      Status::Config => "config",
      Status::Undeclared => "undeclared",
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
  pub name: String,
  pub calls: usize,
  pub generic_hits: usize,
  pub flagged: usize,
  pub status: Status,
  /// `None` when carapace could not be asked.
  pub carapace: Option<bool>,
}

/// What the survey needs to know besides the records.
pub struct Known<'a> {
  pub builtin: &'a BTreeSet<String>,
  pub declared: &'a BTreeSet<String>,
  /// Programs the rules in force have a row for beyond `[name ...]`.
  pub specific: &'a BTreeSet<String>,
  pub carapace: Option<&'a BTreeSet<String>>,
}

/// The programs `rules` say something specific about: a command row
/// whose first word is the name and whose shape is not `[name ...]`.
pub fn specific_programs(rules: &[crate::syntax::Rule]) -> BTreeSet<String> {
  rules
    .iter()
    .flat_map(|rule| &rule.rows)
    .filter_map(|row| match &row.subject {
      crate::syntax::Subject::Command(pattern) => Some(pattern.to_string()),
      crate::syntax::Subject::Tool(_) => None,
    })
    .filter(|text| generic_pattern_name(text).is_none())
    .filter_map(|text| {
      let inner = text.strip_prefix('[')?;
      let name = inner.split_whitespace().next()?;
      let plain = !name.starts_with(['-', '?', '*', '>', '<', '.', '"']);
      plain.then(|| name.to_string())
    })
    .collect()
}

/// One row per program name, sorted as the module docs say.
pub fn survey<'r>(
  records: impl IntoIterator<Item = &'r Record>,
  known: &Known<'_>,
) -> Vec<Row> {
  let mut rows: BTreeMap<String, Row> = BTreeMap::new();
  fn row<'m>(
    rows: &'m mut BTreeMap<String, Row>,
    known: &Known<'_>,
    name: &str,
  ) -> &'m mut Row {
    rows.entry(name.to_string()).or_insert_with(|| Row {
      name: name.to_string(),
      calls: 0,
      generic_hits: 0,
      flagged: 0,
      status: if known.builtin.contains(name) {
        Status::BuiltIn
      } else if known.declared.contains(name) {
        Status::Config
      } else {
        Status::Undeclared
      },
      carapace: known.carapace.map(|set| set.contains(name)),
    })
  }

  for record in records {
    let Subject::Bash {
      commands,
      parse_error: None,
      ..
    } = &record.subject
    else {
      continue;
    };
    for command in commands {
      let Some(Word::Literal(first)) = command.words.first() else {
        continue;
      };
      let name = first.rsplit('/').next().unwrap_or(first);
      let entry = row(&mut rows, known, name);
      entry.calls += 1;
      if entry.status == Status::Undeclared && has_flag_then_value(&command.words) {
        entry.flagged += 1;
      }
    }
    if let Some(pattern) = &record.pattern
      && let Some(name) = generic_pattern_name(pattern.as_ref())
      && known.specific.contains(name)
    {
      row(&mut rows, known, name).generic_hits += 1;
    }
  }

  let mut rows: Vec<Row> = rows.into_values().collect();
  rows.sort_by(|a, b| {
    b.generic_hits
      .cmp(&a.generic_hits)
      .then(b.flagged.cmp(&a.flagged))
      .then(b.calls.cmp(&a.calls))
      .then(a.name.cmp(&b.name))
  });
  rows
}

/// `[git ...]` is the catch-all shape; anything else is not.
fn generic_pattern_name(pattern: &str) -> Option<&str> {
  let inner = pattern.strip_prefix('[')?.strip_suffix(']')?;
  let mut words = inner.split_whitespace();
  let name = words.next()?;
  if words.next() == Some("...") && words.next().is_none() && !name.starts_with('-') {
    Some(name)
  } else {
    None
  }
}

/// A dash word followed by a non-dash word, after the name: the shape
/// of a flag with a value, or of a flag before an argument.
fn has_flag_then_value(words: &[Word]) -> bool {
  words.windows(2).skip(1).any(|pair| match pair {
    [Word::Literal(flag), Word::Literal(next)] => {
      flag.starts_with('-') && flag != "-" && flag != "--" && !next.starts_with('-')
    }
    _ => false,
  })
}

/// The table as text, columns aligned.
pub fn render(rows: &[Row]) -> String {
  if rows.is_empty() {
    return "no commands logged yet\n".into();
  }
  let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(7).max(7);
  let mut out = String::new();
  let _ = writeln!(
    out,
    "{:<width$}  {:>5}  {:>12}  {:>7}  {:<10}  carapace",
    "command", "calls", "generic-hits", "flagged", "status"
  );
  for row in rows {
    let carapace = match row.carapace {
      Some(true) => "yes",
      Some(false) => "no",
      None => "?",
    };
    let _ = writeln!(
      out,
      "{:<width$}  {:>5}  {:>12}  {:>7}  {:<10}  {}",
      row.name,
      row.calls,
      row.generic_hits,
      row.flagged,
      row.status.text(),
      carapace
    );
  }
  out
}

// --- commands add ---

/// A carapace export as a declaration file. `today` goes in the header.
pub fn convert(
  name: &str,
  json: &str,
  today: &str,
) -> Result<String> {
  let value: Value = serde_json::from_str(json).wrap_err("parse the carapace export")?;
  let mut out = String::new();
  let _ = writeln!(
    out,
    ";; {name}: generated by `claude-guard commands add {name}` from `carapace {name} export` on {today}."
  );
  let _ = writeln!(
    out,
    ";; Edit freely; `commands add --force {name}` regenerates it. Add `:inner (...)` if {name}"
  );
  let _ = writeln!(
    out,
    ";; carries another command; see rules/commands.scm for the forms."
  );
  let _ = writeln!(out, "(command {}", symbol(name));
  write_declaration(&mut out, &value, 1);
  out.push_str(")\n");
  Ok(out)
}

/// Options, then subcommands, of one export node, indented by `depth`.
fn write_declaration(
  out: &mut String,
  node: &Value,
  depth: usize,
) {
  let pad = "  ".repeat(depth);
  let mut seen_short = BTreeSet::new();
  let mut seen_long = BTreeSet::new();
  let flags = ["LocalFlags", "PersistentFlags"]
    .into_iter()
    .filter_map(|key| node.get(key).and_then(Value::as_array))
    .flatten();
  for flag in flags {
    let short = flag
      .get("Shorthand")
      .and_then(Value::as_str)
      .filter(|s| !s.is_empty());
    let long = flag
      .get("Longhand")
      .and_then(Value::as_str)
      .filter(|s| !s.is_empty());
    let mut names = Vec::new();
    match short {
      Some(s) if s.chars().count() == 1 && seen_short.insert(s.to_string()) => {
        names.push(format!("\"-{}\"", escape(s)));
      }
      Some(s) if s.chars().count() != 1 => {
        let _ = writeln!(out, "{pad};; skipped: shorthand {s:?} is not one character");
      }
      _ => {}
    }
    if let Some(l) = long
      && seen_long.insert(l.to_string())
    {
      names.push(format!("\"--{}\"", escape(l)));
    }
    if names.is_empty() {
      continue;
    }
    let arity = if flag
      .get("NoOptDefVal")
      .and_then(Value::as_str)
      .is_some_and(|v| !v.is_empty())
    {
      " :optional"
    } else {
      match flag.get("Type").and_then(Value::as_str).unwrap_or("bool") {
        "bool" | "count" => "",
        _ => " :value",
      }
    };
    let _ = writeln!(out, "{pad}(option {}{arity})", names.join(" "));
  }

  let mut seen_sub = BTreeSet::new();
  for sub in node
    .get("Commands")
    .and_then(Value::as_array)
    .into_iter()
    .flatten()
  {
    let Some(sub_name) = sub.get("Name").and_then(Value::as_str) else {
      continue;
    };
    if !seen_sub.insert(sub_name.to_string()) {
      continue;
    }
    let aliases: Vec<&str> = sub
      .get("Aliases")
      .and_then(Value::as_array)
      .into_iter()
      .flatten()
      .filter_map(Value::as_str)
      .filter(|a| seen_sub.insert((*a).to_string()))
      .collect();
    let _ = write!(out, "{pad}(subcommand {}", symbol(sub_name));
    for alias in aliases {
      let _ = write!(out, " :alias {}", symbol(alias));
    }
    out.push('\n');
    write_declaration(out, sub, depth + 1);
    // Close the subcommand on the last line written for it.
    if out.ends_with('\n') {
      out.pop();
    }
    out.push_str(")\n");
  }
}

/// A name as a symbol, or a string when the reader would not take it.
fn symbol(name: &str) -> String {
  let plain = !name.is_empty()
    && !name.starts_with(':')
    && !name
      .chars()
      .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '"' | ';'));
  if plain {
    name.to_string()
  } else {
    format!("\"{}\"", escape(name))
  }
}

fn escape(text: &str) -> String {
  text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// What `commands add` did for one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
  Written {
    path: String,
    options: usize,
    subcommands: usize,
  },
  Printed {
    options: usize,
    subcommands: usize,
  },
  Skipped(String),
}

/// Generate and write, or print, the declaration for `name`.
pub fn add(
  name: &str,
  carapace: &dyn Carapace,
  dir: Option<&Path>,
  force: bool,
  to_stdout: bool,
  today: &str,
  print: &mut dyn FnMut(&str),
) -> Result<Added> {
  let Some(json) = carapace.export(name)? else {
    return Ok(Added::Skipped(format!(
      "carapace has no completer for `{name}`; write ~/.config/claude-guard/commands/{name}.scm by hand"
    )));
  };
  let text = convert(name, &json, today)?;

  // Never write a file the loader would reject.
  let forms = crate::sexp::read_all(&text)
    .map_err(|e| color_eyre::eyre::eyre!("generated text does not read: {e}"))?;
  let file = crate::syntax::parse(&forms, &crate::facts::Facts::builtin()).map_err(|errors| {
    color_eyre::eyre::eyre!(
      "generated declaration does not type-check: {}",
      errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
    )
  })?;
  let declaration = &file.commands[0].declaration;
  let options = declaration.options.len();
  let subcommands = declaration.subcommands.len();

  if to_stdout {
    print(&text);
    return Ok(Added::Printed {
      options,
      subcommands,
    });
  }
  let Some(dir) = dir else {
    bail!("no config directory: set CLAUDE_GUARD_COMMANDS_DIR, XDG_CONFIG_HOME, or HOME");
  };
  let path = dir.join(format!("{name}.scm"));
  if path.exists() && !force {
    return Ok(Added::Skipped(format!(
      "{} exists; pass --force to regenerate it",
      path.display()
    )));
  }
  std::fs::create_dir_all(dir).wrap_err_with(|| format!("create {}", dir.display()))?;
  std::fs::write(&path, &text).wrap_err_with(|| format!("write {}", path.display()))?;
  Ok(Added::Written {
    path: path.display().to_string(),
    options,
    subcommands,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::input::{HookInput, Tool};
  use crate::rules::testing::builtin_in_repo;
  use crate::rules::{Context, Ruleset};

  const GIT_EXPORT: &str = r#"{
    "Name": "git", "Short": "vcs",
    "LocalFlags": [
      {"Shorthand": "C", "Usage": "path", "Type": "string"},
      {"Longhand": "git-dir", "Type": "string"},
      {"Longhand": "no-pager", "Shorthand": "P", "Type": "bool"},
      {"Longhand": "color", "Type": "string", "NoOptDefVal": "always"},
      {"Longhand": "config-env", "Type": "stringArray"},
      {"Shorthand": "v", "Longhand": "verbose", "Type": "count"},
      {"Shorthand": "xx", "Longhand": "weird", "Type": "bool"},
      {"Shorthand": "C", "Type": "string"}
    ],
    "PersistentFlags": [{"Longhand": "help", "Shorthand": "h", "Type": "bool"}],
    "Commands": [
      {"Name": "add", "Aliases": ["stage"], "LocalFlags": [{"Shorthand": "A", "Longhand": "all", "Type": "bool"}]},
      {"Name": "stash", "LocalFlags": [{"Shorthand": "q", "Longhand": "quiet", "Type": "bool"}],
       "Commands": [{"Name": "pop", "LocalFlags": [{"Longhand": "index", "Type": "bool"}]}]},
      {"Name": "add"}
    ]
  }"#;

  struct Fake;

  impl Carapace for Fake {
    fn list(&self) -> Result<BTreeSet<String>> {
      Ok(
        ["git", "jj", "cargo"]
          .into_iter()
          .map(String::from)
          .collect(),
      )
    }

    fn export(
      &self,
      name: &str,
    ) -> Result<Option<String>> {
      Ok((name == "git").then(|| GIT_EXPORT.to_string()))
    }
  }

  // --- convert ---

  #[test]
  fn convert_maps_carapace_types_and_nests_subcommands() {
    let text = convert("git", GIT_EXPORT, "2026-09-06").unwrap();
    assert_eq!(
      text,
      concat!(
        ";; git: generated by `claude-guard commands add git` from `carapace git export` on 2026-09-06.\n",
        ";; Edit freely; `commands add --force git` regenerates it. Add `:inner (...)` if git\n",
        ";; carries another command; see rules/commands.scm for the forms.\n",
        "(command git\n",
        "  (option \"-C\" :value)\n",
        "  (option \"--git-dir\" :value)\n",
        "  (option \"-P\" \"--no-pager\")\n",
        "  (option \"--color\" :optional)\n",
        "  (option \"--config-env\" :value)\n",
        "  (option \"-v\" \"--verbose\")\n",
        "  ;; skipped: shorthand \"xx\" is not one character\n",
        "  (option \"--weird\")\n",
        "  (option \"-h\" \"--help\")\n",
        "  (subcommand add :alias stage\n",
        "    (option \"-A\" \"--all\"))\n",
        "  (subcommand stash\n",
        "    (option \"-q\" \"--quiet\")\n",
        "    (subcommand pop\n",
        "      (option \"--index\")))\n",
        ")\n",
      )
    );
    // What it wrote loads.
    let forms = crate::sexp::read_all(&text).unwrap();
    let file = crate::syntax::parse(&forms, &crate::facts::Facts::builtin())
      .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(file.commands[0].declaration.options.len(), 8);
    assert_eq!(file.commands[0].declaration.subcommands.len(), 3);
  }

  #[test]
  fn convert_quotes_a_name_the_reader_would_not_take() {
    let text = convert("odd name", r#"{"Name":"odd name"}"#, "2026-09-06").unwrap();
    assert!(text.contains("(command \"odd name\"\n)"), "{text}");
    assert!(convert("x", "not json", "2026-09-06").is_err());
  }

  // --- add ---

  #[test]
  fn add_writes_a_file_once_unless_forced() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cmds");
    let printed = std::cell::RefCell::new(String::new());
    let mut print = |s: &str| printed.borrow_mut().push_str(s);
    let added = add(
      "git",
      &Fake,
      Some(&target),
      false,
      false,
      "2026-09-06",
      &mut print,
    )
    .unwrap();
    assert_eq!(
      added,
      Added::Written {
        path: target.join("git.scm").display().to_string(),
        options: 8,
        subcommands: 3
      }
    );
    let again = add(
      "git",
      &Fake,
      Some(&target),
      false,
      false,
      "2026-09-06",
      &mut print,
    )
    .unwrap();
    assert!(
      matches!(again, Added::Skipped(ref m) if m.contains("--force")),
      "{again:?}"
    );
    let forced = add(
      "git",
      &Fake,
      Some(&target),
      true,
      false,
      "2026-09-06",
      &mut print,
    )
    .unwrap();
    assert!(matches!(forced, Added::Written { .. }));
    assert!(printed.borrow().is_empty());

    let unknown = add(
      "nope",
      &Fake,
      Some(&target),
      false,
      false,
      "2026-09-06",
      &mut print,
    )
    .unwrap();
    assert!(matches!(unknown, Added::Skipped(ref m) if m.contains("no completer for `nope`")));
    assert!(!target.join("nope.scm").exists());
  }

  #[test]
  fn add_can_print_instead() {
    let printed = std::cell::RefCell::new(String::new());
    let mut print = |s: &str| printed.borrow_mut().push_str(s);
    let added = add("git", &Fake, None, false, true, "2026-09-06", &mut print).unwrap();
    assert_eq!(
      added,
      Added::Printed {
        options: 8,
        subcommands: 3
      }
    );
    assert!(printed.borrow().starts_with(";; git: generated"));
    assert!(add("git", &Fake, None, false, false, "2026-09-06", &mut print).is_err());
  }

  // --- survey ---

  fn record(
    command: &str,
    rules: &Ruleset,
  ) -> Record {
    let ctx = Context::new(
      HookInput {
        session_id: "s".into(),
        cwd: "/x".into(),
        tool_use_id: "t".into(),
        agent_id: None,
        tool: Tool::Bash {
          command: command.into(),
        },
      },
      &rules.declarations,
    );
    let verdict = rules.evaluate(&ctx);
    Record::pre_tool_use(
      &ctx,
      verdict.as_ref(),
      "2026-09-06T00:00:00Z".parse().unwrap(),
    )
  }

  #[test]
  fn survey_counts_calls_generic_hits_and_flagged_calls() {
    let rules = builtin_in_repo(true);
    let records: Vec<Record> = [
      "git -C . push origin main",
      "git -C . log",
      "git stash",
      "rg -n foo src",
      "rg foo",
      "ssh -o BatchMode=yes nas ls",
      "cargo build && /usr/bin/git reflog",
      "echo hi",
    ]
    .iter()
    .map(|c| record(c, &rules))
    .collect();
    let builtin: BTreeSet<String> = ["ssh".to_string()].into();
    let declared: BTreeSet<String> = ["ssh".to_string(), "cargo".to_string()].into();
    let carapace: BTreeSet<String> = ["git", "rg", "cargo", "ssh"]
      .into_iter()
      .map(String::from)
      .collect();
    let specific = specific_programs(rules.rules());
    assert!(specific.contains("git"), "{specific:?}");
    assert!(!specific.contains("grep"), "{specific:?}");
    let rows = survey(
      &records,
      &Known {
        builtin: &builtin,
        declared: &declared,
        specific: &specific,
        carapace: Some(&carapace),
      },
    );
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["git", "rg", "cargo", "echo", "ssh"]);
    let git = &rows[0];
    // `/usr/bin/git reflog` counts as a git call, but the `[git ...]` row
    // does not match a program path, so it is not a generic hit.
    assert_eq!((git.calls, git.generic_hits, git.flagged), (4, 2, 2));
    assert_eq!(git.status, Status::Undeclared);
    assert_eq!(git.carapace, Some(true));
    let rg = &rows[1];
    assert_eq!((rg.calls, rg.generic_hits, rg.flagged), (2, 0, 1));
    let cargo = &rows[2];
    assert_eq!(
      (cargo.calls, cargo.flagged, cargo.status),
      (1, 0, Status::Config)
    );
    let echo = &rows[3];
    assert_eq!(
      (echo.status, echo.carapace),
      (Status::Undeclared, Some(false))
    );
    let ssh = &rows[4];
    assert_eq!((ssh.flagged, ssh.status), (0, Status::BuiltIn));
  }

  #[test]
  fn survey_without_carapace_marks_the_column_unknown() {
    let rules = builtin_in_repo(true);
    let records = vec![record("ls", &rules)];
    let empty = BTreeSet::new();
    let rows = survey(
      &records,
      &Known {
        builtin: &empty,
        declared: &empty,
        specific: &empty,
        carapace: None,
      },
    );
    assert_eq!(rows[0].carapace, None);
    assert_eq!(
      render(&rows),
      "command  calls  generic-hits  flagged  status      carapace\n\
       ls           1             0        0  undeclared  ?\n"
    );
    assert_eq!(render(&[]), "no commands logged yet\n");
  }

  #[test]
  fn a_catch_all_only_program_never_has_generic_hits() {
    let rules = builtin_in_repo(true);
    let records = vec![
      record("grep -r foo src", &rules),
      record("git reflog", &rules),
    ];
    assert_eq!(
      records[0].pattern.as_ref().map(ToString::to_string),
      Some("[grep ...]".into())
    );
    let empty = BTreeSet::new();
    let specific = specific_programs(rules.rules());
    let rows = survey(
      &records,
      &Known {
        builtin: &empty,
        declared: &empty,
        specific: &specific,
        carapace: None,
      },
    );
    let by_name: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.name.as_str(), r)).collect();
    assert_eq!(by_name["grep"].generic_hits, 0);
    assert_eq!(by_name["git"].generic_hits, 1);
  }

  #[test]
  fn generic_patterns_are_name_then_rest_only() {
    assert_eq!(generic_pattern_name("[git ...]"), Some("git"));
    assert_eq!(generic_pattern_name("[git -... stash ...]"), None);
    assert_eq!(generic_pattern_name("[... > ?out]"), None);
    assert_eq!(generic_pattern_name("(write ?path)"), None);
  }
}
