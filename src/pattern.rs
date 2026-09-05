//! Rule patterns: one simple command in bash syntax with wildcard words,
//! matched against a [`SimpleCommand`] the segmenter produced.
//!
//! Wildcards are ordinary bash words, so a pattern parses with the same
//! quoting rules as a command and cannot disagree with the segmenter:
//!
//! | Word        | Matches                                            |
//! |-------------|----------------------------------------------------|
//! | `*`         | exactly one word, literal or dynamic                |
//! | `...`       | zero or more words of any kind                     |
//! | `-*`        | exactly one literal word starting with `-`         |
//! | `-...`      | zero or more literal words each starting with `-`  |
//! | `<path>/**` | one literal word whose normalized path is under `<path>` |
//! | anything else | one literal word, byte-equal after quote removal |
//!
//! A redirect in a pattern must find a redirect on the command, in any
//! position. `> X` accepts a write or an append; `>> X` accepts an append
//! only; `< X` accepts a read. The target takes the same wildcards.
//!
//! A dynamic word never matches a literal, a path prefix, or an option
//! wildcard. Patterns know nothing about which flags take values, so
//! `git -C . stash` does not match `git -... stash ...`. Authors who want
//! that write `git ... stash ...` and accept the looser match.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::segment::{self, Redirect, RedirectKind, SimpleCommand, Word};

/// A parsed pattern. Build one with [`Pattern::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
  source: String,
  words: Vec<Token>,
  redirects: Vec<RedirectPattern>,
}

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
  /// `<path>/**`: one literal word whose normalized path is under `path`.
  Under(PathBuf),
}

/// A redirect the command must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectPattern {
  kind: RedirectKind,
  target: Token,
}

/// The pattern text is not one plain simple command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError(String);

impl fmt::Display for PatternError {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl std::error::Error for PatternError {}

impl Pattern {
  /// Parse `source` as bash. It must segment to exactly one simple command
  /// with only literal words; anything the shell would expand at run time
  /// has no meaning in a pattern.
  pub fn parse(source: &str) -> Result<Pattern, PatternError> {
    let mut segments = segment::segment(source).map_err(|e| PatternError(e.to_string()))?;
    let command = match segments.commands.len() {
      0 => return Err(PatternError("pattern is empty".into())),
      1 => segments.commands.remove(0),
      n => {
        return Err(PatternError(format!(
          "pattern must be one simple command, found {n}"
        )));
      }
    };

    let words = command
      .words
      .into_iter()
      .map(token)
      .collect::<Result<_, _>>()?;
    let redirects = command
      .redirects
      .into_iter()
      .map(|r| {
        Ok(RedirectPattern {
          kind: r.kind,
          target: token(r.target)?,
        })
      })
      .collect::<Result<_, _>>()?;

    // A substitution in an assignment leaves no dynamic word behind, so
    // the word mapping above cannot see it.
    if let Some(text) = segments.uninspected.first() {
      return Err(PatternError(format!(
        "pattern has a command substitution: $({text})"
      )));
    }

    Ok(Pattern {
      source: source.to_string(),
      words,
      redirects,
    })
  }

  /// True when every pattern word and redirect finds its counterpart on
  /// `command`.
  pub fn matches(
    &self,
    command: &SimpleCommand,
  ) -> bool {
    match_words(&self.words, &command.words)
      && self
        .redirects
        .iter()
        .all(|wanted| command.redirects.iter().any(|found| wanted.matches(found)))
  }
}

/// Map one literal pattern word to its token. Dynamic words have no
/// meaning in a pattern and are refused.
fn token(word: Word) -> Result<Token, PatternError> {
  let text = match word {
    Word::Dynamic(raw) => {
      return Err(PatternError(format!(
        "pattern word {raw:?} would be expanded by the shell"
      )));
    }
    Word::Literal(text) => text,
  };
  Ok(match text.as_str() {
    "*" => Token::Any,
    "..." => Token::Rest,
    "-*" => Token::Option,
    "-..." => Token::Options,
    _ => match text.strip_suffix("/**") {
      Some("") => {
        return Err(PatternError(format!(
          "pattern word {text:?} has no path before /**"
        )));
      }
      Some(prefix) => Token::Under(PathBuf::from(prefix)),
      None => Token::Literal(text),
    },
  })
}

/// Match a token sequence against a word sequence. `...` and `-...` try
/// every length they could take, shortest first, so a pattern may hold
/// several of them.
fn match_words(
  tokens: &[Token],
  words: &[Word],
) -> bool {
  let Some((first, rest)) = tokens.split_first() else {
    return words.is_empty();
  };
  match first {
    Token::Rest => (0..=words.len()).any(|n| match_words(rest, &words[n..])),
    Token::Options => {
      let limit = words.iter().take_while(|w| is_option(w)).count();
      (0..=limit).any(|n| match_words(rest, &words[n..]))
    }
    single => match words.split_first() {
      Some((word, tail)) => single.matches_one(word) && match_words(rest, tail),
      None => false,
    },
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
      (Token::Under(prefix), Word::Literal(found)) => {
        normalize_path(Path::new(found)).starts_with(prefix)
      }
    }
  }
}

/// macOS mounts `/tmp` as a link to `/private/tmp`; both spellings name
/// the same place. Relative paths are left alone: the matcher has no cwd.
pub fn normalize_path(path: &Path) -> PathBuf {
  match path.strip_prefix("/private") {
    Ok(rest) => Path::new("/").join(rest),
    Err(_) => path.to_path_buf(),
  }
}

impl RedirectPattern {
  fn matches(
    &self,
    found: &Redirect,
  ) -> bool {
    let kind_ok = match self.kind {
      RedirectKind::Write => matches!(found.kind, RedirectKind::Write | RedirectKind::Append),
      RedirectKind::Append => found.kind == RedirectKind::Append,
      RedirectKind::Read => found.kind == RedirectKind::Read,
    };
    kind_ok && self.target.matches_one(&found.target)
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
  use super::*;

  fn pattern(source: &str) -> Pattern {
    Pattern::parse(source).unwrap_or_else(|e| panic!("pattern {source:?}: {e}"))
  }

  fn parse_error(source: &str) -> String {
    match Pattern::parse(source) {
      Ok(p) => panic!("pattern {source:?} parsed: {p:?}"),
      Err(e) => e.to_string(),
    }
  }

  /// Segment `input` and return its only simple command.
  fn command(input: &str) -> SimpleCommand {
    let mut segments = segment::segment(input).unwrap();
    assert_eq!(segments.commands.len(), 1, "{input:?} is not one command");
    segments.commands.remove(0)
  }

  fn matches(
    source: &str,
    input: &str,
  ) -> bool {
    pattern(source).matches(&command(input))
  }

  fn lit(s: &str) -> Token {
    Token::Literal(s.to_string())
  }

  // --- parsing ---

  #[test]
  fn every_token_kind_parses() {
    let p = pattern("git * ... -* -... /tmp/** stash");
    assert_eq!(
      p.words,
      vec![
        lit("git"),
        Token::Any,
        Token::Rest,
        Token::Option,
        Token::Options,
        Token::Under(PathBuf::from("/tmp")),
        lit("stash"),
      ]
    );
    assert!(p.redirects.is_empty());
  }

  #[test]
  fn redirects_parse_with_their_kind_and_target() {
    let p = pattern("cat ... > * >> /tmp/** < in");
    assert_eq!(p.words, vec![lit("cat"), Token::Rest]);
    assert_eq!(
      p.redirects,
      vec![
        RedirectPattern {
          kind: RedirectKind::Write,
          target: Token::Any
        },
        RedirectPattern {
          kind: RedirectKind::Append,
          target: Token::Under(PathBuf::from("/tmp"))
        },
        RedirectPattern {
          kind: RedirectKind::Read,
          target: lit("in")
        },
      ]
    );
  }

  #[test]
  fn quotes_in_a_pattern_fold_like_a_command() {
    assert_eq!(
      pattern("git 'sta'\"sh\"").words,
      vec![lit("git"), lit("stash")]
    );
  }

  #[test]
  fn display_is_the_source_text() {
    assert_eq!(pattern("git stash ...").to_string(), "git stash ...");
  }

  #[test]
  fn an_empty_pattern_is_an_error() {
    assert_eq!(parse_error(""), "pattern is empty");
    assert_eq!(parse_error("# comment"), "pattern is empty");
  }

  #[test]
  fn a_pattern_must_be_one_simple_command() {
    assert_eq!(
      parse_error("git stash; jj new"),
      "pattern must be one simple command, found 2"
    );
    assert_eq!(
      parse_error("git log | head"),
      "pattern must be one simple command, found 2"
    );
  }

  #[test]
  fn a_pattern_cannot_contain_expansions() {
    assert_eq!(
      parse_error("git $sub"),
      "pattern word \"$sub\" would be expanded by the shell"
    );
    assert_eq!(
      parse_error("git $(x)"),
      "pattern word \"$(x)\" would be expanded by the shell"
    );
    assert_eq!(
      parse_error("cat > $out"),
      "pattern word \"$out\" would be expanded by the shell"
    );
    assert_eq!(
      parse_error("X=$(y) git"),
      "pattern has a command substitution: $(y)"
    );
  }

  #[test]
  fn a_path_prefix_cannot_be_empty() {
    assert_eq!(
      parse_error("cp ... /**"),
      "pattern word \"/**\" has no path before /**"
    );
  }

  #[test]
  fn a_pattern_that_does_not_parse_reports_the_parser_message() {
    assert_eq!(
      parse_error("git \"stash"),
      "unterminated double quote at 1,5 (detected near line 1 col 11)"
    );
  }

  // --- literal and * ---

  #[test]
  fn literals_match_byte_for_byte() {
    assert!(matches("git stash", "git stash"));
    assert!(!matches("git stash", "git stas"));
    assert!(!matches("git stash", "git Stash"));
    assert!(!matches("git stash", "git stash pop"));
    assert!(!matches("git stash", "git"));
  }

  #[test]
  fn a_quoted_command_word_matches_an_unquoted_literal() {
    assert!(matches("git stash", "git \"stash\""));
    assert!(matches("git stash", "'git' sta\\sh"));
  }

  #[test]
  fn star_takes_exactly_one_word() {
    assert!(matches("git *", "git stash"));
    assert!(!matches("git *", "git"));
    assert!(!matches("git *", "git stash pop"));
  }

  // --- ... ---

  #[test]
  fn rest_takes_zero_or_more_words() {
    assert!(matches("git stash ...", "git stash"));
    assert!(matches("git stash ...", "git stash pop"));
    assert!(matches("git stash ...", "git stash push -m wip"));
    assert!(!matches("git stash ...", "git log"));
  }

  #[test]
  fn rest_in_the_middle_backtracks() {
    assert!(matches("cp ... /tmp/**", "cp -r a b /tmp/c"));
    assert!(matches("cp ... /tmp/**", "cp /tmp/c"));
    assert!(!matches("cp ... /tmp/**", "cp /tmp/c d"));
  }

  #[test]
  fn two_rests_in_one_pattern() {
    assert!(matches("git ... stash ...", "git -C . stash pop"));
    assert!(matches("git ... stash ...", "git stash"));
    assert!(!matches("git ... stash ...", "git log"));
  }

  // --- -* and -... ---

  #[test]
  fn option_takes_one_dash_word() {
    assert!(matches("git -* stash", "git --no-pager stash"));
    assert!(matches("git -* stash", "git -v stash"));
    assert!(!matches("git -* stash", "git stash"));
    assert!(!matches("git -* stash", "git log stash"));
  }

  #[test]
  fn options_take_zero_or_more_dash_words() {
    assert!(matches("git -... stash ...", "git stash"));
    assert!(matches("git -... stash ...", "git --no-pager -v stash pop"));
    assert!(!matches("git -... stash ...", "git log stash"));
  }

  #[test]
  fn a_flag_value_is_not_an_option() {
    assert!(!matches("git -... stash ...", "git -C . stash"));
  }

  // --- dynamic words ---

  #[test]
  fn a_dynamic_word_matches_only_the_open_wildcards() {
    assert!(!matches("git stash", "git $cmd"));
    assert!(!matches("git -* stash", "git \"-$flag\" stash"));
    assert!(!matches("git -... stash", "git \"-$flag\" stash"));
    assert!(!matches("cp ... /tmp/**", "cp a $TMPDIR/x"));
    assert!(matches("git *", "git $cmd"));
    assert!(matches("git ...", "git $cmd $args"));
  }

  // --- <path>/** ---

  #[test]
  fn under_matches_by_path_component() {
    assert!(matches("cp ... /tmp/**", "cp a /tmp/x"));
    assert!(matches("cp ... /tmp/**", "cp a /tmp/x/y"));
    assert!(matches("cp ... /tmp/**", "cp a /private/tmp/x"));
    assert!(matches("cp ... /tmp/**", "cp a /tmp"));
    assert!(!matches("cp ... /tmp/**", "cp a /tmpfoo"));
    assert!(!matches("cp ... /tmp/**", "cp a tmp/x"));
    assert!(!matches("cp ... /tmp/**", "cp a /var/tmp/x"));
  }

  // --- redirects ---

  #[test]
  fn a_write_pattern_accepts_append_but_not_the_reverse() {
    assert!(matches("cat ... > *", "cat a > b"));
    assert!(matches("cat ... > *", "cat a >> b"));
    assert!(matches("cat ... > *", "cat a 2> b"));
    assert!(matches("cat ... > *", "cat a &> b"));
    assert!(matches("cat ... >> *", "cat a >> b"));
    assert!(!matches("cat ... >> *", "cat a > b"));
    assert!(!matches("cat ... > *", "cat a < b"));
  }

  #[test]
  fn a_redirect_is_found_in_any_position() {
    assert!(matches("cat ... > /tmp/**", "cat > /tmp/x a b"));
    assert!(matches("cat ... > /tmp/**", "cat a > /tmp/x b"));
    assert!(matches("cat ... > /tmp/**", "cat a 2> err > /tmp/x"));
  }

  #[test]
  fn a_missing_redirect_fails() {
    assert!(!matches("cat ... > *", "cat a b"));
    assert!(!matches("cat ... > /tmp/**", "cat a > b"));
  }

  #[test]
  fn a_redirect_target_uses_the_word_rules() {
    assert!(matches("cat ... > *", "cat a > \"$out\""));
    assert!(!matches("cat ... > /tmp/**", "cat a > \"$out\""));
    assert!(matches("cat ... > out", "cat a > 'out'"));
  }

  #[test]
  fn a_wordless_command_matches_a_rest_pattern() {
    assert!(matches("... > /tmp/**", "> /tmp/out"));
    assert!(!matches("* > /tmp/**", "> /tmp/out"));

    // `( a; b ) > out` segments to a, b, and a wordless entry last.
    let segments = segment::segment("( a; b ) > /tmp/out").unwrap();
    let group = segments.commands.last().unwrap();
    assert!(group.words.is_empty());
    assert!(pattern("... > /tmp/**").matches(group));
  }

  #[test]
  fn a_pattern_with_only_redirects_needs_no_words() {
    assert!(matches("> /tmp/**", "> /tmp/out"));
    assert!(!matches("> /tmp/**", "echo hi > /tmp/out"));
  }
}
