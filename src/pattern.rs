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
//! yet. Patterns know nothing about which flags take values, so
//! `git -C . stash` does not match `git -... stash ...`. Authors who want
//! that write `git ... stash ...` and accept the looser match.
//!
//! The rule file syntax in [`syntax`] builds patterns; this module only
//! matches them. The executable spec in `spec/patterns.scm` is the
//! readable statement of everything here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::input::string_id;
use crate::segment::{Redirect, RedirectKind, SimpleCommand, Word};

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

  /// Every distinct way the pattern matches `command`, as what its
  /// binders captured. Empty means no match. A binder that appears twice
  /// must capture the same word both times.
  pub fn bindings(
    &self,
    command: &SimpleCommand,
  ) -> Vec<Bindings> {
    let mut found = Vec::new();
    bind_words(&self.words, &command.words, Bindings::new(), &mut found);
    for wanted in &self.redirects {
      let mut next = Vec::new();
      for bound in &found {
        for redirect in &command.redirects {
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

/// Match a token sequence against a word sequence, collecting every
/// binding set that reaches the end. `...` and `-...` try every length
/// they could take, shortest first, so a pattern may hold several of them.
fn bind_words(
  tokens: &[Token],
  words: &[Word],
  bound: Bindings,
  out: &mut Vec<Bindings>,
) {
  let Some((first, rest)) = tokens.split_first() else {
    if words.is_empty() {
      push_unique(out, bound);
    }
    return;
  };
  match first {
    Token::Rest => {
      for n in 0..=words.len() {
        bind_words(rest, &words[n..], bound.clone(), out);
      }
    }
    Token::Options => {
      let limit = words.iter().take_while(|w| is_option(w)).count();
      for n in 0..=limit {
        bind_words(rest, &words[n..], bound.clone(), out);
      }
    }
    single => {
      if let Some((word, tail)) = words.split_first()
        && let Some(next) = single.bind_one(word, &bound)
      {
        bind_words(rest, tail, next, out);
      }
    }
  }
}

fn is_option(word: &Word) -> bool {
  matches!(word, Word::Literal(text) if text.starts_with('-'))
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
