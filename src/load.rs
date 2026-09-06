//! Which rule file is in force, which command declarations apply, and
//! everything wrong with them at once.
//!
//! Rules, first match wins:
//!
//! 1. `CLAUDE_GUARD_RULES`, a path. Set for tests and experiments; a
//!    missing file is an error, not a fallback.
//! 2. `$XDG_CONFIG_HOME/claude-guard/rules.scm`, else
//!    `$HOME/.config/claude-guard/rules.scm`, when it exists.
//! 3. The file embedded at build time, `rules/builtin.scm`.
//!
//! A user rules file replaces the embedded one whole. There is no merging,
//! so there is one place to look for why a rule fired.
//!
//! Command declarations merge, later winning by name:
//!
//! 1. The embedded `rules/commands.scm`.
//! 2. `(command ...)` forms in the rules file in force.
//! 3. Every `.scm` under `CLAUDE_GUARD_COMMANDS_DIR`, else the config
//!    dir's `claude-guard/commands/`, in name order.
//!
//! A declaration is a fact about a program rather than a policy, which is
//! why it merges where rules replace.
//!
//! Reading, parsing, and type-checking each report against their source,
//! one `source:line:col: message` line per problem, across every file
//! that had one, so a broken setup prints everything in one run. The hook
//! fails open on any of them.

use std::env::var_os;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::elaborate::Declarations;
use crate::sexp::{self, ReadError};
use crate::syntax::{self, File, TypeError};

/// The rules path override.
pub const RULES_ENV: &str = "CLAUDE_GUARD_RULES";

/// The command declarations directory override.
pub const COMMANDS_DIR_ENV: &str = "CLAUDE_GUARD_COMMANDS_DIR";

/// The rules shipped with the binary.
pub const BUILTIN: &str = include_str!("../rules/builtin.scm");

/// The command declarations shipped with the binary.
pub const BUILTIN_COMMANDS: &str = include_str!("../rules/commands.scm");

/// Where a rule table or a declaration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
  Builtin,
  BuiltinCommands,
  File(PathBuf),
}

impl fmt::Display for Source {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Source::Builtin => f.write_str("built-in rules"),
      Source::BuiltinCommands => f.write_str("built-in commands"),
      Source::File(path) => write!(f, "{}", path.display()),
    }
  }
}

/// Everything in force: the rules, where they came from, and the merged
/// command declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
  pub source: Source,
  pub file: File,
  pub declarations: Declarations,
}

/// One thing wrong with a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
  /// The file or directory could not be read.
  Io(String),
  /// The text is not well-formed s-expressions.
  Read(ReadError),
  /// The tree is not a rule table.
  Type(TypeError),
}

/// A problem and the source it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
  pub source: Source,
  pub problem: Problem,
}

/// Everything wrong, across every source. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
  pub problems: Vec<Located>,
}

impl LoadError {
  fn one(
    source: Source,
    problem: Problem,
  ) -> LoadError {
    LoadError {
      problems: vec![Located { source, problem }],
    }
  }

  /// The source of the first problem.
  #[cfg(test)]
  pub fn source(&self) -> &Source {
    &self.problems[0].source
  }
}

impl fmt::Display for LoadError {
  /// One line per problem, each prefixed with its source.
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    for (i, located) in self.problems.iter().enumerate() {
      if i > 0 {
        f.write_str("\n")?;
      }
      let source = &located.source;
      match &located.problem {
        Problem::Io(message) => write!(f, "{source}: {message}")?,
        Problem::Read(e) => write!(f, "{source}:{e}")?,
        Problem::Type(e) => write!(f, "{source}:{e}")?,
      }
    }
    Ok(())
  }
}

impl std::error::Error for LoadError {}

/// Load everything in force, per the module docs.
pub fn from_env() -> Result<Loaded, LoadError> {
  let config = config_dir(var_os("XDG_CONFIG_HOME"), var_os("HOME"));
  let rules = resolve_rules(var_os(RULES_ENV), config.as_deref());
  let commands = resolve_commands_dir(var_os(COMMANDS_DIR_ENV), config.as_deref());
  load(rules, commands)
}

/// `$XDG_CONFIG_HOME`, else `$HOME/.config`.
fn config_dir(
  xdg_config_home: Option<OsString>,
  home: Option<OsString>,
) -> Option<PathBuf> {
  match (xdg_config_home, home) {
    (Some(xdg), _) => Some(PathBuf::from(xdg)),
    (None, Some(home)) => Some(PathBuf::from(home).join(".config")),
    (None, None) => None,
  }
}

/// Which user rules file to try, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Candidate {
  /// Named by `CLAUDE_GUARD_RULES`; must exist.
  Explicit(PathBuf),
  /// The config-dir default; used only when present.
  Default(PathBuf),
  /// No home to look in.
  None,
}

fn resolve_rules(
  explicit: Option<OsString>,
  config: Option<&Path>,
) -> Candidate {
  if let Some(path) = explicit {
    return Candidate::Explicit(PathBuf::from(path));
  }
  match config {
    Some(config) => Candidate::Default(config.join("claude-guard").join("rules.scm")),
    None => Candidate::None,
  }
}

/// The user commands directory, if there is one to look in. It need not
/// exist; a missing directory is simply no declarations.
fn resolve_commands_dir(
  explicit: Option<OsString>,
  config: Option<&Path>,
) -> Option<PathBuf> {
  match explicit {
    Some(dir) => Some(PathBuf::from(dir)),
    None => config.map(|c| c.join("claude-guard").join("commands")),
  }
}

fn load(
  rules: Candidate,
  commands_dir: Option<PathBuf>,
) -> Result<Loaded, LoadError> {
  let mut problems = Vec::new();

  // Built-in declarations first, so everything later wins over them.
  let mut declarations = Declarations::new();
  match parse_text(Source::BuiltinCommands, BUILTIN_COMMANDS) {
    Ok(file) => declare_all(&mut declarations, &file),
    Err(e) => problems.extend(e.problems),
  }

  let rules = match rules {
    Candidate::Explicit(path) => read(Source::File(path)),
    Candidate::Default(path) if path.is_file() => read(Source::File(path)),
    Candidate::Default(_) | Candidate::None => {
      parse_text(Source::Builtin, BUILTIN).map(|f| (Source::Builtin, f))
    }
  };
  let rules = match rules {
    Ok(rules) => Some(rules),
    Err(e) => {
      problems.extend(e.problems);
      None
    }
  };
  if let Some((_, file)) = &rules {
    declare_all(&mut declarations, file);
  }

  if let Some(dir) = commands_dir {
    for path in scm_files(&dir) {
      match read(Source::File(path)) {
        Ok((_, file)) => declare_all(&mut declarations, &file),
        Err(e) => problems.extend(e.problems),
      }
    }
  }

  match (rules, problems.is_empty()) {
    (Some((source, file)), true) => Ok(Loaded {
      source,
      file,
      declarations,
    }),
    _ => Err(LoadError { problems }),
  }
}

fn declare_all(
  declarations: &mut Declarations,
  file: &File,
) {
  for command in &file.commands {
    declarations.declare(&command.name, command.declaration.clone());
  }
}

/// The `.scm` files directly under `dir`, in name order. A directory
/// that does not exist has none.
fn scm_files(dir: &Path) -> Vec<PathBuf> {
  let Ok(entries) = fs::read_dir(dir) else {
    return Vec::new();
  };
  let mut files: Vec<PathBuf> = entries
    .filter_map(|e| e.ok().map(|e| e.path()))
    .filter(|p| p.extension().is_some_and(|ext| ext == "scm") && p.is_file())
    .collect();
  files.sort();
  files
}

fn read(source: Source) -> Result<(Source, File), LoadError> {
  let Source::File(path) = &source else {
    unreachable!("only files are read");
  };
  match fs::read_to_string(path) {
    Ok(text) => parse_text(source.clone(), &text).map(|file| (source, file)),
    Err(e) => Err(LoadError::one(
      source,
      Problem::Io(format!("cannot read: {e}")),
    )),
  }
}

/// Parse and type-check `text` as a file from `source`.
fn parse_text(
  source: Source,
  text: &str,
) -> Result<File, LoadError> {
  let forms = sexp::read_all(text).map_err(|e| LoadError::one(source.clone(), Problem::Read(e)))?;
  syntax::parse(&forms).map_err(|errors| LoadError {
    problems: errors
      .into_iter()
      .map(|e| Located {
        source: source.clone(),
        problem: Problem::Type(e),
      })
      .collect(),
  })
}

/// Load `text` as the rules in force with the built-in declarations, for
/// tests. `source` is what errors name.
#[cfg(test)]
pub fn load_text(
  source: Source,
  text: &str,
) -> Result<Loaded, LoadError> {
  let mut declarations = Declarations::new();
  let builtin = parse_text(Source::BuiltinCommands, BUILTIN_COMMANDS)?;
  declare_all(&mut declarations, &builtin);
  let file = parse_text(source.clone(), text)?;
  declare_all(&mut declarations, &file);
  Ok(Loaded {
    source,
    file,
    declarations,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn os(s: &str) -> Option<OsString> {
    Some(OsString::from(s))
  }

  fn write(
    dir: &Path,
    rel: &str,
    text: &str,
  ) -> PathBuf {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
  }

  const GOOD: &str = "(rule r (deny [a] :reason \"r.\" :instead \"i.\"))";

  // --- the shipped files ---

  #[test]
  fn the_builtin_files_type_check_and_declare_the_inner_set() {
    let loaded = load_text(Source::Builtin, BUILTIN).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(loaded.source, Source::Builtin);
    assert!(!loaded.file.rules.is_empty());
    let names: Vec<_> = loaded.declarations.by_name.keys().cloned().collect();
    assert_eq!(
      names,
      [
        "bash", "env", "nice", "nohup", "nu", "python3", "sh", "ssh", "sudo", "timeout", "xargs"
      ]
    );
  }

  // --- choosing sources ---

  #[test]
  fn the_env_override_wins_and_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "mine.scm", GOOD);
    let candidate = resolve_rules(Some(path.clone().into_os_string()), Some(Path::new("/xdg")));
    assert_eq!(candidate, Candidate::Explicit(path.clone()));
    let loaded = load(candidate, None).unwrap();
    assert_eq!(loaded.source, Source::File(path));
    assert_eq!(loaded.file.rules.len(), 1);

    let missing = dir.path().join("missing.scm");
    let error = load(Candidate::Explicit(missing.clone()), None).unwrap_err();
    assert_eq!(error.source(), &Source::File(missing.clone()));
    assert!(
      error
        .to_string()
        .starts_with(&format!("{}: cannot read: ", missing.display())),
      "{error}"
    );
  }

  #[test]
  fn the_config_dir_follows_xdg_then_home() {
    assert_eq!(
      config_dir(os("/xdg"), os("/home")),
      Some(PathBuf::from("/xdg"))
    );
    assert_eq!(
      config_dir(None, os("/home")),
      Some(PathBuf::from("/home/.config"))
    );
    assert_eq!(config_dir(None, None), None);
    assert_eq!(
      resolve_rules(None, Some(Path::new("/xdg"))),
      Candidate::Default(PathBuf::from("/xdg/claude-guard/rules.scm"))
    );
    assert_eq!(resolve_rules(None, None), Candidate::None);
    assert_eq!(
      resolve_commands_dir(None, Some(Path::new("/xdg"))),
      Some(PathBuf::from("/xdg/claude-guard/commands"))
    );
    assert_eq!(
      resolve_commands_dir(os("/elsewhere"), Some(Path::new("/xdg"))),
      Some(PathBuf::from("/elsewhere"))
    );
    assert_eq!(resolve_commands_dir(None, None), None);
  }

  #[test]
  fn the_default_file_is_used_only_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude-guard").join("rules.scm");

    let loaded = load(Candidate::Default(path.clone()), None).unwrap();
    assert_eq!(loaded.source, Source::Builtin);

    write(dir.path(), "claude-guard/rules.scm", GOOD);
    let loaded = load(Candidate::Default(path.clone()), None).unwrap();
    assert_eq!(loaded.source, Source::File(path));
    assert_eq!(loaded.file.rules[0].name.to_string(), "r");
  }

  #[test]
  fn no_home_means_the_builtin_rules() {
    assert_eq!(load(Candidate::None, None).unwrap().source, Source::Builtin);
  }

  #[test]
  fn from_env_reads_the_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "env.scm", GOOD);
    let commands = dir.path().join("cmds");
    write(&commands, "jj.scm", "(command jj (option \"-R\" :value))");
    temp_env::with_vars(
      [
        (RULES_ENV, Some(path.as_os_str())),
        (COMMANDS_DIR_ENV, Some(commands.as_os_str())),
      ],
      || {
        let loaded = from_env().unwrap();
        assert_eq!(loaded.source, Source::File(path.clone()));
        assert!(loaded.declarations.by_name.contains_key("jj"));
      },
    );
  }

  // --- declarations merge ---

  #[test]
  fn declarations_merge_with_later_sources_winning_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let rules = write(
      dir.path(),
      "rules.scm",
      "(command git (option \"-C\" :value))\n(command sudo (option \"-z\"))\n(rule r (deny [a] :reason \"r.\" :instead \"i.\"))",
    );
    let commands = dir.path().join("commands");
    write(&commands, "b-git.scm", "(command git (option \"-P\"))");
    write(&commands, "a-jj.scm", "(command jj (option \"-R\" :value))");
    write(&commands, "notes.txt", "ignored");

    let loaded = load(Candidate::Explicit(rules), Some(commands)).unwrap();
    let by_name = &loaded.declarations.by_name;
    // The rules file replaced sudo's built-in declaration.
    assert_eq!(by_name["sudo"].options.len(), 1);
    assert_eq!(by_name["sudo"].inner, None);
    // The commands dir replaced the rules file's git.
    assert_eq!(by_name["git"].options[0].long, None);
    assert_eq!(by_name["git"].options[0].short, Some('P'));
    assert!(by_name.contains_key("jj"));
    assert!(by_name.contains_key("ssh"));
  }

  #[test]
  fn a_missing_commands_dir_is_no_declarations() {
    let loaded = load(Candidate::None, Some(PathBuf::from("/definitely/not/here"))).unwrap();
    assert!(loaded.declarations.by_name.contains_key("ssh"));
    assert!(!loaded.declarations.by_name.contains_key("jj"));
  }

  // --- errors ---

  #[test]
  fn a_read_error_names_the_source_and_position() {
    let error = load_text(Source::File(PathBuf::from("/x/rules.scm")), "(rule r").unwrap_err();
    assert_eq!(error.to_string(), "/x/rules.scm:1:1: unterminated `(`");
    let error = load_text(Source::Builtin, "\"open").unwrap_err();
    assert_eq!(error.to_string(), "built-in rules:1:1: unterminated string");
  }

  #[test]
  fn every_problem_in_every_file_is_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let rules = write(
      dir.path(),
      "rules.scm",
      "(rule a)\n(rule b (deny [x] :reason \"r.\" :instead \"i.\"))\n(rule c (deny [y]))",
    );
    let commands = dir.path().join("commands");
    let bad = write(&commands, "git.scm", "(command git (option \"C\"))");
    let error = load(Candidate::Explicit(rules.clone()), Some(commands)).unwrap_err();
    assert_eq!(
      error.to_string(),
      format!(
        "{r}:1:1: rule `a` has no rows\n{r}:3:9: `deny` needs a :reason\n{c}:1:22: option names look like \"-c\" or \"--long\", not \"C\"",
        r = rules.display(),
        c = bad.display()
      )
    );
    assert_eq!(error.problems.len(), 3);
  }

  #[test]
  fn a_bad_declaration_file_fails_the_load_even_with_good_rules() {
    let dir = tempfile::tempdir().unwrap();
    let commands = dir.path().join("commands");
    write(&commands, "git.scm", "(command)");
    assert!(load(Candidate::None, Some(commands)).is_err());
  }
}
