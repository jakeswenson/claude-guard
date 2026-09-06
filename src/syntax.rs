//! From the s-expression tree to the typed rule table.
//!
//! The grammar this module accepts:
//!
//! ```text
//! file    := rule*
//! rule    := (rule <name> [:when <cond>] row+)
//! row     := (deny|ask|warn <subject> [:when <cond>] :reason "..." [:instead "..."])
//! subject := [word*]                       ; a Bash command pattern
//!          | (write|edit|multi-edit|read <path>)   ; a file tool pattern
//! word    := <symbol> | "..." | > <word> | >> <word> | < <word>
//! path    := "..." | ?name
//! ```
//!
//! Pattern words map to the tokens in [`pattern`]: `*`, `...`, `-*`,
//! `-...`, `?name`, and literals. A string in a bracket is a literal that
//! may hold spaces. The `<path>/**` form is refused here: path constraints
//! belong in the condition, as `(under? ?p "/tmp")`.
//!
//! `:instead` is required on deny and ask, optional on warn. A condition
//! is checked by [`cond`] against the binders its pattern declares; a
//! rule's `:when` may use none.
//!
//! Every error names the node it is about. Top-level forms are checked
//! independently, so one load reports every rule that is wrong.

// Used by the loader, which lands in claude-guard-110.4.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use crate::cond::{self, Cond, Scope};
use crate::pattern::{Pattern, RedirectPattern, Token, Var};
use crate::rules::{Kind, RuleName};
use crate::segment::RedirectKind;
use crate::sexp::{Kind as Sx, Node, Span};

/// A whole rule file, in evaluation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
  pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
  pub span: Span,
  pub name: RuleName,
  pub when: Option<Cond>,
  pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
  pub span: Span,
  pub decision: Kind,
  pub subject: Subject,
  pub when: Option<Cond>,
  pub reason: String,
  pub instead: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
  Command(Pattern),
  Tool(ToolPattern),
}

/// A file tool call, matched by tool and path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPattern {
  pub tool: FileTool,
  pub path: PathArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTool {
  Write,
  Edit,
  MultiEdit,
  Read,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathArg {
  Literal(String),
  Var(Var),
}

impl Subject {
  /// The binders a condition on this subject may use.
  pub fn binders(&self) -> BTreeSet<Var> {
    match self {
      Subject::Command(pattern) => pattern.binders(),
      Subject::Tool(ToolPattern {
        path: PathArg::Var(var),
        ..
      }) => BTreeSet::from([var.clone()]),
      Subject::Tool(_) => BTreeSet::new(),
    }
  }
}

/// The tree does not describe a rule. The span names the node at fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
  pub span: Span,
  pub message: String,
}

impl fmt::Display for TypeError {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    write!(f, "{}: {}", self.span, self.message)
  }
}

impl std::error::Error for TypeError {}

fn err<T>(
  span: Span,
  message: impl Into<String>,
) -> Result<T, TypeError> {
  Err(TypeError {
    span,
    message: message.into(),
  })
}

/// Check every top-level form. All the errors, or the table.
pub fn parse(forms: &[Node]) -> Result<File, Vec<TypeError>> {
  let mut rules = Vec::new();
  let mut errors = Vec::new();
  for form in forms {
    match parse_rule(form) {
      Ok(rule) => rules.push(rule),
      Err(e) => errors.push(e),
    }
  }
  if errors.is_empty() {
    Ok(File { rules })
  } else {
    Err(errors)
  }
}

fn parse_rule(form: &Node) -> Result<Rule, TypeError> {
  let Sx::List(items) = &form.kind else {
    return err(form.span, "expected (rule ...)");
  };
  let Some((head, items)) = items.split_first() else {
    return err(form.span, "expected (rule ...)");
  };
  match &head.kind {
    Sx::Symbol(s) if s == "rule" => {}
    Sx::Symbol(other) => return err(head.span, format!("expected (rule ...), found `{other}`")),
    _ => return err(head.span, "expected (rule ...)"),
  }
  let Some((name_node, items)) = items.split_first() else {
    return err(form.span, "rule needs a name");
  };
  let Sx::Symbol(name) = &name_node.kind else {
    return err(name_node.span, "rule name must be a symbol");
  };

  let mut when = None;
  let mut rows = Vec::new();
  let mut i = 0;
  while i < items.len() {
    let item = &items[i];
    match &item.kind {
      Sx::Keyword(key) if key == "when" => {
        let Some(value) = items.get(i + 1) else {
          return err(item.span, "`:when` needs a condition");
        };
        if when.is_some() {
          return err(item.span, "`:when` given twice");
        }
        when = Some(cond::parse(value, &Scope::Rule)?);
        i += 2;
      }
      Sx::Keyword(key) => return err(item.span, format!("unknown keyword `:{key}` in rule")),
      Sx::List(_) => {
        rows.push(parse_row(item)?);
        i += 1;
      }
      _ => {
        return err(
          item.span,
          "expected a (deny ...), (ask ...), or (warn ...) row",
        );
      }
    }
  }
  if rows.is_empty() {
    return err(form.span, format!("rule `{name}` has no rows"));
  }
  Ok(Rule {
    span: form.span,
    name: RuleName::from(name.as_str()),
    when,
    rows,
  })
}

fn parse_row(node: &Node) -> Result<Row, TypeError> {
  let Sx::List(items) = &node.kind else {
    return err(
      node.span,
      "expected a (deny ...), (ask ...), or (warn ...) row",
    );
  };
  let Some((head, items)) = items.split_first() else {
    return err(node.span, "expected deny, ask, or warn");
  };
  let Sx::Symbol(head_name) = &head.kind else {
    return err(head.span, "expected deny, ask, or warn");
  };
  let decision = match head_name.as_str() {
    "deny" => Kind::Deny,
    "ask" => Kind::Ask,
    "warn" => Kind::Warn,
    other => {
      return err(
        head.span,
        format!("expected deny, ask, or warn, found `{other}`"),
      );
    }
  };
  let Some((subject_node, items)) = items.split_first() else {
    return err(node.span, format!("`{head_name}` needs a pattern"));
  };
  let subject = parse_subject(subject_node)?;
  let scope = Scope::Row(subject.binders());

  let mut when = None;
  let mut reason = None;
  let mut instead = None;
  let mut i = 0;
  while i < items.len() {
    let item = &items[i];
    let Sx::Keyword(key) = &item.kind else {
      return err(item.span, format!("unexpected `{item}` after the pattern"));
    };
    let Some(value) = items.get(i + 1) else {
      return err(item.span, format!("`:{key}` needs a value"));
    };
    match key.as_str() {
      "when" => set_once(&mut when, item, cond::parse(value, &scope)?)?,
      "reason" => set_once(&mut reason, item, string(item, value)?)?,
      "instead" => set_once(&mut instead, item, string(item, value)?)?,
      other => {
        return err(
          item.span,
          format!("unknown keyword `:{other}` in `{head_name}`"),
        );
      }
    }
    i += 2;
  }
  let Some(reason) = reason else {
    return err(node.span, format!("`{head_name}` needs a :reason"));
  };
  if instead.is_none() && decision != Kind::Warn {
    return err(node.span, format!("`{head_name}` needs an :instead"));
  }
  Ok(Row {
    span: node.span,
    decision,
    subject,
    when,
    reason,
    instead,
  })
}

fn set_once<T>(
  slot: &mut Option<T>,
  key: &Node,
  value: T,
) -> Result<(), TypeError> {
  if slot.is_some() {
    return err(key.span, format!("`{key}` given twice"));
  }
  *slot = Some(value);
  Ok(())
}

fn string(
  key: &Node,
  value: &Node,
) -> Result<String, TypeError> {
  match &value.kind {
    Sx::Str(text) => Ok(text.clone()),
    _ => err(value.span, format!("`{key}` needs a string")),
  }
}

fn parse_subject(node: &Node) -> Result<Subject, TypeError> {
  match &node.kind {
    Sx::Pattern(words) => Ok(Subject::Command(parse_pattern(node, words)?)),
    Sx::List(items) => Ok(Subject::Tool(parse_tool(node, items)?)),
    _ => err(node.span, "expected a [pattern] or a tool pattern"),
  }
}

fn parse_tool(
  node: &Node,
  items: &[Node],
) -> Result<ToolPattern, TypeError> {
  let Some((head, args)) = items.split_first() else {
    return err(node.span, "expected a tool pattern such as (write ?path)");
  };
  let Sx::Symbol(head_name) = &head.kind else {
    return err(head.span, "expected a tool pattern such as (write ?path)");
  };
  let tool = match head_name.as_str() {
    "write" => FileTool::Write,
    "edit" => FileTool::Edit,
    "multi-edit" => FileTool::MultiEdit,
    "read" => FileTool::Read,
    other => return err(head.span, format!("unknown tool pattern `{other}`")),
  };
  let [arg] = args else {
    return err(node.span, format!("`({head_name} ...)` takes one path"));
  };
  let path = match &arg.kind {
    Sx::Str(text) => PathArg::Literal(text.clone()),
    Sx::Symbol(text) if text.starts_with('?') => PathArg::Var(binder(arg, text)?),
    _ => {
      return err(
        arg.span,
        format!("`({head_name} ...)` takes a string or a `?binder`"),
      );
    }
  };
  Ok(ToolPattern { tool, path })
}

fn parse_pattern(
  node: &Node,
  items: &[Node],
) -> Result<Pattern, TypeError> {
  let mut words = Vec::new();
  let mut redirects = Vec::new();
  let mut i = 0;
  while i < items.len() {
    let item = &items[i];
    let redirect = match &item.kind {
      Sx::Symbol(s) if s == ">" => Some(RedirectKind::Write),
      Sx::Symbol(s) if s == ">>" => Some(RedirectKind::Append),
      Sx::Symbol(s) if s == "<" => Some(RedirectKind::Read),
      _ => None,
    };
    match redirect {
      Some(kind) => {
        let Some(target) = items.get(i + 1) else {
          return err(item.span, format!("`{item}` needs a target"));
        };
        redirects.push(RedirectPattern {
          kind,
          target: word(target)?,
        });
        i += 2;
      }
      None => {
        words.push(word(item)?);
        i += 1;
      }
    }
  }
  Ok(Pattern::from_tokens(node.flat(), words, redirects))
}

fn word(node: &Node) -> Result<Token, TypeError> {
  let text = match &node.kind {
    Sx::Str(text) => return Ok(Token::Literal(text.clone())),
    Sx::Symbol(text) => text,
    Sx::Keyword(_) => return err(node.span, "a pattern holds words, not keywords"),
    Sx::List(_) | Sx::Pattern(_) => return err(node.span, "a pattern holds words, not lists"),
  };
  Ok(match text.as_str() {
    "*" => Token::Any,
    "..." => Token::Rest,
    "-*" => Token::Option,
    "-..." => Token::Options,
    _ if text.starts_with('?') => Token::Var(binder(node, text)?),
    _ => match text.strip_suffix("/**") {
      Some(prefix) => {
        return err(
          node.span,
          format!("path prefixes go in the condition: (under? ?p {prefix:?})"),
        );
      }
      None => Token::Literal(text.clone()),
    },
  })
}

/// `?name` to its [`Var`].
fn binder(
  node: &Node,
  text: &str,
) -> Result<Var, TypeError> {
  match text.strip_prefix('?') {
    Some("") | None => err(node.span, "binder has no name"),
    Some(name) => Ok(Var::from(name)),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::pattern::Bindings;
  use crate::segment;
  use crate::sexp;

  fn parse_text(source: &str) -> Result<File, Vec<TypeError>> {
    parse(&sexp::read_all(source).unwrap_or_else(|e| panic!("read: {e}")))
  }

  fn file(source: &str) -> File {
    parse_text(source).unwrap_or_else(|errors| {
      panic!(
        "{}",
        errors
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join("\n")
      )
    })
  }

  fn errors(source: &str) -> Vec<String> {
    match parse_text(source) {
      Ok(file) => panic!("parsed: {file:?}"),
      Err(errors) => errors.iter().map(ToString::to_string).collect(),
    }
  }

  fn error(source: &str) -> String {
    let all = errors(source);
    assert_eq!(all.len(), 1, "{all:?}");
    all.into_iter().next().unwrap()
  }

  /// The only row of the only rule of `source`.
  fn row(source: &str) -> Row {
    let mut file = file(source);
    assert_eq!(file.rules.len(), 1);
    let rule = file.rules.remove(0);
    assert_eq!(rule.rows.len(), 1);
    rule.rows.into_iter().next().unwrap()
  }

  fn command_pattern(source: &str) -> Pattern {
    match row(&format!(
      "(rule r (deny {source} :reason \"r.\" :instead \"i.\"))"
    ))
    .subject
    {
      Subject::Command(pattern) => pattern,
      other => panic!("{other:?}"),
    }
  }

  fn bindings(
    pattern: &str,
    command: &str,
  ) -> Vec<Bindings> {
    let mut segments = segment::segment(command).unwrap();
    assert_eq!(segments.commands.len(), 1);
    command_pattern(pattern).bindings(&segments.commands.remove(0))
  }

  fn bound(pairs: &[(&str, &str)]) -> Bindings {
    pairs
      .iter()
      .map(|(k, v)| (Var::from(*k), v.to_string()))
      .collect()
  }

  const TABLE: &str = r#"
    ; the shape the shipped file will take
    (rule hard-denies
      (deny [git -... stash ...]
        :reason  "jj has no dirty tree, so there is nothing to stash."
        :instead "use `jj new` or `jj describe`.")
      (deny [cat ... > *] :reason "no cat writes." :instead "use the Write tool."))

    (rule git-in-jj :when (ancestor-has? ".jj")
      (deny [git ...] :reason "this repo is managed by jj." :instead "use jj."))

    (rule tmp-writes
      (deny [... > ?out] :when (under? ?out "/tmp") :reason "no /tmp." :instead "write in the project.")
      (deny (write ?path) :when (under? ?path "/tmp") :reason "no /tmp." :instead "write in the project.")
      (warn [mktemp ...] :reason "mktemp creates files under /tmp."))
  "#;

  // --- the table ---

  #[test]
  fn a_file_parses_to_rules_in_order() {
    let file = file(TABLE);
    let names: Vec<_> = file.rules.iter().map(|r| r.name.to_string()).collect();
    assert_eq!(names, ["hard-denies", "git-in-jj", "tmp-writes"]);
    assert_eq!(file.rules[0].span, Span { line: 3, col: 5 });
    assert_eq!(file.rules[0].when, None);
    assert_eq!(file.rules[0].rows.len(), 2);
  }

  #[test]
  fn a_rule_condition_is_checked_with_no_binders() {
    let file = file(TABLE);
    assert_eq!(file.rules[1].when, Some(Cond::AncestorHas(".jj".into())));
    assert_eq!(
      error("(rule x :when (under? ?p \"/tmp\") (deny [a ?p] :reason \"r\" :instead \"i\"))"),
      "1:23: `?p` is not bound: a rule `:when` runs before any pattern matches"
    );
    assert_eq!(
      error("(rule x :when (nope) (deny [a] :reason \"r\" :instead \"i\"))"),
      "1:16: unknown predicate `nope`"
    );
  }

  #[test]
  fn a_row_carries_decision_guidance_and_position() {
    let file = file(TABLE);
    let row = &file.rules[0].rows[0];
    assert_eq!(row.span, Span { line: 4, col: 7 });
    assert_eq!(row.decision, Kind::Deny);
    assert_eq!(
      row.reason,
      "jj has no dirty tree, so there is nothing to stash."
    );
    assert_eq!(
      row.instead.as_deref(),
      Some("use `jj new` or `jj describe`.")
    );
    assert_eq!(row.when, None);
    match &row.subject {
      Subject::Command(pattern) => assert_eq!(pattern.to_string(), "[git -... stash ...]"),
      other => panic!("{other:?}"),
    }
  }

  #[test]
  fn a_row_condition_is_checked_against_its_pattern_binders() {
    let table = file(TABLE);
    let row = &table.rules[2].rows[0];
    assert_eq!(
      row.when,
      Some(Cond::Under(
        cond::Arg::Var(Var::from("out")),
        std::path::PathBuf::from("/tmp")
      ))
    );
    // Binders come from words, redirect targets, and tool paths.
    file(
      "(rule x (deny [cp ?a > ?b] :when (and (under? ?a \"/\") (under? ?b \"/\")) :reason \"r\" :instead \"i\"))",
    );
    file("(rule x (deny (edit ?p) :when (under? ?p \"/\") :reason \"r\" :instead \"i\"))");
    assert_eq!(
      error(
        "(rule x (deny [cp ?src *] :when (under? ?dst \"/tmp\") :reason \"r\" :instead \"i\"))"
      ),
      "1:41: `?dst` is not bound by this pattern"
    );
    assert_eq!(
      error(
        "(rule x (deny (write \"/x\") :when (under? ?p \"/tmp\") :reason \"r\" :instead \"i\"))"
      ),
      "1:42: `?p` is not bound by this pattern"
    );
  }

  #[test]
  fn a_warn_row_needs_no_instead() {
    let file = file(TABLE);
    let row = &file.rules[2].rows[2];
    assert_eq!(row.decision, Kind::Warn);
    assert_eq!(row.instead, None);
  }

  #[test]
  fn a_tool_pattern_names_the_tool_and_binds_the_path() {
    let file = file(TABLE);
    assert_eq!(
      file.rules[2].rows[1].subject,
      Subject::Tool(ToolPattern {
        tool: FileTool::Write,
        path: PathArg::Var(Var::from("path")),
      })
    );
    let row = row(r#"(rule r (ask (multi-edit "/etc/hosts") :reason "r." :instead "i."))"#);
    assert_eq!(
      row.subject,
      Subject::Tool(ToolPattern {
        tool: FileTool::MultiEdit,
        path: PathArg::Literal("/etc/hosts".into()),
      })
    );
    assert_eq!(row.decision, Kind::Ask);
  }

  #[test]
  fn an_empty_file_is_an_empty_table() {
    assert_eq!(file("; nothing").rules, vec![]);
  }

  // --- pattern words ---

  #[test]
  fn pattern_words_map_to_the_matcher_tokens() {
    assert_eq!(
      bindings("[git -... stash ...]", "git --no-pager stash pop"),
      vec![Bindings::new()]
    );
    assert!(bindings("[git -... stash ...]", "git log").is_empty());
    assert_eq!(bindings("[git *]", "git stash"), vec![Bindings::new()]);
    assert!(bindings("[git *]", "git stash pop").is_empty());
    assert_eq!(
      bindings("[git -* stash]", "git -v stash"),
      vec![Bindings::new()]
    );
    assert!(bindings("[git -* stash]", "git stash").is_empty());
  }

  #[test]
  fn a_string_word_is_a_literal_that_may_hold_spaces() {
    assert_eq!(
      bindings(r#"[echo "hi there"]"#, "echo 'hi there'"),
      vec![Bindings::new()]
    );
    assert!(bindings(r#"[echo "hi there"]"#, "echo hi there").is_empty());
  }

  #[test]
  fn a_binder_captures_a_word() {
    assert_eq!(
      bindings("[cp ... ?dst]", "cp -r a b"),
      vec![bound(&[("dst", "b")])]
    );
  }

  #[test]
  fn redirects_take_the_next_word_as_target() {
    assert_eq!(
      bindings("[... > ?out]", "echo hi > /tmp/x"),
      vec![bound(&[("out", "/tmp/x")])]
    );
    assert_eq!(
      bindings("[cat ... >> *]", "cat a >> b"),
      vec![Bindings::new()]
    );
    assert!(bindings("[cat ... >> *]", "cat a > b").is_empty());
    assert_eq!(bindings("[cat < in]", "cat < in"), vec![Bindings::new()]);
    assert_eq!(
      bindings("[> ?out]", "> /tmp/out"),
      vec![bound(&[("out", "/tmp/out")])]
    );
  }

  #[test]
  fn a_word_that_looks_like_a_redirect_but_is_not_stays_literal() {
    // The segmenter drops fd duplication from a command, so the only way
    // a `2>&1` word reaches the matcher is quoted.
    assert_eq!(bindings("[cmd 2>&1]", "cmd '2>&1'"), vec![Bindings::new()]);
    assert!(bindings("[cmd 2>&1]", "cmd 2>&1").is_empty());
  }

  // --- errors: file and rule shape ---

  #[test]
  fn a_top_level_form_must_be_a_rule() {
    assert_eq!(error("[git stash]"), "1:1: expected (rule ...)");
    assert_eq!(error("()"), "1:1: expected (rule ...)");
    assert_eq!(
      error("(rulez x)"),
      "1:2: expected (rule ...), found `rulez`"
    );
    assert_eq!(error("(\"rule\" x)"), "1:2: expected (rule ...)");
  }

  #[test]
  fn a_rule_needs_a_symbol_name_and_rows() {
    assert_eq!(error("(rule)"), "1:1: rule needs a name");
    assert_eq!(error("(rule \"x\")"), "1:7: rule name must be a symbol");
    assert_eq!(error("(rule x)"), "1:1: rule `x` has no rows");
    assert_eq!(
      error("(rule x :when (ancestor-has? \".jj\"))"),
      "1:1: rule `x` has no rows"
    );
  }

  #[test]
  fn a_rule_condition_is_given_once_with_a_value() {
    assert_eq!(error("(rule x :when)"), "1:9: `:when` needs a condition");
    assert_eq!(
      error(
        "(rule x :when (ancestor-has? \".jj\") :when (ancestor-has? \".git\") (deny [a] :reason \"r\" :instead \"i\"))"
      ),
      "1:37: `:when` given twice"
    );
    assert_eq!(
      error("(rule x :foo 1)"),
      "1:9: unknown keyword `:foo` in rule"
    );
    assert_eq!(
      error("(rule x banana)"),
      "1:9: expected a (deny ...), (ask ...), or (warn ...) row"
    );
  }

  // --- errors: rows ---

  #[test]
  fn a_row_starts_with_its_decision() {
    assert_eq!(
      error("(rule x (nope [a]))"),
      "1:10: expected deny, ask, or warn, found `nope`"
    );
    assert_eq!(error("(rule x ())"), "1:9: expected deny, ask, or warn");
    assert_eq!(
      error("(rule x (\"deny\" [a]))"),
      "1:10: expected deny, ask, or warn"
    );
  }

  #[test]
  fn a_row_needs_a_subject_then_only_keywords() {
    assert_eq!(error("(rule x (deny))"), "1:9: `deny` needs a pattern");
    assert_eq!(
      error("(rule x (deny \"str\" :reason \"r\" :instead \"i\"))"),
      "1:15: expected a [pattern] or a tool pattern"
    );
    assert_eq!(
      error("(rule x (deny [a] extra :reason \"r\" :instead \"i\"))"),
      "1:19: unexpected `extra` after the pattern"
    );
  }

  #[test]
  fn guidance_is_required_by_decision_kind() {
    assert_eq!(error("(rule x (deny [a]))"), "1:9: `deny` needs a :reason");
    assert_eq!(
      error("(rule x (ask [a] :instead \"i\"))"),
      "1:9: `ask` needs a :reason"
    );
    assert_eq!(
      error("(rule x (deny [a] :reason \"r\"))"),
      "1:9: `deny` needs an :instead"
    );
    assert_eq!(
      error("(rule x (ask [a] :reason \"r\"))"),
      "1:9: `ask` needs an :instead"
    );
  }

  #[test]
  fn guidance_keywords_take_one_string_each() {
    assert_eq!(
      error("(rule x (deny [a] :reason r :instead \"i\"))"),
      "1:27: `:reason` needs a string"
    );
    assert_eq!(
      error("(rule x (deny [a] :reason))"),
      "1:19: `:reason` needs a value"
    );
    assert_eq!(
      error("(rule x (deny [a] :reason \"r\" :reason \"s\" :instead \"i\"))"),
      "1:31: `:reason` given twice"
    );
    assert_eq!(
      error("(rule x (deny [a] :because \"r\"))"),
      "1:19: unknown keyword `:because` in `deny`"
    );
  }

  // --- errors: patterns ---

  #[test]
  fn a_pattern_holds_words_only() {
    assert_eq!(
      error("(rule x (deny [a :k] :reason \"r\" :instead \"i\"))"),
      "1:18: a pattern holds words, not keywords"
    );
    assert_eq!(
      error("(rule x (deny [a (b)] :reason \"r\" :instead \"i\"))"),
      "1:18: a pattern holds words, not lists"
    );
    assert_eq!(
      error("(rule x (deny [a [b]] :reason \"r\" :instead \"i\"))"),
      "1:18: a pattern holds words, not lists"
    );
  }

  #[test]
  fn path_prefixes_are_refused_with_the_condition_to_use() {
    assert_eq!(
      error("(rule x (deny [cp ... /tmp/**] :reason \"r\" :instead \"i\"))"),
      "1:23: path prefixes go in the condition: (under? ?p \"/tmp\")"
    );
  }

  #[test]
  fn redirects_and_binders_are_checked() {
    assert_eq!(
      error("(rule x (deny [a >] :reason \"r\" :instead \"i\"))"),
      "1:18: `>` needs a target"
    );
    assert_eq!(
      error("(rule x (deny [a ?] :reason \"r\" :instead \"i\"))"),
      "1:18: binder has no name"
    );
  }

  // --- errors: tool patterns ---

  #[test]
  fn tool_patterns_are_checked() {
    assert_eq!(
      error("(rule x (deny (nuke ?p) :reason \"r\" :instead \"i\"))"),
      "1:16: unknown tool pattern `nuke`"
    );
    assert_eq!(
      error("(rule x (deny (write) :reason \"r\" :instead \"i\"))"),
      "1:15: `(write ...)` takes one path"
    );
    assert_eq!(
      error("(rule x (deny (write ?p \"x\") :reason \"r\" :instead \"i\"))"),
      "1:15: `(write ...)` takes one path"
    );
    assert_eq!(
      error("(rule x (deny (write path) :reason \"r\" :instead \"i\"))"),
      "1:22: `(write ...)` takes a string or a `?binder`"
    );
    assert_eq!(
      error("(rule x (deny (write ?) :reason \"r\" :instead \"i\"))"),
      "1:22: binder has no name"
    );
    assert_eq!(
      error("(rule x (deny () :reason \"r\" :instead \"i\"))"),
      "1:15: expected a tool pattern such as (write ?path)"
    );
  }

  // --- errors: every bad form is reported ---

  #[test]
  fn one_load_reports_every_failing_rule() {
    let all =
      errors("(rule a)\n(rule b (deny [x] :reason \"r.\" :instead \"i.\"))\n(rule c (deny [y]))");
    assert_eq!(
      all,
      ["1:1: rule `a` has no rows", "3:9: `deny` needs a :reason"]
    );
  }
}
