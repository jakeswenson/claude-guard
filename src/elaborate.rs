//! The command elaborator: one simple command, classified.
//!
//! Given a [`SimpleCommand`] and what is declared about its program, an
//! [`Elaborated`] says which words are the name, options with their
//! values, and arguments, which argument names a subcommand, and where an
//! inner command or script sits. Design: D22 to D25 in
//! `docs/design/rule-language/01-decisions.md`.
//!
//! Elaboration is a partition of the words. Every input word appears
//! exactly once in [`Elaborated::parts`], in order, and [`Elaborated::
//! flatten`] gives the input back. An option group keeps the word as
//! written and derives its flags and value from it, so `-fxd` stays
//! `-fxd` with flags `f`, `x`, `d`. The inner command is a view over the
//! same words, never a second copy.
//!
//! Getopt rules apply to a declared command:
//!
//! - `--` ends options; everything after is an argument.
//! - `--name=value` is one group with an attached value. `--name` with a
//!   declared value takes the next word.
//! - `-fxd` splits into flags when every letter is a declared short flag.
//!   A letter that takes a value swallows the rest of the word, or the
//!   next word when nothing is left.
//! - A word with an undeclared flag stays whole: no derived flags, no
//!   value. Wrong data degrades to loose, never to swallowing an argument.
//! - A dynamic word is an argument. The shell has not said what it is.
//!
//! An undeclared command elaborates as its name and arguments, so the
//! matcher sees one shape whether or not a declaration existed.

// Wired into the syntax and the matcher in claude-guard-1ma.2 and .3.
#![allow(dead_code)]

use std::collections::BTreeMap;

use crate::segment::{Redirect, SimpleCommand, Word};

/// What is known about one program, or one subcommand of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Declaration {
  pub options: Vec<OptionSpec>,
  /// Keyed by name; an alias maps to the same declaration.
  pub subcommands: BTreeMap<String, Declaration>,
  pub inner: Option<InnerSpec>,
}

/// One option: a short form, a long form, or both, and what it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionSpec {
  pub short: Option<char>,
  pub long: Option<String>,
  pub arity: Arity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
  /// A bare flag.
  None,
  /// One value, attached or the next word.
  One,
  /// A value only when attached (`--color=always`), never the next word.
  Optional,
}

/// Where a declared command carries another command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InnerSpec {
  /// The arguments from `from` on are a command: sudo, env, nice, nohup,
  /// xargs at 0; timeout at 1, after the duration.
  Command { from: usize },
  /// The arguments from `from` on are a script, joined with spaces. For
  /// ssh `from` is 1, after the host. `when_flag` gates it: bash's script
  /// is argument 0 only under `-c`. Flags are named without dashes.
  Script {
    from: usize,
    when_flag: Option<String>,
  },
  /// The value of option `flag` is a script: `nu -c "..."`,
  /// `python3 -c "..."`. Named without dashes.
  ScriptOption { flag: String },
}

/// Every declaration the guard knows, by program name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Declarations {
  pub by_name: BTreeMap<String, Declaration>,
}

/// A command, classified. `parts` is the partition; everything else is
/// derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elaborated {
  pub parts: Vec<Part>,
  /// The subcommand path the arguments named, such as `["stash", "pop"]`
  /// for `git stash pop`. Empty when the declaration has none.
  pub subcommand: Vec<String>,
  /// The inner command or script, a view over the arguments.
  pub inner: Option<Inner>,
  pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
  /// The program name. Absent for a wordless command such as `> out`.
  Name(Word),
  Option(OptionGroup),
  /// An argument, including a subcommand name.
  Arg(Word),
}

/// One option word with what was derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionGroup {
  /// The word as written: `-fxd`, `--git-dir=x`, `-C`.
  pub text: Word,
  /// The flags the word names, without dashes: `["f", "x", "d"]`,
  /// `["git-dir"]`. Empty when the word was not understood.
  pub flags: Vec<String>,
  pub value: Option<Value>,
}

/// An option's value, and whether it lived inside the option word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
  pub text: Word,
  /// `-Cfoo` and `--git-dir=foo` are attached; `-C foo` is not, and the
  /// value is its own input word.
  pub attached: bool,
}

/// What an inner declaration found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inner {
  /// The arguments elaborated as a command of their own.
  Command(Box<Elaborated>),
  /// The arguments joined as a script for the segmenter to run.
  Script(String),
}

impl Declarations {
  pub fn new() -> Declarations {
    Declarations::default()
  }

  pub fn declare(
    &mut self,
    name: &str,
    declaration: Declaration,
  ) {
    self.by_name.insert(name.to_string(), declaration);
  }

  /// Classify `command`. Never fails: an unknown program elaborates as
  /// its name and arguments.
  pub fn elaborate(
    &self,
    command: &SimpleCommand,
  ) -> Elaborated {
    let (name, rest) = match command.words.split_first() {
      Some((name, rest)) => (Some(name.clone()), rest),
      None => (None, &command.words[..]),
    };
    let declaration = name.as_ref().and_then(|word| match word {
      Word::Literal(text) => self.by_name.get(program_name(text)),
      Word::Dynamic(_) => None,
    });

    let mut elaborated = Elaborated {
      parts: name.into_iter().map(Part::Name).collect(),
      subcommand: Vec::new(),
      inner: None,
      redirects: command.redirects.clone(),
    };
    match declaration {
      Some(declaration) => self.walk(declaration, rest, &mut elaborated),
      None => elaborated.parts.extend(rest.iter().cloned().map(Part::Arg)),
    }
    elaborated
  }

  /// The getopt walk over the words after the name, under `declaration`
  /// and then under whichever subcommand the arguments name.
  fn walk(
    &self,
    declaration: &Declaration,
    words: &[Word],
    out: &mut Elaborated,
  ) {
    let mut current = declaration;
    let mut options_open = true;
    let mut positional = 0usize;
    let mut i = 0;
    while i < words.len() {
      let word = &words[i];
      let text = match word {
        Word::Literal(text) if options_open => text,
        _ => {
          out.parts.push(Part::Arg(word.clone()));
          positional += 1;
          i += 1;
          continue;
        }
      };

      if text == "--" {
        out.parts.push(Part::Option(OptionGroup {
          text: word.clone(),
          flags: Vec::new(),
          value: None,
        }));
        options_open = false;
        i += 1;
        continue;
      }

      if let Some(group) = classify_option(current, word, words.get(i + 1)) {
        let took_next = matches!(&group.value, Some(v) if !v.attached);
        out.parts.push(Part::Option(group));
        i += if took_next { 2 } else { 1 };
        continue;
      }

      // An argument. The first one may name a subcommand, which changes
      // the declaration for what follows.
      if positional == 0
        && out.subcommand.len() < 64
        && let Some(sub) = current.subcommands.get(text)
      {
        out.subcommand.push(text.clone());
        current = sub;
        out.parts.push(Part::Arg(word.clone()));
        i += 1;
        continue;
      }
      out.parts.push(Part::Arg(word.clone()));
      positional += 1;
      i += 1;
      // A command that carries another one stops taking options at its
      // first positional: `sudo -u root git -C .` gives `-C` to git.
      if current.inner.is_some() {
        options_open = false;
      }
    }

    out.inner = current
      .inner
      .as_ref()
      .and_then(|spec| self.inner(spec, out));
  }

  /// The inner view, per the spec, over the arguments after the
  /// subcommand path.
  fn inner(
    &self,
    spec: &InnerSpec,
    out: &Elaborated,
  ) -> Option<Inner> {
    let args: Vec<&Word> = out.args().skip(out.subcommand.len()).collect();
    match spec {
      InnerSpec::Command { from } => {
        let words: Vec<Word> = args.into_iter().skip(*from).cloned().collect();
        if words.is_empty() {
          return None;
        }
        let command = SimpleCommand {
          words,
          redirects: Vec::new(),
        };
        Some(Inner::Command(Box::new(self.elaborate(&command))))
      }
      InnerSpec::ScriptOption { flag } => out
        .options()
        .find(|group| group.flags.iter().any(|f| f == flag))
        .and_then(|group| group.value.as_ref())
        .map(|value| match &value.text {
          Word::Literal(text) | Word::Dynamic(text) => Inner::Script(text.clone()),
        }),
      InnerSpec::Script { from, when_flag } => {
        if let Some(flag) = when_flag
          && !out.has_flag(flag)
        {
          return None;
        }
        let script: Vec<&Word> = args.into_iter().skip(*from).collect();
        match script.as_slice() {
          [] => None,
          // One word is the script itself, as in `bash -c 'git stash'`.
          [Word::Literal(text) | Word::Dynamic(text)] => Some(Inner::Script(text.clone())),
          many => Some(Inner::Script(join_words(many))),
        }
      }
    }
  }
}

/// `git` from `/usr/bin/git`.
fn program_name(text: &str) -> &str {
  text.rsplit('/').next().unwrap_or(text)
}

/// Read `word` as an option under `declaration`, taking `next` as its
/// value when the declaration says so. `None` when the word is not an
/// option at all. A word that starts with a dash but names an undeclared
/// flag is an option group with nothing derived.
fn classify_option(
  declaration: &Declaration,
  word: &Word,
  next: Option<&Word>,
) -> Option<OptionGroup> {
  let Word::Literal(text) = word else {
    return None;
  };
  if text == "-" || !text.starts_with('-') {
    return None;
  }
  let whole = || OptionGroup {
    text: word.clone(),
    flags: Vec::new(),
    value: None,
  };

  if let Some(long) = text.strip_prefix("--") {
    let (name, attached) = match long.split_once('=') {
      Some((name, value)) => (name, Some(value)),
      None => (long, None),
    };
    let Some(spec) = declaration
      .options
      .iter()
      .find(|o| o.long.as_deref() == Some(name))
    else {
      return Some(whole());
    };
    let value = match (spec.arity, attached) {
      (_, Some(value)) => Some(Value {
        text: Word::Literal(value.to_string()),
        attached: true,
      }),
      (Arity::One, None) => next.map(|n| Value {
        text: n.clone(),
        attached: false,
      }),
      (Arity::None | Arity::Optional, None) => None,
    };
    return Some(OptionGroup {
      text: word.clone(),
      flags: vec![name.to_string()],
      value,
    });
  }

  // A cluster of short flags. Every letter must be declared, and a
  // letter that takes a value ends the cluster.
  let mut flags = Vec::new();
  let letters: Vec<char> = text[1..].chars().collect();
  for (n, letter) in letters.iter().enumerate() {
    let Some(spec) = declaration
      .options
      .iter()
      .find(|o| o.short == Some(*letter))
    else {
      return Some(whole());
    };
    flags.push(letter.to_string());
    match spec.arity {
      Arity::None => {}
      Arity::One | Arity::Optional => {
        let rest: String = letters[n + 1..].iter().collect();
        let value = if !rest.is_empty() {
          Some(Value {
            text: Word::Literal(rest),
            attached: true,
          })
        } else if spec.arity == Arity::One {
          next.map(|n| Value {
            text: n.clone(),
            attached: false,
          })
        } else {
          None
        };
        return Some(OptionGroup {
          text: word.clone(),
          flags,
          value,
        });
      }
    }
  }
  Some(OptionGroup {
    text: word.clone(),
    flags,
    value: None,
  })
}

/// Words back to one shell line. A literal word that needs quoting gets
/// single quotes; a dynamic word is left as the shell saw it.
fn join_words(words: &[&Word]) -> String {
  words
    .iter()
    .map(|word| match word {
      Word::Literal(text) if needs_quotes(text) => {
        format!("'{}'", text.replace('\'', "'\\''"))
      }
      Word::Literal(text) | Word::Dynamic(text) => text.clone(),
    })
    .collect::<Vec<_>>()
    .join(" ")
}

fn needs_quotes(text: &str) -> bool {
  text.is_empty()
    || text.chars().any(|c| {
      c.is_whitespace()
        || matches!(
          c,
          '\''
            | '"'
            | '$'
            | '`'
            | '\\'
            | '|'
            | '&'
            | ';'
            | '<'
            | '>'
            | '('
            | ')'
            | '*'
            | '?'
            | '['
            | ']'
            | '#'
            | '~'
            | '!'
            | '{'
            | '}'
        )
    })
}

impl Elaborated {
  /// The input words, in order. The partition property: this equals
  /// the words elaboration was given.
  pub fn flatten(&self) -> Vec<Word> {
    let mut words = Vec::new();
    for part in &self.parts {
      match part {
        Part::Name(word) | Part::Arg(word) => words.push(word.clone()),
        Part::Option(group) => {
          words.push(group.text.clone());
          if let Some(value) = &group.value
            && !value.attached
          {
            words.push(value.text.clone());
          }
        }
      }
    }
    words
  }

  pub fn name(&self) -> Option<&Word> {
    self.parts.iter().find_map(|part| match part {
      Part::Name(word) => Some(word),
      _ => None,
    })
  }

  pub fn options(&self) -> impl Iterator<Item = &OptionGroup> {
    self.parts.iter().filter_map(|part| match part {
      Part::Option(group) => Some(group),
      _ => None,
    })
  }

  /// Arguments in order, subcommand names included.
  pub fn args(&self) -> impl Iterator<Item = &Word> {
    self.parts.iter().filter_map(|part| match part {
      Part::Arg(word) => Some(word),
      _ => None,
    })
  }

  /// Whether any option group derived `flag`, given without dashes.
  pub fn has_flag(
    &self,
    flag: &str,
  ) -> bool {
    self
      .options()
      .any(|group| group.flags.iter().any(|f| f == flag))
  }
}

#[cfg(test)]
pub mod testing {
  use super::*;

  fn opt(
    short: Option<char>,
    long: Option<&str>,
    arity: Arity,
  ) -> OptionSpec {
    OptionSpec {
      short,
      long: long.map(str::to_string),
      arity,
    }
  }

  /// git with `-C`, `--git-dir`, `--no-pager`/`-P`, and `clean`, `stash`,
  /// `commit` subcommands; enough to exercise every rule.
  pub fn git() -> Declaration {
    let mut subcommands = BTreeMap::new();
    subcommands.insert(
      "clean".into(),
      Declaration {
        options: vec![
          opt(Some('f'), Some("force"), Arity::None),
          opt(Some('x'), None, Arity::None),
          opt(Some('d'), None, Arity::None),
          opt(Some('e'), Some("exclude"), Arity::One),
        ],
        ..Declaration::default()
      },
    );
    subcommands.insert(
      "stash".into(),
      Declaration {
        options: vec![opt(Some('q'), Some("quiet"), Arity::None)],
        ..Declaration::default()
      },
    );
    subcommands.insert(
      "commit".into(),
      Declaration {
        options: vec![
          opt(Some('m'), Some("message"), Arity::One),
          opt(Some('a'), Some("all"), Arity::None),
        ],
        ..Declaration::default()
      },
    );
    Declaration {
      options: vec![
        opt(Some('C'), None, Arity::One),
        opt(None, Some("git-dir"), Arity::One),
        opt(Some('P'), Some("no-pager"), Arity::None),
        opt(None, Some("color"), Arity::Optional),
      ],
      subcommands,
      inner: None,
    }
  }

  pub fn sudo() -> Declaration {
    Declaration {
      options: vec![
        opt(Some('u'), Some("user"), Arity::One),
        opt(Some('n'), Some("non-interactive"), Arity::None),
      ],
      subcommands: BTreeMap::new(),
      inner: Some(InnerSpec::Command { from: 0 }),
    }
  }

  pub fn timeout() -> Declaration {
    Declaration {
      options: vec![opt(Some('k'), Some("kill-after"), Arity::One)],
      subcommands: BTreeMap::new(),
      inner: Some(InnerSpec::Command { from: 1 }),
    }
  }

  pub fn nu() -> Declaration {
    Declaration {
      options: vec![
        opt(Some('c'), Some("commands"), Arity::One),
        opt(Some('n'), Some("no-config-file"), Arity::None),
      ],
      subcommands: BTreeMap::new(),
      inner: Some(InnerSpec::ScriptOption { flag: "c".into() }),
    }
  }

  pub fn ssh() -> Declaration {
    Declaration {
      options: vec![
        opt(Some('o'), None, Arity::One),
        opt(Some('p'), None, Arity::One),
        opt(Some('v'), None, Arity::None),
        opt(Some('t'), None, Arity::None),
      ],
      subcommands: BTreeMap::new(),
      inner: Some(InnerSpec::Script {
        from: 1,
        when_flag: None,
      }),
    }
  }

  pub fn bash() -> Declaration {
    Declaration {
      options: vec![
        opt(Some('c'), None, Arity::None),
        opt(Some('e'), None, Arity::None),
        opt(Some('x'), None, Arity::None),
      ],
      subcommands: BTreeMap::new(),
      inner: Some(InnerSpec::Script {
        from: 0,
        when_flag: Some("c".into()),
      }),
    }
  }

  /// git, sudo, timeout, ssh, bash, nu.
  pub fn declarations() -> Declarations {
    let mut d = Declarations::new();
    d.declare("git", git());
    d.declare("sudo", sudo());
    d.declare("timeout", timeout());
    d.declare("ssh", ssh());
    d.declare("bash", bash());
    d.declare("nu", nu());
    d
  }
}

#[cfg(test)]
mod tests {
  use super::testing::declarations;
  use super::*;
  use crate::segment;

  fn lit(s: &str) -> Word {
    Word::Literal(s.into())
  }

  fn command(input: &str) -> SimpleCommand {
    let mut segments = segment::segment(input).unwrap();
    assert_eq!(segments.commands.len(), 1, "{input:?} is not one command");
    segments.commands.remove(0)
  }

  fn elaborate(input: &str) -> Elaborated {
    let command = command(input);
    let out = declarations().elaborate(&command);
    assert_eq!(
      out.flatten(),
      command.words,
      "partition broken for {input:?}"
    );
    out
  }

  /// `(text, flags, value)` per option, for terse assertions.
  type Opt = (String, Vec<String>, Option<(String, bool)>);

  fn options(e: &Elaborated) -> Vec<Opt> {
    e.options()
      .map(|g| {
        let text = match &g.text {
          Word::Literal(t) | Word::Dynamic(t) => t.clone(),
        };
        let value = g.value.as_ref().map(|v| {
          let t = match &v.text {
            Word::Literal(t) | Word::Dynamic(t) => t.clone(),
          };
          (t, v.attached)
        });
        (text, g.flags.clone(), value)
      })
      .collect()
  }

  fn args(e: &Elaborated) -> Vec<String> {
    e.args()
      .map(|w| match w {
        Word::Literal(t) | Word::Dynamic(t) => t.clone(),
      })
      .collect()
  }

  fn s(x: &str) -> String {
    x.into()
  }

  // --- the partition ---

  #[test]
  fn an_undeclared_command_is_name_and_args() {
    let e = elaborate("rg -n foo src");
    assert_eq!(e.name(), Some(&lit("rg")));
    assert!(options(&e).is_empty());
    assert_eq!(args(&e), ["-n", "foo", "src"]);
    assert_eq!(e.subcommand, Vec::<String>::new());
    assert_eq!(e.inner, None);
  }

  #[test]
  fn a_wordless_command_has_no_name() {
    let e = elaborate("> out");
    assert_eq!(e.name(), None);
    assert!(e.parts.is_empty());
    assert_eq!(e.redirects.len(), 1);
  }

  #[test]
  fn a_program_path_finds_its_declaration() {
    let e = elaborate("/usr/bin/git -C . stash");
    assert_eq!(
      options(&e),
      [(s("-C"), vec![s("C")], Some((s("."), false)))]
    );
  }

  // --- options with values ---

  #[test]
  fn a_short_flag_takes_the_next_word_or_the_rest_of_its_own() {
    let e = elaborate("git -C . stash");
    assert_eq!(
      options(&e),
      [(s("-C"), vec![s("C")], Some((s("."), false)))]
    );
    assert_eq!(args(&e), ["stash"]);
    assert_eq!(e.subcommand, ["stash"]);

    let e = elaborate("git -C. stash");
    assert_eq!(
      options(&e),
      [(s("-C."), vec![s("C")], Some((s("."), true)))]
    );
    assert_eq!(args(&e), ["stash"]);
  }

  #[test]
  fn a_long_flag_takes_an_attached_or_next_value() {
    let e = elaborate("git --git-dir=.git status");
    assert_eq!(
      options(&e),
      [(
        s("--git-dir=.git"),
        vec![s("git-dir")],
        Some((s(".git"), true))
      )]
    );
    let e = elaborate("git --git-dir .git status");
    assert_eq!(
      options(&e),
      [(s("--git-dir"), vec![s("git-dir")], Some((s(".git"), false)))]
    );
    assert_eq!(args(&e), ["status"]);
  }

  #[test]
  fn an_optional_value_is_taken_only_when_attached() {
    let e = elaborate("git --color=always log");
    assert_eq!(
      options(&e),
      [(
        s("--color=always"),
        vec![s("color")],
        Some((s("always"), true))
      )]
    );
    let e = elaborate("git --color log");
    assert_eq!(options(&e), [(s("--color"), vec![s("color")], None)]);
    assert_eq!(args(&e), ["log"]);
  }

  #[test]
  fn a_value_flag_at_the_end_has_no_value() {
    let e = elaborate("git -C");
    assert_eq!(options(&e), [(s("-C"), vec![s("C")], None)]);
  }

  // --- clustering ---

  #[test]
  fn a_cluster_splits_into_declared_flags() {
    let e = elaborate("git clean -fxd");
    assert_eq!(e.subcommand, ["clean"]);
    assert_eq!(
      options(&e),
      [(s("-fxd"), vec![s("f"), s("x"), s("d")], None)]
    );
    assert_eq!(e.flatten(), vec![lit("git"), lit("clean"), lit("-fxd")]);
  }

  #[test]
  fn a_value_letter_ends_the_cluster_and_takes_what_follows() {
    let e = elaborate("git commit -am wip");
    assert_eq!(
      options(&e),
      [(s("-am"), vec![s("a"), s("m")], Some((s("wip"), false)))]
    );
    assert_eq!(args(&e), ["commit"]);
    let e = elaborate("git commit -amwip");
    assert_eq!(
      options(&e),
      [(s("-amwip"), vec![s("a"), s("m")], Some((s("wip"), true)))]
    );
  }

  #[test]
  fn an_undeclared_letter_keeps_the_word_whole_and_takes_nothing() {
    let e = elaborate("git -zq . stash");
    assert_eq!(options(&e), [(s("-zq"), vec![], None)]);
    assert_eq!(args(&e), [".", "stash"]);
    let e = elaborate("git --no-such x");
    assert_eq!(options(&e), [(s("--no-such"), vec![], None)]);
    assert_eq!(args(&e), ["x"]);
  }

  #[test]
  fn a_lone_dash_is_an_argument() {
    let e = elaborate("git commit -m -");
    assert_eq!(
      options(&e),
      [(s("-m"), vec![s("m")], Some((s("-"), false)))]
    );
    let e = elaborate("git -");
    assert_eq!(args(&e), ["-"]);
  }

  // --- subcommands ---

  #[test]
  fn the_subcommand_switches_the_declaration() {
    let e = elaborate("git -P stash -q");
    assert_eq!(
      options(&e),
      [(s("-P"), vec![s("P")], None), (s("-q"), vec![s("q")], None)]
    );
    assert_eq!(e.subcommand, ["stash"]);
    // -q is stash's; before the subcommand it is unknown.
    let e = elaborate("git -q stash");
    assert_eq!(options(&e), [(s("-q"), vec![], None)]);
  }

  #[test]
  fn only_the_first_argument_can_name_a_subcommand() {
    let e = elaborate("git log stash");
    assert_eq!(e.subcommand, Vec::<String>::new());
    assert_eq!(args(&e), ["log", "stash"]);
  }

  // --- double dash ---

  #[test]
  fn double_dash_ends_options() {
    let e = elaborate("git commit -- -m");
    assert_eq!(options(&e), [(s("--"), vec![], None)]);
    assert_eq!(args(&e), ["commit", "-m"]);
  }

  // --- dynamic words ---

  #[test]
  fn a_dynamic_word_is_an_argument_even_with_a_dash() {
    // The segmenter keeps a dynamic word's raw text, quotes included.
    let e = elaborate("git \"-$flag\" stash");
    assert_eq!(options(&e), []);
    assert_eq!(args(&e), ["\"-$flag\"", "stash"]);
    assert_eq!(e.subcommand, Vec::<String>::new());
  }

  #[test]
  fn a_dynamic_value_is_still_the_value() {
    let e = elaborate("git -C $dir stash");
    assert_eq!(
      options(&e),
      [(s("-C"), vec![s("C")], Some((s("$dir"), false)))]
    );
    assert_eq!(e.subcommand, ["stash"]);
  }

  // --- inner ---

  #[test]
  fn a_wrapper_elaborates_its_command() {
    let e = elaborate("sudo -u root git -C . stash");
    assert_eq!(
      options(&e),
      [(s("-u"), vec![s("u")], Some((s("root"), false)))]
    );
    let Some(Inner::Command(inner)) = &e.inner else {
      panic!("{:?}", e.inner);
    };
    assert_eq!(inner.name(), Some(&lit("git")));
    assert_eq!(inner.subcommand, ["stash"]);
    assert_eq!(
      options(inner),
      [(s("-C"), vec![s("C")], Some((s("."), false)))]
    );
    assert_eq!(elaborate("sudo -n").inner, None);
  }

  #[test]
  fn a_wrapper_can_skip_leading_positionals() {
    let e = elaborate("timeout -k 5 30s git stash");
    let Some(Inner::Command(inner)) = &e.inner else {
      panic!("{:?}", e.inner);
    };
    assert_eq!(inner.name(), Some(&lit("git")));
    assert_eq!(inner.subcommand, ["stash"]);
    assert_eq!(elaborate("timeout 30s").inner, None);
  }

  #[test]
  fn a_script_can_be_an_option_value() {
    let e = elaborate("nu -n -c 'ls | length'");
    assert_eq!(e.inner, Some(Inner::Script("ls | length".into())));
    assert_eq!(elaborate("nu script.nu").inner, None);
    assert_eq!(elaborate("nu -c").inner, None);
  }

  #[test]
  fn ssh_carries_a_script_after_the_host() {
    let e = elaborate("ssh -o ConnectTimeout=10 nas 'cd /a && rm -rf x'");
    assert_eq!(e.inner, Some(Inner::Script("cd /a && rm -rf x".into())));
    let e = elaborate("ssh nas ls -la /var");
    assert_eq!(e.inner, Some(Inner::Script("ls -la /var".into())));
    assert_eq!(elaborate("ssh nas").inner, None);
  }

  #[test]
  fn a_script_rejoins_words_that_need_quotes() {
    // A literal with spaces is re-quoted; a dynamic word keeps the raw
    // text the shell saw, quotes and all.
    let e = elaborate("ssh nas echo 'a b' \"$HOME\"");
    assert_eq!(e.inner, Some(Inner::Script("echo 'a b' \"$HOME\"".into())));
  }

  #[test]
  fn bash_carries_a_script_only_under_dash_c() {
    let e = elaborate("bash -c 'git stash'");
    assert_eq!(e.inner, Some(Inner::Script("git stash".into())));
    let e = elaborate("bash -ec 'git stash'");
    assert_eq!(e.inner, Some(Inner::Script("git stash".into())));
    assert_eq!(elaborate("bash script.sh").inner, None);
  }

  // --- the corpus ---

  #[test]
  fn every_logged_command_round_trips() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/commands.jsonl");
    let text = std::fs::read_to_string(fixture).unwrap();
    let declarations = declarations();
    let mut checked = 0;
    for line in text.lines() {
      let source: String = serde_json::from_str(line).unwrap();
      let Ok(segments) = segment::segment(&source) else {
        continue;
      };
      for command in &segments.commands {
        let out = declarations.elaborate(command);
        assert_eq!(
          out.flatten(),
          command.words,
          "partition broken for {source:?}"
        );
        checked += 1;
      }
    }
    assert!(checked > 100, "only {checked} commands checked");
  }
}
