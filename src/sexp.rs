//! The surface syntax of rule files: s-expressions with a bracket form.
//!
//! Five node kinds and nothing else:
//!
//! | Text            | Node                                        |
//! |-----------------|---------------------------------------------|
//! | `( ... )`       | list                                        |
//! | `[ ... ]`       | pattern, a list the rule layer reads as words |
//! | `"..."`         | string, with `\"`, `\\`, `\n`, `\t` escapes  |
//! | `:name`         | keyword                                     |
//! | anything else   | symbol                                      |
//!
//! `;` starts a comment that runs to the end of the line. A symbol is any
//! run of characters up to whitespace or one of `()[]";`, so `*`, `-...`,
//! `>>`, `/tmp/**`, `?file`, and `ancestor-has?` are all symbols. There
//! are no numbers: `5m` is a symbol the rule layer interprets.
//!
//! Every node carries the line and column where it starts, one-based, so
//! the rule layer can report a type error at its source. The printer
//! writes a node back so that reading the output gives the same tree,
//! which is what `rules --export` relies on.

use std::fmt;

/// Where a node starts in its source text. One-based, columns in chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
  pub line: u32,
  pub col: u32,
}

impl fmt::Display for Span {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    write!(f, "{}:{}", self.line, self.col)
  }
}

/// One node of the tree, with its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
  pub span: Span,
  pub kind: Kind,
}

/// What a node is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
  /// `( ... )`
  List(Vec<Node>),
  /// `[ ... ]`
  Pattern(Vec<Node>),
  /// `"..."`, unescaped.
  Str(String),
  /// `:name`, stored without the colon.
  Keyword(String),
  /// Anything else.
  Symbol(String),
}

/// The source text is not well formed. The span is where the problem is,
/// not where the enclosing form started, except for an unterminated form,
/// where the start is the only position there is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError {
  pub span: Span,
  pub message: String,
}

impl fmt::Display for ReadError {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    write!(f, "{}: {}", self.span, self.message)
  }
}

impl std::error::Error for ReadError {}

/// Read every top-level form in `source`.
pub fn read_all(source: &str) -> Result<Vec<Node>, ReadError> {
  let mut reader = Reader::new(source);
  let mut forms = Vec::new();
  while let Some(node) = reader.next_node(None)? {
    forms.push(node);
  }
  Ok(forms)
}

/// Read exactly one form. Anything after it is an error.
#[cfg(test)]
pub fn read_one(source: &str) -> Result<Node, ReadError> {
  let mut reader = Reader::new(source);
  let Some(node) = reader.next_node(None)? else {
    return Err(ReadError {
      span: reader.span(),
      message: "expected a form, found nothing".into(),
    });
  };
  if let Some(extra) = reader.next_node(None)? {
    return Err(ReadError {
      span: extra.span,
      message: "expected one form, found a second".into(),
    });
  }
  Ok(node)
}

struct Reader<'a> {
  chars: std::iter::Peekable<std::str::Chars<'a>>,
  line: u32,
  col: u32,
}

/// Which bracket a list is waiting to close.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
  Paren,
  Bracket,
}

impl Close {
  fn char(self) -> char {
    match self {
      Close::Paren => ')',
      Close::Bracket => ']',
    }
  }
}

fn is_delimiter(c: char) -> bool {
  c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '"' | ';')
}

impl<'a> Reader<'a> {
  fn new(source: &'a str) -> Self {
    Reader {
      chars: source.chars().peekable(),
      line: 1,
      col: 1,
    }
  }

  fn span(&self) -> Span {
    Span {
      line: self.line,
      col: self.col,
    }
  }

  fn peek(&mut self) -> Option<char> {
    self.chars.peek().copied()
  }

  fn bump(&mut self) -> Option<char> {
    let c = self.chars.next()?;
    if c == '\n' {
      self.line += 1;
      self.col = 1;
    } else {
      self.col += 1;
    }
    Some(c)
  }

  fn skip_blank(&mut self) {
    while let Some(c) = self.peek() {
      if c == ';' {
        while self.peek().is_some_and(|c| c != '\n') {
          self.bump();
        }
      } else if c.is_whitespace() {
        self.bump();
      } else {
        break;
      }
    }
  }

  /// The next node, or `None` at the end of input or at the closer the
  /// caller is waiting for. A closer nobody is waiting for is an error.
  fn next_node(
    &mut self,
    waiting_for: Option<Close>,
  ) -> Result<Option<Node>, ReadError> {
    self.skip_blank();
    let span = self.span();
    let Some(c) = self.peek() else {
      return Ok(None);
    };
    let kind = match c {
      ')' | ']' => {
        let closer = if c == ')' {
          Close::Paren
        } else {
          Close::Bracket
        };
        if waiting_for == Some(closer) {
          return Ok(None);
        }
        return Err(ReadError {
          span,
          message: match waiting_for {
            Some(open) => format!("expected `{}`, found `{c}`", open.char()),
            None => format!("unexpected `{c}`"),
          },
        });
      }
      '(' => {
        self.bump();
        Kind::List(self.items(span, Close::Paren)?)
      }
      '[' => {
        self.bump();
        Kind::Pattern(self.items(span, Close::Bracket)?)
      }
      '"' => {
        self.bump();
        Kind::Str(self.string(span)?)
      }
      _ => self.atom(span)?,
    };
    Ok(Some(Node { span, kind }))
  }

  /// Items of a list whose opener is already consumed.
  fn items(
    &mut self,
    open: Span,
    close: Close,
  ) -> Result<Vec<Node>, ReadError> {
    let mut items = Vec::new();
    loop {
      match self.next_node(Some(close))? {
        Some(node) => items.push(node),
        None => {
          if self.bump().is_none() {
            return Err(ReadError {
              span: open,
              message: format!("unterminated `{}`", opener(close)),
            });
          }
          return Ok(items);
        }
      }
    }
  }

  /// Body of a string whose opening quote is already consumed.
  fn string(
    &mut self,
    open: Span,
  ) -> Result<String, ReadError> {
    let mut text = String::new();
    loop {
      let escape_at = self.span();
      match self.bump() {
        None => {
          return Err(ReadError {
            span: open,
            message: "unterminated string".into(),
          });
        }
        Some('"') => return Ok(text),
        Some('\\') => match self.bump() {
          Some('"') => text.push('"'),
          Some('\\') => text.push('\\'),
          Some('n') => text.push('\n'),
          Some('t') => text.push('\t'),
          Some(other) => {
            return Err(ReadError {
              span: escape_at,
              message: format!("unknown escape `\\{other}`"),
            });
          }
          None => {
            return Err(ReadError {
              span: open,
              message: "unterminated string".into(),
            });
          }
        },
        Some(c) => text.push(c),
      }
    }
  }

  fn atom(
    &mut self,
    span: Span,
  ) -> Result<Kind, ReadError> {
    let mut text = String::new();
    while let Some(c) = self.peek() {
      if is_delimiter(c) {
        break;
      }
      text.push(c);
      self.bump();
    }
    Ok(match text.strip_prefix(':') {
      Some("") => {
        return Err(ReadError {
          span,
          message: "keyword has no name".into(),
        });
      }
      Some(name) => Kind::Keyword(name.to_string()),
      None => Kind::Symbol(text),
    })
  }
}

fn opener(close: Close) -> char {
  match close {
    Close::Paren => '(',
    Close::Bracket => '[',
  }
}

// --- printing ---

/// Width the pretty printer tries to stay within. The pretty printer has
/// no caller yet: `rules --export` prints the built-in file verbatim so
/// its comments survive. A formatter for user files will use it.
#[allow(dead_code)]
const WIDTH: usize = 80;

impl Node {
  /// The node on one line.
  pub fn flat(&self) -> String {
    let mut out = String::new();
    self.write_flat(&mut out);
    out
  }

  fn write_flat(
    &self,
    out: &mut String,
  ) {
    match &self.kind {
      Kind::List(items) => write_seq(out, '(', ')', items),
      Kind::Pattern(items) => write_seq(out, '[', ']', items),
      Kind::Str(text) => write_str(out, text),
      Kind::Keyword(name) => {
        out.push(':');
        out.push_str(name);
      }
      Kind::Symbol(text) => out.push_str(text),
    }
  }

  /// The node over as many lines as it needs. A list that fits in
  /// [`WIDTH`] prints flat. One that does not keeps its head on the first
  /// line and puts every other item on its own line, two columns in.
  /// Patterns always print flat: they are one shell line.
  #[allow(dead_code)]
  pub fn pretty(&self) -> String {
    let mut out = String::new();
    self.write_pretty(&mut out, 0);
    out
  }

  fn write_pretty(
    &self,
    out: &mut String,
    indent: usize,
  ) {
    let flat = self.flat();
    let Kind::List(items) = &self.kind else {
      out.push_str(&flat);
      return;
    };
    if indent + flat.chars().count() <= WIDTH || items.len() < 2 {
      out.push_str(&flat);
      return;
    }
    out.push('(');
    items[0].write_flat(out);
    for item in &items[1..] {
      out.push('\n');
      out.extend(std::iter::repeat_n(' ', indent + 2));
      item.write_pretty(out, indent + 2);
    }
    out.push(')');
  }
}

/// Print top-level forms, one per paragraph.
#[allow(dead_code)]
pub fn pretty_all(forms: &[Node]) -> String {
  let mut out = String::new();
  for (i, form) in forms.iter().enumerate() {
    if i > 0 {
      out.push_str("\n\n");
    }
    out.push_str(&form.pretty());
  }
  if !forms.is_empty() {
    out.push('\n');
  }
  out
}

fn write_seq(
  out: &mut String,
  open: char,
  close: char,
  items: &[Node],
) {
  out.push(open);
  for (i, item) in items.iter().enumerate() {
    if i > 0 {
      out.push(' ');
    }
    item.write_flat(out);
  }
  out.push(close);
}

fn write_str(
  out: &mut String,
  text: &str,
) {
  out.push('"');
  for c in text.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\t' => out.push_str("\\t"),
      c => out.push(c),
    }
  }
  out.push('"');
}

impl fmt::Display for Node {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    f.write_str(&self.flat())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn read(source: &str) -> Node {
    read_one(source).unwrap_or_else(|e| panic!("read {source:?}: {e}"))
  }

  fn read_error(source: &str) -> String {
    match read_one(source) {
      Ok(node) => panic!("read {source:?}: {node}"),
      Err(e) => e.to_string(),
    }
  }

  fn at(
    line: u32,
    col: u32,
  ) -> Span {
    Span { line, col }
  }

  /// The tree without positions, for structural comparison.
  fn shape(node: &Node) -> String {
    match &node.kind {
      Kind::List(items) => format!("L{:?}", items.iter().map(shape).collect::<Vec<_>>()),
      Kind::Pattern(items) => format!("P{:?}", items.iter().map(shape).collect::<Vec<_>>()),
      Kind::Str(s) => format!("S{s:?}"),
      Kind::Keyword(k) => format!("K{k}"),
      Kind::Symbol(s) => format!("Y{s}"),
    }
  }

  // --- reading atoms ---

  #[test]
  fn symbols_are_any_run_up_to_a_delimiter() {
    for text in [
      "git",
      "*",
      "...",
      "-*",
      "-...",
      ">>",
      "/tmp/**",
      "?file",
      "ancestor-has?",
      "5m",
      "2>&1",
    ] {
      assert_eq!(read(text).kind, Kind::Symbol(text.into()), "{text}");
    }
  }

  #[test]
  fn keywords_drop_the_colon() {
    assert_eq!(read(":reason").kind, Kind::Keyword("reason".into()));
    assert_eq!(read_error(":"), "1:1: keyword has no name");
  }

  #[test]
  fn strings_unescape() {
    assert_eq!(read(r#""plain""#).kind, Kind::Str("plain".into()));
    assert_eq!(
      read(r#""a \"b\" \\ c\nd\te""#).kind,
      Kind::Str("a \"b\" \\ c\nd\te".into())
    );
    assert_eq!(
      read("\"multi\nline\"").kind,
      Kind::Str("multi\nline".into())
    );
  }

  #[test]
  fn a_string_may_hold_delimiters() {
    assert_eq!(
      read(r#""(not a list) ; nor a comment""#).kind,
      Kind::Str("(not a list) ; nor a comment".into())
    );
  }

  #[test]
  fn bad_strings_report_where() {
    assert_eq!(read_error("\"open"), "1:1: unterminated string");
    assert_eq!(
      read_error("\"ends in backslash\\"),
      "1:1: unterminated string"
    );
    assert_eq!(read_error("\"bad \\q\""), "1:6: unknown escape `\\q`");
  }

  // --- reading lists and patterns ---

  #[test]
  fn lists_and_patterns_nest() {
    let node = read("(deny [git -... stash ...] :reason \"no\")");
    assert_eq!(
      shape(&node),
      r#"L["Ydeny", "P[\"Ygit\", \"Y-...\", \"Ystash\", \"Y...\"]", "Kreason", "S\"no\""]"#
    );
  }

  #[test]
  fn empty_forms_read() {
    assert_eq!(read("()").kind, Kind::List(vec![]));
    assert_eq!(read("[]").kind, Kind::Pattern(vec![]));
  }

  #[test]
  fn delimiters_need_no_whitespace() {
    assert_eq!(
      shape(&read("(a(b)[c]\"d\";e\n)")),
      r#"L["Ya", "L[\"Yb\"]", "P[\"Yc\"]", "S\"d\""]"#
    );
  }

  #[test]
  fn comments_run_to_end_of_line() {
    let forms = read_all("; leading\n(a ; trailing\n b) ; end").unwrap();
    assert_eq!(forms.len(), 1);
    assert_eq!(shape(&forms[0]), r#"L["Ya", "Yb"]"#);
  }

  #[test]
  fn read_all_returns_every_top_level_form() {
    let forms = read_all("(a)\n\n[b c]\nd").unwrap();
    assert_eq!(
      forms.iter().map(shape).collect::<Vec<_>>(),
      [r#"L["Ya"]"#, r#"P["Yb", "Yc"]"#, "Yd"]
    );
    assert!(read_all("").unwrap().is_empty());
    assert!(read_all("  ; only a comment").unwrap().is_empty());
  }

  #[test]
  fn read_one_wants_exactly_one_form() {
    assert_eq!(read_error(""), "1:1: expected a form, found nothing");
    assert_eq!(
      read_error("(a) (b)"),
      "1:5: expected one form, found a second"
    );
  }

  #[test]
  fn mismatched_and_stray_closers_report_where() {
    assert_eq!(read_error("(a]"), "1:3: expected `)`, found `]`");
    assert_eq!(read_error("[a)"), "1:3: expected `]`, found `)`");
    assert_eq!(read_error(")"), "1:1: unexpected `)`");
    assert_eq!(read_error("a]"), "1:2: unexpected `]`");
  }

  #[test]
  fn unterminated_forms_point_at_their_opener() {
    assert_eq!(read_error("(a (b)"), "1:1: unterminated `(`");
    assert_eq!(read_error("(a\n  [b"), "2:3: unterminated `[`");
  }

  // --- spans ---

  #[test]
  fn every_node_knows_where_it_starts() {
    let node = read("(rule x\n  [git stash]\n  :reason \"no\")");
    assert_eq!(node.span, at(1, 1));
    let Kind::List(items) = &node.kind else {
      panic!()
    };
    assert_eq!(items[0].span, at(1, 2));
    assert_eq!(items[1].span, at(1, 7));
    assert_eq!(items[2].span, at(2, 3));
    let Kind::Pattern(words) = &items[2].kind else {
      panic!()
    };
    assert_eq!(words[1].span, at(2, 8));
    assert_eq!(items[3].span, at(3, 3));
    assert_eq!(items[4].span, at(3, 11));
  }

  #[test]
  fn columns_count_chars_not_bytes() {
    let node = read("(é x)");
    let Kind::List(items) = &node.kind else {
      panic!()
    };
    assert_eq!(items[1].span, at(1, 4));
  }

  // --- printing ---

  #[test]
  fn flat_prints_one_line_with_escapes() {
    let node = read("(deny [git -... stash ...]\n  :reason \"say \\\"no\\\"\\n\")");
    assert_eq!(
      node.flat(),
      r#"(deny [git -... stash ...] :reason "say \"no\"\n")"#
    );
    assert_eq!(node.to_string(), node.flat());
  }

  #[test]
  fn printing_then_reading_gives_the_same_tree() {
    let source = "(rule git-in-jj :when (ancestor-has? \".jj\")\n  (deny [git -... stash ...] :reason \"a \\\"b\\\"\" :instead \"c\\nd\")\n  (warn [] :reason \"\"))";
    let node = read(source);
    for printed in [node.flat(), node.pretty()] {
      assert_eq!(shape(&read(&printed)), shape(&node), "{printed}");
    }
  }

  #[test]
  fn pretty_keeps_short_lists_flat() {
    assert_eq!(read("(a b (c d))").pretty(), "(a b (c d))");
  }

  #[test]
  fn pretty_breaks_a_long_list_after_its_head() {
    let node = read(
      "(deny [git -... stash ...] :reason \"jj has no dirty tree, so there is nothing to stash.\" :instead \"use `jj new` or `jj describe`.\")",
    );
    assert_eq!(
      node.pretty(),
      "(deny\n  [git -... stash ...]\n  :reason\n  \"jj has no dirty tree, so there is nothing to stash.\"\n  :instead\n  \"use `jj new` or `jj describe`.\")"
    );
  }

  #[test]
  fn pretty_indents_nested_breaks() {
    let node = read(
      "(rule hard-denies (deny [git -... stash ...] :reason \"jj has no dirty tree, so there is nothing to stash.\" :instead \"x\") (deny [sed ...] :reason \"no\"))",
    );
    assert_eq!(
      node.pretty(),
      "(rule\n  hard-denies\n  (deny\n    [git -... stash ...]\n    :reason\n    \"jj has no dirty tree, so there is nothing to stash.\"\n    :instead\n    \"x\")\n  (deny [sed ...] :reason \"no\"))"
    );
  }

  #[test]
  fn pretty_never_breaks_a_pattern() {
    let long = format!("[{}]", "word ".repeat(30).trim_end());
    assert_eq!(read(&long).pretty(), long);
  }

  #[test]
  fn pretty_all_separates_forms_by_a_blank_line() {
    let forms = read_all("(a)(b)").unwrap();
    assert_eq!(pretty_all(&forms), "(a)\n\n(b)\n");
    assert_eq!(pretty_all(&[]), "");
  }
}
