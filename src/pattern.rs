//! Term patterns: one simple command with wildcard words, matched against
//! a [`SimpleCommand`] the segmenter produced.
//!
//! | Token       | Matches                                            |
//! |-------------|----------------------------------------------------|
//! | `*`         | exactly one word, literal or dynamic                |
//! | `...`       | zero or more words of any kind                     |
//! | `-*`        | exactly one literal word starting with `-`         |
//! | `-...`      | zero or more literal words each starting with `-`  |
//! | `?name`     | one literal word, bound to `name` for the row's condition |
//! | a literal   | one literal word, byte-equal after quote removal   |
//!
//! A redirect in a pattern must find a redirect on the command, in any
//! position. `> X` accepts a write or an append; `>> X` accepts an append
//! only; `< X` accepts a read. The target takes the same tokens.
//!
//! A dynamic word never matches a literal, an option wildcard, or a
//! binder: a condition needs the text, and the shell has not produced it
//! yet.
//!
//! The matcher walks [`Unit`]s, which the elaborator builds from a
//! declared command: an option with its value is one unit whose elements
//! are the canonical flag and the value, so `-...` takes `-C .` whole and
//! `[git -C ?dir]` binds under `-C .` and `-C.` alike. Without a
//! declaration every word is a unit of one element and the tokens behave
//! as they always did: `git -C . stash` does not match
//! `[git -... stash ...]`, because `.` is an argument. The tokens keep
//! their meaning either way; elaboration changes what they see.
//!
//! The rule file syntax in [`syntax`] builds patterns; this module only
//! matches them. The executable spec in `spec/patterns.scm` is the
//! readable statement of everything here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::input::string_id;
#[cfg(test)]
use crate::segment::SimpleCommand;
use crate::segment::{Redirect, RedirectKind, Word};

/// A pattern. Build one with [`Pattern::from_tokens`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
  source: String,
  words: Vec<Token>,
  redirects: Vec<RedirectPattern>,
}

string_id! {
  /// A binder's name, without the `?`.
  Var
}

/// What each binder in a pattern captured, one word per name. A pattern
/// with no binders yields one empty map per way it matches, deduplicated
/// to one.
pub type Bindings = BTreeMap<Var, String>;

/// What one pattern word matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
  /// A literal word, byte-equal.
  Literal(String),
  /// `*`: one word of any kind.
  Any,
  /// `...`: zero or more words of any kind.
  Rest,
  /// `-*`: one literal word starting with `-`.
  Option,
  /// `-...`: zero or more literal words starting with `-`.
  Options,
  /// `?name`: one literal word, captured under `name`.
  Var(Var),
}

/// A redirect the command must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectPattern {
  pub kind: RedirectKind,
  pub target: Token,
}

impl Pattern {
  /// A pattern from tokens the syntax split. `source` is what
  /// [`fmt::Display`] shows and what the log records.
  pub fn from_tokens(
    source: String,
    words: Vec<Token>,
    redirects: Vec<RedirectPattern>,
  ) -> Pattern {
    Pattern {
      source,
      words,
      redirects,
    }
  }

  /// Every binder the pattern declares, in words and redirect targets.
  pub fn binders(&self) -> BTreeSet<Var> {
    self
      .words
      .iter()
      .chain(self.redirects.iter().map(|r| &r.target))
      .filter_map(|token| match token {
        Token::Var(var) => Some(var.clone()),
        _ => None,
      })
      .collect()
  }

  /// True when every pattern word and redirect finds its counterpart on
  /// `command`. The engine wants the bindings, so this is for tests.
  #[cfg(test)]
  pub fn matches(
    &self,
    command: &SimpleCommand,
  ) -> bool {
    !self.bindings(command).is_empty()
  }

  /// Every distinct way the pattern matches `command` with no declaration
  /// in force: each word is one unit, and a dash word is an option unit.
  /// Empty means no match. A binder that appears twice must capture the
  /// same word both times. The engine elaborates first and uses
  /// [`Pattern::bindings_in`]; this is for tests.
  #[cfg(test)]
  pub fn bindings(
    &self,
    command: &SimpleCommand,
  ) -> Vec<Bindings> {
    let units: Vec<Unit> = command.words.iter().map(Unit::word).collect();
    self.bindings_in(&units, &command.redirects)
  }

  /// Every distinct way the pattern matches the elaborated `units` and
  /// `redirects`. See the module docs for what each token takes.
  pub fn bindings_in(
    &self,
    units: &[Unit],
    redirects: &[Redirect],
  ) -> Vec<Bindings> {
    let mut found = Vec::new();
    bind_units(
      &self.words,
      units,
      Cursor {
        unit: 0,
        element: 0,
      },
      Bindings::new(),
      &mut found,
    );
    for wanted in &self.redirects {
      let mut next = Vec::new();
      for bound in &found {
        for redirect in redirects {
          if let Some(extended) = wanted.bind(redirect, bound) {
            push_unique(&mut next, extended);
          }
        }
      }
      found = next;
    }
    found
  }
}

fn push_unique(
  out: &mut Vec<Bindings>,
  bindings: Bindings,
) {
  if !out.contains(&bindings) {
    out.push(bindings);
  }
}

/// What the matcher walks: one argument, or one option with its value.
/// An elaborated option group's elements are its canonical flags and
/// value, so `-fxd` is `-f`, `-x`, `-d` and `-C.` is `-C`, `.`. A word
/// with no declaration behind it is a unit of one element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
  pub elements: Vec<Word>,
  /// True for an option unit: what `-*` and `-...` take.
  pub option: bool,
}

impl Unit {
  /// One word, as the matcher saw it before elaboration existed: a
  /// literal starting with `-` is an option, anything else is not.
  pub fn word(word: &Word) -> Unit {
    Unit {
      option: matches!(word, Word::Literal(text) if text.starts_with('-')),
      elements: vec![word.clone()],
    }
  }
}

/// A position in the unit list: the unit, and the element within it.
/// `element` is 0 at a unit boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
  unit: usize,
  element: usize,
}

impl Cursor {
  fn at_end(
    self,
    units: &[Unit],
  ) -> bool {
    self.unit >= units.len()
  }

  fn at_boundary(self) -> bool {
    self.element == 0
  }

  fn current(
    self,
    units: &[Unit],
  ) -> Option<&Word> {
    units.get(self.unit)?.elements.get(self.element)
  }

  /// One element on, crossing into the next unit when this one is done.
  fn next_element(
    self,
    units: &[Unit],
  ) -> Cursor {
    let len = units.get(self.unit).map_or(0, |u| u.elements.len());
    if self.element + 1 >= len {
      Cursor {
        unit: self.unit + 1,
        element: 0,
      }
    } else {
      Cursor {
        unit: self.unit,
        element: self.element + 1,
      }
    }
  }

  fn next_unit(self) -> Cursor {
    Cursor {
      unit: self.unit + 1,
      element: 0,
    }
  }
}

/// Match a token sequence from `at`, collecting every binding set that
/// reaches the end. `...` and `-...` try every length they could take,
/// shortest first, so a pattern may hold several of them.
///
/// `*`, `-*`, and `-...` take whole units and only from a unit boundary;
/// a literal, a binder, and `...` walk elements, so a literal can look
/// inside a cluster and a binder can take the value after a flag.
fn bind_units(
  tokens: &[Token],
  units: &[Unit],
  at: Cursor,
  bound: Bindings,
  out: &mut Vec<Bindings>,
) {
  let Some((first, rest)) = tokens.split_first() else {
    if at.at_end(units) {
      push_unique(out, bound);
    }
    return;
  };
  match first {
    Token::Rest => {
      let mut here = at;
      loop {
        bind_units(rest, units, here, bound.clone(), out);
        if here.at_end(units) {
          break;
        }
        here = here.next_element(units);
      }
    }
    Token::Options => {
      if !at.at_boundary() {
        return;
      }
      let mut here = at;
      loop {
        bind_units(rest, units, here, bound.clone(), out);
        match units.get(here.unit) {
          Some(unit) if unit.option && is_literal(&unit.elements[0]) => here = here.next_unit(),
          _ => break,
        }
      }
    }
    Token::Any => {
      if at.at_end(units) {
        return;
      }
      // A whole unit from a boundary; one element from inside a unit.
      let next = if at.at_boundary() {
        at.next_unit()
      } else {
        at.next_element(units)
      };
      bind_units(rest, units, next, bound, out);
    }
    Token::Option => {
      if let Some(unit) = units.get(at.unit)
        && at.at_boundary()
        && unit.option
        && is_literal(&unit.elements[0])
      {
        bind_units(rest, units, at.next_unit(), bound, out);
      }
    }
    Token::Literal(_) | Token::Var(_) => {
      if let Some(word) = at.current(units)
        && let Some(next) = first.bind_one(word, &bound)
      {
        bind_units(rest, units, at.next_element(units), next, out);
      }
    }
  }
}

fn is_literal(word: &Word) -> bool {
  matches!(word, Word::Literal(_))
}

impl Token {
  /// Match against exactly one word. `...` and `-...` behave as `*` and
  /// `-*` here, which is what they mean in a redirect target.
  fn matches_one(
    &self,
    word: &Word,
  ) -> bool {
    match (self, word) {
      (Token::Any | Token::Rest, _) => true,
      (_, Word::Dynamic(_)) => false,
      (Token::Literal(wanted), Word::Literal(found)) => wanted == found,
      (Token::Option | Token::Options, Word::Literal(found)) => found.starts_with('-'),
      (Token::Var(_), Word::Literal(_)) => true,
    }
  }

  /// [`Token::matches_one`] plus the capture: `bound` extended with this
  /// word when the token is a binder, or unchanged. `None` when the word
  /// does not match or a repeated binder disagrees with its first capture.
  fn bind_one(
    &self,
    word: &Word,
    bound: &Bindings,
  ) -> Option<Bindings> {
    if !self.matches_one(word) {
      return None;
    }
    let mut next = bound.clone();
    if let (Token::Var(name), Word::Literal(text)) = (self, word)
      && let Some(earlier) = next.insert(name.clone(), text.clone())
      && earlier != *text
    {
      return None;
    }
    Some(next)
  }
}

/// macOS mounts `/tmp` as a link to `/private/tmp`; both spellings name
/// the same place. Relative paths are left alone: the caller resolves
/// them against a cwd when it has one.
pub fn normalize_path(path: &Path) -> PathBuf {
  match path.strip_prefix("/private") {
    Ok(rest) => Path::new("/").join(rest),
    Err(_) => path.to_path_buf(),
  }
}

impl RedirectPattern {
  fn bind(
    &self,
    found: &Redirect,
    bound: &Bindings,
  ) -> Option<Bindings> {
    let kind_ok = match self.kind {
      RedirectKind::Write => matches!(found.kind, RedirectKind::Write | RedirectKind::Append),
      RedirectKind::Append => found.kind == RedirectKind::Append,
      RedirectKind::Read => found.kind == RedirectKind::Read,
    };
    if !kind_ok {
      return None;
    }
    self.target.bind_one(&found.target, bound)
  }
}

impl fmt::Display for Pattern {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    f.write_str(&self.source)
  }
}

#[cfg(test)]
mod tests {
  //! The matcher's behavior is specified in `spec/patterns.scm`. These
  //! tests cover the Rust API around it.

  use super::*;
  use crate::segment;

  /// Segment `input` and return its only simple command.
  fn command(input: &str) -> SimpleCommand {
    let mut segments = segment::segment(input).unwrap();
    assert_eq!(segments.commands.len(), 1, "{input:?} is not one command");
    segments.commands.remove(0)
  }

  fn lit(s: &str) -> Token {
    Token::Literal(s.to_string())
  }

  fn var(name: &str) -> Token {
    Token::Var(Var::from(name))
  }

  fn bound(pairs: &[(&str, &str)]) -> Bindings {
    pairs
      .iter()
      .map(|(k, v)| (Var::from(*k), v.to_string()))
      .collect()
  }

  fn from_tokens(
    words: Vec<Token>,
    redirects: Vec<RedirectPattern>,
  ) -> Pattern {
    Pattern::from_tokens("[test]".into(), words, redirects)
  }

  #[test]
  fn display_is_the_source_text() {
    let p = from_tokens(
      vec![lit("git"), Token::Options, lit("stash"), Token::Rest],
      vec![],
    );
    assert_eq!(p.to_string(), "[test]");
    assert!(p.matches(&command("git --no-pager stash pop")));
    assert!(!p.matches(&command("git log")));
  }

  #[test]
  fn binders_lists_words_and_redirect_targets() {
    let p = from_tokens(
      vec![lit("cp"), var("src"), Token::Rest],
      vec![RedirectPattern {
        kind: RedirectKind::Write,
        target: var("out"),
      }],
    );
    assert_eq!(
      p.binders(),
      BTreeSet::from([Var::from("out"), Var::from("src")])
    );
    assert!(from_tokens(vec![lit("ls")], vec![]).binders().is_empty());
  }

  #[test]
  fn bindings_returns_every_distinct_binding_set() {
    let p = from_tokens(vec![lit("f"), Token::Rest, var("x"), Token::Rest], vec![]);
    assert_eq!(
      p.bindings(&command("f a b")),
      vec![bound(&[("x", "a")]), bound(&[("x", "b")])]
    );
    let p = from_tokens(vec![lit("git"), Token::Rest, Token::Rest], vec![]);
    assert_eq!(p.bindings(&command("git a b c")), vec![Bindings::new()]);
    assert!(p.bindings(&command("ls")).is_empty());
  }

  #[test]
  fn normalize_path_folds_private() {
    assert_eq!(
      normalize_path(Path::new("/private/tmp/x")),
      PathBuf::from("/tmp/x")
    );
    assert_eq!(normalize_path(Path::new("/tmp/x")), PathBuf::from("/tmp/x"));
    assert_eq!(normalize_path(Path::new("tmp/x")), PathBuf::from("tmp/x"));
    assert_eq!(
      normalize_path(Path::new("/privateer/x")),
      PathBuf::from("/privateer/x")
    );
  }
}
