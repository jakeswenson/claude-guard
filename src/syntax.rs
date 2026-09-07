//! From the s-expression tree to the typed rule table.
//!
//! The grammar this module accepts:
//!
//! ```text
//! file    := (rule | command | fact)*
//! rule    := (rule <name> [:when <cond>] row+)
//! row     := (deny|ask|warn <subject> [:when <cond>] :reason "..." [:instead "..."])
//! subject := [word*]                       ; a Bash command pattern
//!          | (write|edit|multi-edit|read <path>)   ; a file tool pattern
//! word    := <symbol> | "..." | > <word> | >> <word> | < <word>
//! path    := "..." | ?name
//! command := (command <name> decl)
//! decl    := (option "-c" ["--long"] [:value | :optional])*
//!            (subcommand <name> [:alias <name>]* decl)*
//!            [:inner (command [:from N]) | (script [:from N] [:when "-c"]) | (script :option "-c")]
//! fact    := (fact <name>? (exec "program" "arg"*) :lifetime fresh :timeout "1s")
//! ```
//!
//! Facts are read before rules, whatever the order in the file, so a
//! rule may name a fact declared below it. A fact must be declared in
//! the file whose rules name it, or be built in; `:lifetime` and
//! `:timeout` have no defaults (ADR 0002).
//!
//! Pattern words map to the tokens in [`pattern`]: `*`, `...`, `-*`,
//! `-...`, `?name`, and literals. A string in a bracket is a literal that
//! may hold spaces. The `<path>/**` form is refused here: path constraints
//! belong in the condition, as `(under? ?p "/tmp")`.
//!
//! `:instead` is required on deny and ask, optional on warn. A condition
//! is checked by [`cond`] against the binders its pattern declares and
//! the facts the registry knows; a rule's `:when` may use no binders.
//!
//! Every error names the node it is about. Top-level forms are checked
//! independently, so one load reports every rule that is wrong.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use crate::cond::{self, Cond, Scope};
use crate::elaborate::{Arity, Declaration, InnerSpec, OptionSpec};
use crate::facts::{Exec, FactName, Facts, Lifetime};
use crate::pattern::{Pattern, RedirectPattern, Token, Var};
use crate::rules::{Kind, RuleName};
use crate::segment::RedirectKind;
use crate::sexp::{Kind as Sx, Node, Span};

/// A whole rule file: rules in evaluation order, and the command and
/// fact declarations it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
  pub rules: Vec<Rule>,
  pub commands: Vec<CommandDecl>,
  pub facts: Vec<FactDecl>,
}

/// One `(fact name (exec ...) ...)` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactDecl {
  pub span: Span,
  pub name: FactName,
  pub program: String,
  pub args: Vec<String>,
  pub lifetime: Lifetime,
  pub timeout: Duration,
}

impl FactDecl {
  /// The fact this declaration registers.
  pub fn fact(&self) -> Exec {
    Exec {
      program: self.program.clone(),
      args: self.args.clone(),
      timeout: self.timeout,
    }
  }
}

/// One `(command name ...)` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDecl {
  pub span: Span,
  pub name: String,
  pub declaration: Declaration,
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

impl fmt::Display for Subject {
  /// The subject as written: `[git -... stash ...]` or `(write ?path)`.
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Subject::Command(pattern) => write!(f, "{pattern}"),
      Subject::Tool(tool) => write!(f, "{tool}"),
    }
  }
}

impl fmt::Display for ToolPattern {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    let tool = match self.tool {
      FileTool::Write => "write",
      FileTool::Edit => "edit",
      FileTool::MultiEdit => "multi-edit",
      FileTool::Read => "read",
    };
    match &self.path {
      PathArg::Literal(path) => write!(f, "({tool} {path:?})"),
      PathArg::Var(var) => write!(f, "({tool} ?{var})"),
    }
  }
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

const EXPECTED_FORM: &str = "expected (rule ...), (command ...), or (fact ...)";

/// Check every top-level form. Conditions may name the facts in `base`
/// and the facts the file declares. All the errors, or the table.
pub fn parse(
  forms: &[Node],
  base: &Facts,
) -> Result<File, Vec<TypeError>> {
  let mut file = File {
    rules: Vec::new(),
    commands: Vec::new(),
    facts: Vec::new(),
  };
  let mut errors = Vec::new();

  // Declared facts first, so a rule may name one declared anywhere in
  // the file.
  let mut facts = base.clone();
  for form in forms.iter().filter(|f| head_symbol(f) == Some("fact")) {
    match parse_fact(form) {
      Ok(decl) if base.get(&decl.name).is_some() => errors.push(TypeError {
        span: decl.span,
        message: format!("`{}` is built in and cannot be redeclared", decl.name),
      }),
      Ok(decl) if file.facts.iter().any(|f| f.name == decl.name) => errors.push(TypeError {
        span: decl.span,
        message: format!("fact `{}` declared twice", decl.name),
      }),
      Ok(decl) => {
        facts.declare(decl.name.clone(), decl.fact());
        file.facts.push(decl);
      }
      Err(e) => errors.push(e),
    }
  }

  for form in forms {
    let result = match head_symbol(form) {
      Some("fact") => continue,
      Some("rule") => parse_rule(form, &facts).map(|rule| file.rules.push(rule)),
      Some("command") => parse_command(form).map(|decl| file.commands.push(decl)),
      Some(other) => err(
        form_head(form).span,
        format!("{EXPECTED_FORM}, found `{other}`"),
      ),
      None => err(form_head(form).span, EXPECTED_FORM),
    };
    if let Err(e) = result {
      errors.push(e);
    }
  }
  if errors.is_empty() {
    Ok(file)
  } else {
    Err(errors)
  }
}

/// The head symbol of a list form, if it has one.
fn head_symbol(form: &Node) -> Option<&str> {
  match &form.kind {
    Sx::List(items) => match items.first().map(|n| &n.kind) {
      Some(Sx::Symbol(s)) => Some(s.as_str()),
      _ => None,
    },
    _ => None,
  }
}

/// The node an error about a form's head points at: the head when there
/// is one, else the form.
fn form_head(form: &Node) -> &Node {
  match &form.kind {
    Sx::List(items) => items.first().unwrap_or(form),
    _ => form,
  }
}

fn parse_rule(
  form: &Node,
  facts: &Facts,
) -> Result<Rule, TypeError> {
  let Sx::List(items) = &form.kind else {
    return err(form.span, EXPECTED_FORM);
  };
  let Some((_head, items)) = items.split_first() else {
    return err(form.span, EXPECTED_FORM);
  };
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
        when = Some(cond::parse(value, &Scope::Rule, facts)?);
        i += 2;
      }
      Sx::Keyword(key) => return err(item.span, format!("unknown keyword `:{key}` in rule")),
      Sx::List(_) => {
        rows.push(parse_row(item, facts)?);
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

fn parse_row(
  node: &Node,
  facts: &Facts,
) -> Result<Row, TypeError> {
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
      "when" => set_once(&mut when, item, cond::parse(value, &scope, facts)?)?,
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

// --- command declarations ---

/// `(command name (option ...)* (subcommand ...)* [:inner ...])`.
/// `(fact <name>? (exec "program" "arg"*) :lifetime fresh :timeout "1s")`.
fn parse_fact(form: &Node) -> Result<FactDecl, TypeError> {
  let Sx::List(items) = &form.kind else {
    return err(form.span, EXPECTED_FORM);
  };
  let [_head, name_node, rest @ ..] = items.as_slice() else {
    return err(form.span, "fact needs a name");
  };
  let name = match &name_node.kind {
    Sx::Symbol(name) if name.ends_with('?') && name.len() > 1 => name,
    _ => {
      return err(
        name_node.span,
        "fact name must be a symbol ending in `?`, such as `in-jj-repo?`",
      );
    }
  };
  let Some((body, keywords)) = rest.split_first() else {
    return err(
      form.span,
      format!("(fact {name} ...) needs an (exec \"program\" \"arg\"...) body"),
    );
  };
  let (program, args) = parse_exec(body)?;

  let mut lifetime = None;
  let mut timeout = None;
  let mut i = 0;
  while i < keywords.len() {
    let item = &keywords[i];
    let Sx::Keyword(key) = &item.kind else {
      return err(item.span, format!("unexpected `{item}` after (exec ...)"));
    };
    let Some(value) = keywords.get(i + 1) else {
      return err(item.span, format!("`:{key}` needs a value"));
    };
    match key.as_str() {
      "lifetime" => {
        let parsed = match &value.kind {
          Sx::Symbol(s) if s == "fresh" => Lifetime::Fresh,
          Sx::Symbol(other) => {
            return err(
              value.span,
              format!("`:lifetime` is `fresh` in this version, not `{other}`"),
            );
          }
          _ => return err(value.span, "`:lifetime` takes a symbol: fresh"),
        };
        set_once(&mut lifetime, item, parsed)?;
      }
      "timeout" => {
        let Sx::Str(text) = &value.kind else {
          return err(
            value.span,
            "`:timeout` takes a duration string such as \"1s\" or \"500ms\"",
          );
        };
        set_once(&mut timeout, item, parse_duration(value, text)?)?;
      }
      other => return err(item.span, format!("unknown keyword `:{other}` in fact")),
    }
    i += 2;
  }
  let Some(lifetime) = lifetime else {
    return err(
      form.span,
      format!("(fact {name} ...) needs :lifetime; there is no default"),
    );
  };
  let Some(timeout) = timeout else {
    return err(
      form.span,
      format!("(fact {name} ...) needs :timeout; there is no default"),
    );
  };
  Ok(FactDecl {
    span: form.span,
    name: FactName::from(name.as_str()),
    program,
    args,
    lifetime,
    timeout,
  })
}

/// `(exec "program" "arg"*)`: the program and its leading arguments.
fn parse_exec(node: &Node) -> Result<(String, Vec<String>), TypeError> {
  const EXPECTED: &str = "expected (exec \"program\" \"arg\"...)";
  let Sx::List(items) = &node.kind else {
    return err(node.span, EXPECTED);
  };
  let Some((head, words)) = items.split_first() else {
    return err(node.span, EXPECTED);
  };
  match &head.kind {
    Sx::Symbol(s) if s == "exec" => {}
    Sx::Symbol(other) => return err(head.span, format!("{EXPECTED}, found `{other}`")),
    _ => return err(head.span, EXPECTED),
  }
  let Some((program, args)) = words.split_first() else {
    return err(node.span, "`exec` needs a program");
  };
  let text = |node: &Node| match &node.kind {
    Sx::Str(s) => Ok(s.clone()),
    _ => err(
      node.span,
      "`exec` takes its program and arguments as strings",
    ),
  };
  Ok((
    text(program)?,
    args.iter().map(text).collect::<Result<Vec<_>, _>>()?,
  ))
}

/// A duration as a rule file writes it: `"1s"`, `"500ms"`, `"1.5s"`.
/// Must be more than zero.
fn parse_duration(
  node: &Node,
  text: &str,
) -> Result<Duration, TypeError> {
  let signed: jiff::SignedDuration = text.parse().map_err(|e| TypeError {
    span: node.span,
    message: format!("`:timeout` is not a duration such as \"1s\" or \"500ms\": {e}"),
  })?;
  let duration: Duration = signed.try_into().map_err(|_| TypeError {
    span: node.span,
    message: "`:timeout` must be more than zero".into(),
  })?;
  if duration.is_zero() {
    return err(node.span, "`:timeout` must be more than zero");
  }
  Ok(duration)
}

fn parse_command(form: &Node) -> Result<CommandDecl, TypeError> {
  let Sx::List(items) = &form.kind else {
    return err(form.span, EXPECTED_FORM);
  };
  let [_head, name_node, body @ ..] = items.as_slice() else {
    return err(form.span, "command needs a name");
  };
  let Sx::Symbol(name) = &name_node.kind else {
    return err(name_node.span, "command name must be a symbol");
  };
  let (declaration, aliases) = parse_declaration(form, body, false)?;
  debug_assert!(aliases.is_empty());
  Ok(CommandDecl {
    span: form.span,
    name: name.clone(),
    declaration,
  })
}

/// The body shared by `(command ...)` and `(subcommand ...)`: options,
/// subcommands, `:inner`, and, for a subcommand, `:alias`. Returns the
/// declaration and the aliases.
fn parse_declaration(
  form: &Node,
  body: &[Node],
  is_subcommand: bool,
) -> Result<(Declaration, Vec<String>), TypeError> {
  let mut declaration = Declaration::default();
  let mut aliases = Vec::new();
  let mut i = 0;
  while i < body.len() {
    let item = &body[i];
    match &item.kind {
      Sx::List(_) => match head_symbol(item) {
        Some("option") => {
          let spec = parse_option(item)?;
          for earlier in &declaration.options {
            if let (Some(a), Some(b)) = (earlier.short, spec.short)
              && a == b
            {
              return err(item.span, format!("option `-{a}` declared twice"));
            }
            if let (Some(a), Some(b)) = (&earlier.long, &spec.long)
              && a == b
            {
              return err(item.span, format!("option `--{a}` declared twice"));
            }
          }
          declaration.options.push(spec);
        }
        Some("subcommand") => {
          let (sub_name, sub, sub_aliases) = parse_subcommand(item)?;
          for name in std::iter::once(&sub_name).chain(&sub_aliases) {
            if declaration
              .subcommands
              .insert(name.clone(), sub.clone())
              .is_some()
            {
              return err(item.span, format!("subcommand `{name}` declared twice"));
            }
          }
        }
        _ => return err(item.span, "expected (option ...) or (subcommand ...)"),
      },
      Sx::Keyword(key) => {
        let Some(value) = body.get(i + 1) else {
          return err(item.span, format!("`:{key}` needs a value"));
        };
        match key.as_str() {
          "inner" => {
            if declaration.inner.is_some() {
              return err(item.span, "`:inner` given twice");
            }
            declaration.inner = Some(parse_inner(value, &declaration)?);
          }
          "alias" if is_subcommand => {
            let Sx::Symbol(alias) = &value.kind else {
              return err(value.span, "`:alias` takes a symbol");
            };
            aliases.push(alias.clone());
          }
          other => return err(item.span, format!("unknown keyword `:{other}` in command")),
        }
        i += 2;
        continue;
      }
      _ => {
        return err(
          item.span,
          "expected (option ...), (subcommand ...), or :inner",
        );
      }
    }
    i += 1;
  }
  let _ = form;
  Ok((declaration, aliases))
}

/// `(option "-C" "--git-dir" [:value | :optional])`.
fn parse_option(node: &Node) -> Result<OptionSpec, TypeError> {
  let Sx::List(items) = &node.kind else {
    unreachable!("caller checked");
  };
  let mut spec = OptionSpec {
    short: None,
    long: None,
    arity: Arity::None,
  };
  let mut named = false;
  for item in &items[1..] {
    match &item.kind {
      Sx::Str(text) => {
        named = true;
        let mut chars = text.strip_prefix('-').unwrap_or("").chars();
        match (chars.next(), text.strip_prefix("--")) {
          (Some('-'), Some(long)) if !long.is_empty() && !long.starts_with('-') => {
            if spec.long.replace(long.to_string()).is_some() {
              return err(item.span, "an option has one long form");
            }
          }
          (Some(short), None) if chars.next().is_none() && short != '-' => {
            if spec.short.replace(short).is_some() {
              return err(item.span, "an option has one short form");
            }
          }
          _ => {
            return err(
              item.span,
              format!("option names look like \"-c\" or \"--long\", not {text:?}"),
            );
          }
        }
      }
      Sx::Keyword(key) if key == "value" => spec.arity = Arity::One,
      Sx::Keyword(key) if key == "optional" => spec.arity = Arity::Optional,
      Sx::Keyword(key) => return err(item.span, format!("unknown keyword `:{key}` in option")),
      _ => {
        return err(
          item.span,
          "expected an option name string, :value, or :optional",
        );
      }
    }
  }
  if !named {
    return err(node.span, "option needs a name");
  }
  Ok(spec)
}

/// `(subcommand name [:alias other]* (option ...)* (subcommand ...)* [:inner ...])`.
fn parse_subcommand(node: &Node) -> Result<(String, Declaration, Vec<String>), TypeError> {
  let Sx::List(items) = &node.kind else {
    unreachable!("caller checked");
  };
  let [_head, name_node, body @ ..] = items.as_slice() else {
    return err(node.span, "subcommand needs a name");
  };
  let Sx::Symbol(name) = &name_node.kind else {
    return err(name_node.span, "subcommand name must be a symbol");
  };
  let (declaration, aliases) = parse_declaration(node, body, true)?;
  Ok((name.clone(), declaration, aliases))
}

/// `(command [:from N])`, `(script :from N [:when "-c"])`, or
/// `(script :option "-c")`. A flag named here must be declared above it.
fn parse_inner(
  node: &Node,
  declaration: &Declaration,
) -> Result<InnerSpec, TypeError> {
  let Sx::List(items) = &node.kind else {
    return err(node.span, "`:inner` takes (command ...) or (script ...)");
  };
  let Some((head, args)) = items.split_first() else {
    return err(node.span, "`:inner` takes (command ...) or (script ...)");
  };
  let Sx::Symbol(kind) = &head.kind else {
    return err(head.span, "`:inner` takes (command ...) or (script ...)");
  };
  let mut from = None;
  let mut when = None;
  let mut option = None;
  let mut i = 0;
  while i < args.len() {
    let Sx::Keyword(key) = &args[i].kind else {
      return err(args[i].span, "expected :from, :when, or :option");
    };
    let Some(value) = args.get(i + 1) else {
      return err(args[i].span, format!("`:{key}` needs a value"));
    };
    match (kind.as_str(), key.as_str()) {
      (_, "from") => {
        let parsed = match &value.kind {
          Sx::Symbol(digits) => digits.parse::<usize>().ok(),
          _ => None,
        };
        let Some(n) = parsed else {
          return err(value.span, "`:from` takes a number");
        };
        from = Some(n);
      }
      ("script", "when") => when = Some(declared_flag(value, declaration)?),
      ("script", "option") => option = Some(declared_flag(value, declaration)?),
      (_, other) => {
        return err(
          args[i].span,
          format!("unknown keyword `:{other}` in `{kind}`"),
        );
      }
    }
    i += 2;
  }
  match (kind.as_str(), from, when, option) {
    ("command", from, None, None) => Ok(InnerSpec::Command {
      from: from.unwrap_or(0),
    }),
    ("script", None, None, Some(flag)) => Ok(InnerSpec::ScriptOption { flag }),
    ("script", Some(_), _, Some(_)) => err(node.span, "`:option` and `:from` do not combine"),
    ("script", from, when_flag, None) => Ok(InnerSpec::Script {
      from: from.unwrap_or(0),
      when_flag,
    }),
    _ => err(head.span, "`:inner` takes (command ...) or (script ...)"),
  }
}

/// A flag string naming an option declared on `declaration`, returned
/// without its dashes as the elaborator names flags.
fn declared_flag(
  node: &Node,
  declaration: &Declaration,
) -> Result<String, TypeError> {
  let Sx::Str(text) = &node.kind else {
    return err(node.span, "expected an option name string");
  };
  let name = text.trim_start_matches('-');
  let declared = declaration.options.iter().any(|o| {
    o.long.as_deref() == Some(name) || (name.chars().count() == 1 && o.short == name.chars().next())
  });
  if !declared || name.is_empty() {
    return err(
      node.span,
      format!("{text:?} names an option this command does not declare"),
    );
  }
  Ok(name.to_string())
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

/// Check one node as a row subject: a `[pattern]` or a tool pattern.
/// Public so the spec runner can check a bare subject.
pub fn parse_subject(node: &Node) -> Result<Subject, TypeError> {
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
  use crate::facts::Arg;
  use crate::pattern::Bindings;
  use crate::segment;
  use crate::sexp;

  fn parse_text(source: &str) -> Result<File, Vec<TypeError>> {
    parse(
      &sexp::read_all(source).unwrap_or_else(|e| panic!("read: {e}")),
      &Facts::builtin(),
    )
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
    assert_eq!(
      file.rules[1].when,
      Some(Cond::Fact {
        name: "ancestor-has?".into(),
        args: vec![Arg::Literal(".jj".into())],
      })
    );
    assert_eq!(
      error("(rule x :when (under? ?p \"/tmp\") (deny [a ?p] :reason \"r\" :instead \"i\"))"),
      "1:23: `?p` is not bound: a rule `:when` runs before any pattern matches"
    );
    assert_eq!(
      error("(rule x :when (nope) (deny [a] :reason \"r\" :instead \"i\"))"),
      "1:16: unknown fact `nope`"
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
      Some(Cond::Fact {
        name: "under?".into(),
        args: vec![Arg::Var(Var::from("out")), Arg::Literal("/tmp".into())],
      })
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
    let empty = file("; nothing");
    assert_eq!(empty.rules, vec![]);
    assert_eq!(empty.commands, vec![]);
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

  // --- fact declarations ---

  #[test]
  fn a_fact_declaration_parses_and_registers_its_name() {
    let parsed = file(
      "(rule r (deny [git stash] :when (managed?) :reason \"r.\" :instead \"i.\"))\n\
       (fact managed? (exec \"sh\" \"-c\" \"echo yes\") :lifetime fresh :timeout \"1s\")",
    );
    assert_eq!(parsed.facts.len(), 1);
    let decl = &parsed.facts[0];
    assert_eq!(decl.span, Span { line: 2, col: 1 });
    assert_eq!(decl.name, FactName::from("managed?"));
    assert_eq!(decl.program, "sh");
    assert_eq!(decl.args, vec!["-c", "echo yes"]);
    assert_eq!(decl.lifetime, Lifetime::Fresh);
    assert_eq!(decl.timeout, Duration::from_secs(1));
    // The rule above the declaration named it.
    assert_eq!(
      parsed.rules[0].rows[0].when,
      Some(Cond::Fact {
        name: "managed?".into(),
        args: vec![],
      })
    );
    // Arguments at the call site are strings or in-scope binders.
    file(
      "(fact f? (exec \"p\") :lifetime fresh :timeout \"500ms\")\n\
       (rule r (deny [cp ?a ?b] :when (f? \"x\" ?a ?b) :reason \"r.\" :instead \"i.\"))",
    );
    assert_eq!(
      error(
        "(fact f? (exec \"p\") :lifetime fresh :timeout \"1s\")\n\
         (rule r (deny [cp ?a] :when (f? ?z) :reason \"r.\" :instead \"i.\"))"
      ),
      "2:33: `?z` is not bound by this pattern"
    );
  }

  #[test]
  fn a_fact_declaration_is_checked_part_by_part() {
    let ok = "(exec \"p\") :lifetime fresh :timeout \"1s\"";
    assert_eq!(error("(fact)"), "1:1: fact needs a name");
    assert_eq!(
      error(&format!("(fact managed {ok})")),
      "1:7: fact name must be a symbol ending in `?`, such as `in-jj-repo?`"
    );
    assert_eq!(
      error(&format!("(fact \"m?\" {ok})")),
      "1:7: fact name must be a symbol ending in `?`, such as `in-jj-repo?`"
    );
    assert_eq!(
      error(&format!("(fact ? {ok})")),
      "1:7: fact name must be a symbol ending in `?`, such as `in-jj-repo?`"
    );
    assert_eq!(
      error("(fact m?)"),
      "1:1: (fact m? ...) needs an (exec \"program\" \"arg\"...) body"
    );
    assert_eq!(
      error("(fact m? \"p\" :lifetime fresh :timeout \"1s\")"),
      "1:10: expected (exec \"program\" \"arg\"...)"
    );
    assert_eq!(
      error("(fact m? (run \"p\") :lifetime fresh :timeout \"1s\")"),
      "1:11: expected (exec \"program\" \"arg\"...), found `run`"
    );
    assert_eq!(
      error("(fact m? (exec) :lifetime fresh :timeout \"1s\")"),
      "1:10: `exec` needs a program"
    );
    assert_eq!(
      error("(fact m? (exec sh) :lifetime fresh :timeout \"1s\")"),
      "1:16: `exec` takes its program and arguments as strings"
    );
    assert_eq!(
      error("(fact m? (exec \"sh\" -c) :lifetime fresh :timeout \"1s\")"),
      "1:21: `exec` takes its program and arguments as strings"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :timeout \"1s\")"),
      "1:1: (fact m? ...) needs :lifetime; there is no default"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh)"),
      "1:1: (fact m? ...) needs :timeout; there is no default"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime session :timeout \"1s\")"),
      "1:31: `:lifetime` is `fresh` in this version, not `session`"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime \"fresh\" :timeout \"1s\")"),
      "1:31: `:lifetime` takes a symbol: fresh"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh :lifetime fresh :timeout \"1s\")"),
      "1:37: `:lifetime` given twice"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh :timeout 1)"),
      "1:46: `:timeout` takes a duration string such as \"1s\" or \"500ms\""
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh :timeout \"soon\")"),
      "1:46: `:timeout` is not a duration such as \"1s\" or \"500ms\": failed to parse input in the \"friendly\" duration format: expected duration to start with a unit value (a decimal integer) after an optional sign, but no integer was found"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh :timeout \"0s\")"),
      "1:46: `:timeout` must be more than zero"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh :timeout \"-1s\")"),
      "1:46: `:timeout` must be more than zero"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh :timeout)"),
      "1:37: `:timeout` needs a value"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") :lifetime fresh :timeout \"1s\" :memo yes)"),
      "1:51: unknown keyword `:memo` in fact"
    );
    assert_eq!(
      error("(fact m? (exec \"p\") fresh :timeout \"1s\")"),
      "1:21: unexpected `fresh` after (exec ...)"
    );
    // Durations in the friendly format.
    let decl = |t: &str| {
      file(&format!(
        "(fact m? (exec \"p\") :lifetime fresh :timeout \"{t}\")"
      ))
      .facts[0]
        .timeout
    };
    assert_eq!(decl("500ms"), Duration::from_millis(500));
    assert_eq!(decl("1.5s"), Duration::from_millis(1500));
    assert_eq!(decl("2m"), Duration::from_secs(120));
  }

  #[test]
  fn a_fact_is_declared_once_and_never_over_a_builtin() {
    assert_eq!(
      error(
        "(fact m? (exec \"p\") :lifetime fresh :timeout \"1s\")\n\
         (fact m? (exec \"q\") :lifetime fresh :timeout \"1s\")"
      ),
      "2:1: fact `m?` declared twice"
    );
    assert_eq!(
      error("(fact ancestor-has? (exec \"p\") :lifetime fresh :timeout \"1s\")"),
      "1:1: `ancestor-has?` is built in and cannot be redeclared"
    );
    // A bad declaration and a rule naming it are two errors, and the
    // rule's is the useful one: the name never registered.
    assert_eq!(
      errors(
        "(fact m? (exec \"p\") :lifetime fresh)\n\
         (rule r (deny [x] :when (m?) :reason \"r.\" :instead \"i.\"))"
      ),
      [
        "1:1: (fact m? ...) needs :timeout; there is no default",
        "2:26: unknown fact `m?`"
      ]
    );
  }

  #[test]
  fn a_top_level_form_must_be_a_rule_a_command_or_a_fact() {
    let expected = "expected (rule ...), (command ...), or (fact ...)";
    assert_eq!(error("[git stash]"), format!("1:1: {expected}"));
    assert_eq!(error("()"), format!("1:1: {expected}"));
    assert_eq!(
      error("(rulez x)"),
      format!("1:2: {expected}, found `rulez`")
    );
    assert_eq!(error("(\"rule\" x)"), format!("1:2: {expected}"));
  }

  // --- command declarations ---

  fn command(source: &str) -> CommandDecl {
    let mut file = file(source);
    assert_eq!(file.commands.len(), 1);
    file.commands.remove(0)
  }

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

  #[test]
  fn a_command_declares_options_with_their_arity() {
    let decl = command(
      "(command git (option \"-C\" :value) (option \"-P\" \"--no-pager\") (option \"--color\" :optional))",
    );
    assert_eq!(decl.name, "git");
    assert_eq!(decl.span, Span { line: 1, col: 1 });
    assert_eq!(
      decl.declaration.options,
      vec![
        opt(Some('C'), None, Arity::One),
        opt(Some('P'), Some("no-pager"), Arity::None),
        opt(None, Some("color"), Arity::Optional),
      ]
    );
    assert_eq!(decl.declaration.inner, None);
    assert!(decl.declaration.subcommands.is_empty());
  }

  #[test]
  fn subcommands_nest_and_aliases_share_the_declaration() {
    let decl = command(
      "(command git (option \"-C\" :value) (subcommand stash :alias save (option \"-q\") (subcommand pop)))",
    );
    let stash = &decl.declaration.subcommands["stash"];
    assert_eq!(stash.options, vec![opt(Some('q'), None, Arity::None)]);
    assert!(stash.subcommands.contains_key("pop"));
    assert_eq!(decl.declaration.subcommands["save"], *stash);
  }

  #[test]
  fn inner_forms_map_to_their_specs() {
    let inner = |source: &str| command(source).declaration.inner;
    assert_eq!(
      inner("(command sudo :inner (command))"),
      Some(InnerSpec::Command { from: 0 })
    );
    assert_eq!(
      inner("(command timeout :inner (command :from 1))"),
      Some(InnerSpec::Command { from: 1 })
    );
    assert_eq!(
      inner("(command ssh :inner (script :from 1))"),
      Some(InnerSpec::Script {
        from: 1,
        when_flag: None
      })
    );
    assert_eq!(
      inner("(command bash (option \"-c\") :inner (script :from 0 :when \"-c\"))"),
      Some(InnerSpec::Script {
        from: 0,
        when_flag: Some("c".into())
      })
    );
    assert_eq!(
      inner(
        "(command nu (option \"-c\" \"--commands\" :value) :inner (script :option \"--commands\"))"
      ),
      Some(InnerSpec::ScriptOption {
        flag: "commands".into()
      })
    );
  }

  #[test]
  fn the_builtin_commands_file_type_checks() {
    let file = file(crate::load::BUILTIN_COMMANDS);
    let names: Vec<_> = file.commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
      names,
      [
        "sudo", "env", "nice", "nohup", "timeout", "xargs", "ssh", "bash", "sh", "nu", "python3"
      ]
    );
    assert!(file.commands.iter().all(|c| c.declaration.inner.is_some()));
    assert!(file.rules.is_empty());
  }

  #[test]
  fn command_declarations_are_checked() {
    assert_eq!(error("(command)"), "1:1: command needs a name");
    assert_eq!(
      error("(command \"git\")"),
      "1:10: command name must be a symbol"
    );
    assert_eq!(
      error("(command git banana)"),
      "1:14: expected (option ...), (subcommand ...), or :inner"
    );
    assert_eq!(
      error("(command git (flag \"-x\"))"),
      "1:14: expected (option ...) or (subcommand ...)"
    );
    assert_eq!(
      error("(command git :alias g)"),
      "1:14: unknown keyword `:alias` in command"
    );
    assert_eq!(error("(command git (option))"), "1:14: option needs a name");
    assert_eq!(
      error("(command git (option \"C\"))"),
      "1:22: option names look like \"-c\" or \"--long\", not \"C\""
    );
    assert_eq!(
      error("(command git (option \"---x\"))"),
      "1:22: option names look like \"-c\" or \"--long\", not \"---x\""
    );
    assert_eq!(
      error("(command git (option \"-C\" \"-D\"))"),
      "1:27: an option has one short form"
    );
    assert_eq!(
      error("(command git (option \"-C\" :maybe))"),
      "1:27: unknown keyword `:maybe` in option"
    );
    assert_eq!(
      error("(command git (option \"-C\") (option \"-C\"))"),
      "1:28: option `-C` declared twice"
    );
    assert_eq!(
      error("(command git (option \"--x\") (option \"-y\" \"--x\"))"),
      "1:29: option `--x` declared twice"
    );
    assert_eq!(
      error("(command git (subcommand))"),
      "1:14: subcommand needs a name"
    );
    assert_eq!(
      error("(command git (subcommand a) (subcommand b :alias a))"),
      "1:29: subcommand `a` declared twice"
    );
    assert_eq!(
      error("(command git :inner)"),
      "1:14: `:inner` needs a value"
    );
    assert_eq!(
      error("(command git :inner (command) :inner (command))"),
      "1:31: `:inner` given twice"
    );
    assert_eq!(
      error("(command git :inner (spawn))"),
      "1:22: `:inner` takes (command ...) or (script ...)"
    );
    assert_eq!(
      error("(command git :inner (command :from x))"),
      "1:36: `:from` takes a number"
    );
    assert_eq!(
      error("(command git :inner (command :when \"-c\"))"),
      "1:30: unknown keyword `:when` in `command`"
    );
    assert_eq!(
      error("(command bash :inner (script :when \"-c\"))"),
      "1:36: \"-c\" names an option this command does not declare"
    );
    assert_eq!(
      error("(command nu (option \"-c\" :value) :inner (script :from 0 :option \"-c\"))"),
      "1:41: `:option` and `:from` do not combine"
    );
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
