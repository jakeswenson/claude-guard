//! Which rule file is in force, and everything wrong with it at once.
//!
//! Sources, first match wins:
//!
//! 1. `CLAUDE_GUARD_RULES`, a path. Set for tests and experiments; a
//!    missing file is an error, not a fallback.
//! 2. `$XDG_CONFIG_HOME/claude-guard/rules.scm`, else
//!    `$HOME/.config/claude-guard/rules.scm`, when it exists.
//! 3. The file embedded at build time, `rules/builtin.scm`.
//!
//! A user file replaces the embedded one whole. There is no merging, so
//! there is one place to look for why a rule fired.
//!
//! Reading, parsing, and type-checking each report against the source,
//! one `source:line:col: message` line per problem, so a broken file
//! prints every problem in one run. The hook fails open on any of them.

// Used by the engine, which switches over in claude-guard-110.6.
#![allow(dead_code)]

use std::env::var_os;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::sexp::{self, ReadError};
use crate::syntax::{self, File, TypeError};

/// The path override.
pub const RULES_ENV: &str = "CLAUDE_GUARD_RULES";

/// The rules shipped with the binary.
pub const BUILTIN: &str = include_str!("../rules/builtin.scm");

/// Where a rule table came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
  Builtin,
  File(PathBuf),
}

impl fmt::Display for Source {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Source::Builtin => f.write_str("built-in rules"),
      Source::File(path) => write!(f, "{}", path.display()),
    }
  }
}

/// A rule table and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
  pub source: Source,
  pub file: File,
}

/// One thing wrong with a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
  /// The file could not be read.
  Io(String),
  /// The text is not well-formed s-expressions.
  Read(ReadError),
  /// The tree is not a rule table.
  Type(TypeError),
}

/// Everything wrong with one source. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
  pub source: Source,
  pub problems: Vec<Problem>,
}

impl fmt::Display for LoadError {
  /// One line per problem, each prefixed with the source.
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    for (i, problem) in self.problems.iter().enumerate() {
      if i > 0 {
        f.write_str("\n")?;
      }
      match problem {
        Problem::Io(message) => write!(f, "{}: {message}", self.source)?,
        Problem::Read(e) => write!(f, "{}:{e}", self.source)?,
        Problem::Type(e) => write!(f, "{}:{e}", self.source)?,
      }
    }
    Ok(())
  }
}

impl std::error::Error for LoadError {}

/// Load the rules in force, per the module docs.
pub fn from_env() -> Result<Loaded, LoadError> {
  let candidate = resolve(var_os(RULES_ENV), var_os("XDG_CONFIG_HOME"), var_os("HOME"));
  load(candidate)
}

/// Which user file to try, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Candidate {
  /// Named by `CLAUDE_GUARD_RULES`; must exist.
  Explicit(PathBuf),
  /// The config-dir default; used only when present.
  Default(PathBuf),
  /// No home to look in.
  None,
}

fn resolve(
  explicit: Option<OsString>,
  xdg_config_home: Option<OsString>,
  home: Option<OsString>,
) -> Candidate {
  if let Some(path) = explicit {
    return Candidate::Explicit(PathBuf::from(path));
  }
  let config = match (xdg_config_home, home) {
    (Some(xdg), _) => PathBuf::from(xdg),
    (None, Some(home)) => PathBuf::from(home).join(".config"),
    (None, None) => return Candidate::None,
  };
  Candidate::Default(config.join("claude-guard").join("rules.scm"))
}

fn load(candidate: Candidate) -> Result<Loaded, LoadError> {
  let path = match candidate {
    Candidate::Explicit(path) => path,
    Candidate::Default(path) if path.is_file() => path,
    Candidate::Default(_) | Candidate::None => return load_text(Source::Builtin, BUILTIN),
  };
  let source = Source::File(path.clone());
  match fs::read_to_string(&path) {
    Ok(text) => load_text(source, &text),
    Err(e) => Err(LoadError {
      source,
      problems: vec![Problem::Io(format!("cannot read: {e}"))],
    }),
  }
}

/// Parse and type-check `text` as the rules from `source`.
pub fn load_text(
  source: Source,
  text: &str,
) -> Result<Loaded, LoadError> {
  let forms = sexp::read_all(text).map_err(|e| LoadError {
    source: source.clone(),
    problems: vec![Problem::Read(e)],
  })?;
  let file = syntax::parse(&forms).map_err(|errors| LoadError {
    source: source.clone(),
    problems: errors.into_iter().map(Problem::Type).collect(),
  })?;
  Ok(Loaded { source, file })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn os(s: &str) -> Option<OsString> {
    Some(OsString::from(s))
  }

  fn write(
    dir: &std::path::Path,
    rel: &str,
    text: &str,
  ) -> PathBuf {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
  }

  const GOOD: &str = "(rule r (deny [a] :reason \"r.\" :instead \"i.\"))";

  // --- the shipped file ---

  #[test]
  fn the_builtin_file_type_checks() {
    let loaded = load_text(Source::Builtin, BUILTIN).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(loaded.source, Source::Builtin);
    assert!(!loaded.file.rules.is_empty());
  }

  // --- choosing a source ---

  #[test]
  fn the_env_override_wins_and_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "mine.scm", GOOD);
    let candidate = resolve(Some(path.clone().into_os_string()), os("/xdg"), os("/home"));
    assert_eq!(candidate, Candidate::Explicit(path.clone()));
    let loaded = load(candidate).unwrap();
    assert_eq!(loaded.source, Source::File(path));
    assert_eq!(loaded.file.rules.len(), 1);

    let missing = dir.path().join("missing.scm");
    let error = load(Candidate::Explicit(missing.clone())).unwrap_err();
    assert_eq!(error.source, Source::File(missing.clone()));
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
      resolve(None, os("/xdg"), os("/home")),
      Candidate::Default(PathBuf::from("/xdg/claude-guard/rules.scm"))
    );
    assert_eq!(
      resolve(None, None, os("/home")),
      Candidate::Default(PathBuf::from("/home/.config/claude-guard/rules.scm"))
    );
    assert_eq!(resolve(None, None, None), Candidate::None);
  }

  #[test]
  fn the_default_file_is_used_only_when_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude-guard").join("rules.scm");

    let loaded = load(Candidate::Default(path.clone())).unwrap();
    assert_eq!(loaded.source, Source::Builtin);

    write(dir.path(), "claude-guard/rules.scm", GOOD);
    let loaded = load(Candidate::Default(path.clone())).unwrap();
    assert_eq!(loaded.source, Source::File(path));
    assert_eq!(loaded.file.rules[0].name.to_string(), "r");
  }

  #[test]
  fn no_home_means_the_builtin_rules() {
    assert_eq!(load(Candidate::None).unwrap().source, Source::Builtin);
  }

  #[test]
  fn from_env_reads_the_override() {
    // Only the override is set here, since the other variables are the
    // process's own. A wrong value in either direction fails loudly.
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "env.scm", GOOD);
    temp_env::with_var(RULES_ENV, Some(&path), || {
      assert_eq!(from_env().unwrap().source, Source::File(path.clone()));
    });
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
  fn every_type_error_is_one_line() {
    let text = "(rule a)\n(rule b (deny [x] :reason \"r.\" :instead \"i.\"))\n(rule c (deny [y]))";
    let error = load_text(Source::File(PathBuf::from("/x/rules.scm")), text).unwrap_err();
    assert_eq!(
      error.to_string(),
      "/x/rules.scm:1:1: rule `a` has no rows\n/x/rules.scm:3:9: `deny` needs a :reason"
    );
    assert_eq!(error.problems.len(), 2);
  }
}
