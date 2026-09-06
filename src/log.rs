//! The decision log: one typed record per hook call, one JSONL file per
//! session. Principles in `docs/design/decision-log/02-principles.md`.
//!
//! [`Record`] is the only shape that crosses the file boundary. [`Writer`]
//! and [`Reader`] are traits so the hook, the escalation logic, and the
//! tests can share one record type without sharing a disk. [`Store`] is the
//! file-backed implementation; [`Memory`] is the in-memory one.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::elaborate::Elaborated;
use crate::input::{
  AgentId, Envelope, Event, McpServer, McpTool, SessionId, Tool, ToolName, ToolUseId, WorkingDir,
  string_id,
};
use crate::output::Decision;
use crate::pattern::Bindings;
use crate::rules::{Context, PatternText, RuleName, Seen, Verdict};
use crate::segment::SimpleCommand;

/// The record's shape. A reader that meets a version it does not know
/// skips the line, so every change to [`Record`] that an old reader could
/// misread adds a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u32)]
pub enum SchemaVersion {
  V1 = 1,
}

impl SchemaVersion {
  pub const CURRENT: SchemaVersion = SchemaVersion::V1;
}

/// What the guard did. `Pass` is a record too: no record means the guard
/// was never called. `Observed` is an event the guard records without
/// deciding anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
  Pass,
  Deny,
  Ask,
  Warn,
  Observed,
}

/// What the guard looked at, typed by tool. Never null: an unmodeled tool
/// logs its whole input as `Raw`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
  /// A Bash call: the verbatim text for escalation to compare, and the
  /// segmentation the rules saw, so a reader never parses bash itself.
  Bash {
    command: String,
    commands: Vec<SimpleCommand>,
    /// Each command as the matcher saw it: options with their values,
    /// subcommands, inner commands and scripts. Absent on lines written
    /// before elaboration existed.
    #[serde(default)]
    elaborated: Vec<Elaborated>,
    uninspected: Vec<String>,
    /// The parser's message when it refused the command. `commands` is
    /// empty in that case.
    parse_error: Option<String>,
  },
  /// Write, Edit, MultiEdit, Read.
  Path(PathBuf),
  /// WebFetch.
  Url(String),
  /// Glob and Grep.
  Search {
    pattern: String,
    path: Option<PathBuf>,
  },
  /// An MCP tool, with its input whole.
  Mcp {
    server: McpServer,
    tool: McpTool,
    input: serde_json::Value,
  },
  /// Any other tool, whole.
  Raw(serde_json::Value),
}

impl Subject {
  fn of(ctx: &Context) -> Subject {
    match (&ctx.input.tool, &ctx.seen) {
      (Tool::Bash { command }, Seen::Bash(Ok(segments))) => Subject::Bash {
        command: command.clone(),
        commands: segments.commands.clone(),
        elaborated: ctx.elaborated.clone(),
        uninspected: segments.uninspected.clone(),
        parse_error: None,
      },
      (Tool::Bash { command }, Seen::Bash(Err(e))) => Subject::Bash {
        command: command.clone(),
        commands: Vec::new(),
        elaborated: Vec::new(),
        uninspected: Vec::new(),
        parse_error: Some(e.to_string()),
      },
      (Tool::Bash { command }, _) => Subject::Bash {
        command: command.clone(),
        commands: Vec::new(),
        elaborated: Vec::new(),
        uninspected: Vec::new(),
        parse_error: Some("command was not segmented".into()),
      },
      (
        Tool::Write { path } | Tool::Edit { path } | Tool::MultiEdit { path } | Tool::Read { path },
        _,
      ) => Subject::Path(path.clone()),
      (Tool::WebFetch { url }, _) => Subject::Url(url.clone()),
      (Tool::Glob { pattern, path } | Tool::Grep { pattern, path }, _) => Subject::Search {
        pattern: pattern.clone(),
        path: path.clone(),
      },
      (
        Tool::Mcp {
          server,
          tool,
          input,
          ..
        },
        _,
      ) => Subject::Mcp {
        server: server.clone(),
        tool: tool.clone(),
        input: input.clone(),
      },
      (Tool::Other { input, .. }, _) => Subject::Raw(input.clone()),
    }
  }
}

string_id! {
  /// The text the model or the user saw, exactly as rendered.
  Reason
}

/// One line of a session file. Every key is present on every line; a
/// field that does not apply is `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
  pub v: SchemaVersion,
  pub ts: Timestamp,
  pub event: Event,
  pub session_id: SessionId,
  /// Absent on events without a tool call, such as SessionStart.
  pub tool_use_id: Option<ToolUseId>,
  pub agent_id: Option<AgentId>,
  pub cwd: WorkingDir,
  pub tool: Option<ToolName>,
  pub subject: Subject,
  pub outcome: Outcome,
  pub rule: Option<RuleName>,
  pub pattern: Option<PatternText>,
  /// What the row's binders captured; `null` when no row fired. Absent
  /// on lines written before it existed.
  #[serde(default)]
  pub bindings: Option<Bindings>,
  pub reason: Option<Reason>,
}

impl Record {
  /// The record for one PreToolUse call. `None` for the verdict is a pass.
  pub fn pre_tool_use(
    ctx: &Context,
    verdict: Option<&Verdict>,
    ts: Timestamp,
  ) -> Record {
    let input = &ctx.input;
    let subject = Subject::of(ctx);
    let (outcome, reason) = match verdict.map(|v| &v.decision) {
      None => (Outcome::Pass, None),
      Some(Decision::Deny { reason }) => (Outcome::Deny, Some(Reason::from(reason.as_str()))),
      Some(Decision::Ask { reason }) => (Outcome::Ask, Some(Reason::from(reason.as_str()))),
      Some(Decision::Warn { context }) => (Outcome::Warn, Some(Reason::from(context.as_str()))),
    };
    Record {
      v: SchemaVersion::CURRENT,
      ts,
      event: Event::PreToolUse,
      session_id: input.session_id.clone(),
      tool_use_id: Some(input.tool_use_id.clone()),
      agent_id: input.agent_id.clone(),
      cwd: input.cwd.clone(),
      tool: Some(input.tool.name()),
      subject,
      outcome,
      rule: verdict.map(|v| v.rule.clone()),
      pattern: verdict.and_then(|v| v.pattern.clone()),
      bindings: verdict
        .filter(|v| v.pattern.is_some())
        .map(|v| v.bindings.clone()),
      reason,
    }
  }

  /// The record for an event the guard only watches. The payload goes in
  /// with its shape intact and its long strings reduced to sizes, because
  /// the shape is what these records exist to capture.
  pub fn observed(
    envelope: &Envelope,
    ts: Timestamp,
  ) -> Record {
    Record {
      v: SchemaVersion::CURRENT,
      ts,
      event: envelope.event,
      session_id: envelope.session_id.clone(),
      tool_use_id: envelope.tool_use_id.clone(),
      agent_id: envelope.agent_id.clone(),
      cwd: envelope.cwd.clone(),
      tool: envelope.tool_name.clone(),
      subject: Subject::Raw(digest(envelope.payload.clone())),
      outcome: Outcome::Observed,
      rule: None,
      pattern: None,
      bindings: None,
      reason: None,
    }
  }
}

/// Strings longer than this in an observed payload are replaced by their
/// size. Below it a value is a path, a flag, or a short message and worth
/// keeping; above it, it is content, and content is what bloats the log
/// and carries secrets.
const DIGEST_THRESHOLD: usize = 256;

/// Replace every long string in `value` with `{"bytes": n, "lines": m}`,
/// recursively. Structure and short scalars survive, so the payload shape
/// stays readable. No sample of the text is kept, on purpose.
fn digest(value: serde_json::Value) -> serde_json::Value {
  use serde_json::Value;
  match value {
    Value::String(s) if s.len() > DIGEST_THRESHOLD => serde_json::json!({
      "bytes": s.len(),
      "lines": s.lines().count(),
    }),
    Value::Array(items) => Value::Array(items.into_iter().map(digest).collect()),
    Value::Object(fields) => {
      Value::Object(fields.into_iter().map(|(k, v)| (k, digest(v))).collect())
    }
    other => other,
  }
}

/// Appends records. A failure is reported, never swallowed, and never
/// changes a decision; that rule lives in the caller.
pub trait Writer {
  fn write(
    &mut self,
    record: &Record,
  ) -> Result<()>;
}

/// Reads back one session in file order. Escalation, in the next step, is
/// the first reader outside the tests.
#[allow(dead_code)]
pub trait Reader {
  fn session(
    &self,
    session_id: &SessionId,
  ) -> Result<Vec<Record>>;
}

/// The file-backed log: `<state dir>/sessions/<session id>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
  sessions: PathBuf,
}

impl Store {
  /// Overrides the state directory when set. Tests and ad hoc runs use it.
  pub const STATE_DIR_ENV: &str = "CLAUDE_GUARD_STATE_DIR";

  pub fn new(state_dir: impl Into<PathBuf>) -> Store {
    Store {
      sessions: state_dir.into().join("sessions"),
    }
  }

  /// `CLAUDE_GUARD_STATE_DIR`, else `$XDG_STATE_HOME/claude-guard`, else
  /// `$HOME/.local/state/claude-guard`.
  pub fn from_env() -> Result<Store> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let dir = resolve_state_dir(
      var(Store::STATE_DIR_ENV),
      var("XDG_STATE_HOME"),
      var("HOME"),
    )?;
    Ok(Store::new(dir))
  }

  pub fn session_file(
    &self,
    session_id: &SessionId,
  ) -> PathBuf {
    self.sessions.join(format!("{session_id}.jsonl"))
  }
}

fn resolve_state_dir(
  explicit: Option<OsString>,
  xdg_state_home: Option<OsString>,
  home: Option<OsString>,
) -> Result<PathBuf> {
  if let Some(dir) = explicit {
    return Ok(PathBuf::from(dir));
  }
  if let Some(xdg) = xdg_state_home {
    return Ok(PathBuf::from(xdg).join("claude-guard"));
  }
  if let Some(home) = home {
    return Ok(
      PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("claude-guard"),
    );
  }
  bail!("no state directory: CLAUDE_GUARD_STATE_DIR, XDG_STATE_HOME, and HOME are all unset")
}

impl Writer for Store {
  /// One `write_all` per record, so lines from parallel agents interleave
  /// whole and never split.
  fn write(
    &mut self,
    record: &Record,
  ) -> Result<()> {
    fs::create_dir_all(&self.sessions)
      .wrap_err_with(|| format!("create {}", self.sessions.display()))?;
    let path = self.session_file(&record.session_id);
    let mut line = serde_json::to_string(record).wrap_err("serialize log record")?;
    line.push('\n');
    let mut file = OpenOptions::new()
      .create(true)
      .append(true)
      .open(&path)
      .wrap_err_with(|| format!("open {}", path.display()))?;
    file
      .write_all(line.as_bytes())
      .wrap_err_with(|| format!("append to {}", path.display()))
  }
}

impl Reader for Store {
  /// A missing file is an empty session. A line that does not parse is
  /// skipped with a warning; one torn line never hides a session.
  fn session(
    &self,
    session_id: &SessionId,
  ) -> Result<Vec<Record>> {
    let path = self.session_file(session_id);
    let file = match fs::File::open(&path) {
      Ok(file) => file,
      Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(e) => return Err(e).wrap_err_with(|| format!("open {}", path.display())),
    };
    read_records(BufReader::new(file), &path)
  }
}

#[allow(dead_code)]
fn read_records(
  reader: impl BufRead,
  path: &Path,
) -> Result<Vec<Record>> {
  let mut records = Vec::new();
  for (index, line) in reader.lines().enumerate() {
    let line = line.wrap_err_with(|| format!("read {}", path.display()))?;
    if line.trim().is_empty() {
      continue;
    }
    match serde_json::from_str::<Record>(&line) {
      Ok(record) => records.push(record),
      Err(e) => tracing::warn!(
        file = %path.display(),
        line = index + 1,
        error = %e,
        "skipping a log line that does not parse"
      ),
    }
  }
  Ok(records)
}

/// The in-memory log for tests: same traits, no disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Memory {
  records: Vec<Record>,
}

#[allow(dead_code)]
impl Memory {
  pub fn records(&self) -> &[Record] {
    &self.records
  }
}

impl Writer for Memory {
  fn write(
    &mut self,
    record: &Record,
  ) -> Result<()> {
    self.records.push(record.clone());
    Ok(())
  }
}

impl Reader for Memory {
  fn session(
    &self,
    session_id: &SessionId,
  ) -> Result<Vec<Record>> {
    Ok(
      self
        .records
        .iter()
        .filter(|r| &r.session_id == session_id)
        .cloned()
        .collect(),
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::input::HookInput;
  use crate::rules::Ruleset;
  use crate::rules::testing::repo;

  fn at(rfc3339: &str) -> Timestamp {
    rfc3339.parse().unwrap()
  }

  fn context(
    session: &str,
    tool: Tool,
  ) -> Context {
    Context::new(
      HookInput {
        session_id: session.into(),
        cwd: "/Users/x/proj".into(),
        tool_use_id: "toolu_1".into(),
        agent_id: None,
        tool,
      },
      &Ruleset::builtin().declarations,
    )
  }

  fn bash(command: &str) -> Tool {
    Tool::Bash {
      command: command.into(),
    }
  }

  fn record(
    session: &str,
    tool: Tool,
  ) -> Record {
    let ctx = context(session, tool);
    let verdict = Ruleset::builtin().evaluate(&ctx, &repo(false));
    Record::pre_tool_use(&ctx, verdict.as_ref(), at("2026-09-05T10:00:00Z"))
  }

  // --- the record ---

  #[test]
  fn a_deny_record_is_one_line_with_every_key() {
    let json = serde_json::to_string(&record("s1", bash("git stash"))).unwrap();
    assert_eq!(
      json,
      concat!(
        r#"{"v":1,"ts":"2026-09-05T10:00:00Z","event":"pre_tool_use","session_id":"s1","#,
        r#""tool_use_id":"toolu_1","agent_id":null,"cwd":"/Users/x/proj","tool":"Bash","#,
        r#""subject":{"bash":{"command":"git stash","#,
        r#""commands":[{"words":[{"literal":"git"},{"literal":"stash"}],"redirects":[]}],"#,
        r#""elaborated":[{"parts":[{"name":{"literal":"git"}},{"arg":{"literal":"stash"}}],"#,
        r#""declared":false,"subcommand":[],"inner":null,"redirects":[]}],"#,
        r#""uninspected":[],"parse_error":null}},"#,
        r#""outcome":"deny","rule":"hard-denies","pattern":"[git -... stash ...]","bindings":{},"#,
        r#""reason":"claude-guard denied `git stash`: jj has no dirty tree, so there is nothing to stash. "#,
        r#"Instead: use `jj new` to park the current change or `jj describe` to name it."}"#,
      )
    );
  }

  #[test]
  fn a_pass_record_has_null_rule_pattern_and_reason() {
    let r = record("s1", bash("cargo build"));
    assert_eq!(r.outcome, Outcome::Pass);
    assert_eq!((r.rule, r.pattern, r.reason), (None, None, None));
    let json = serde_json::to_string(&record("s1", bash("cargo build"))).unwrap();
    assert!(
      json
        .ends_with(r#""outcome":"pass","rule":null,"pattern":null,"bindings":null,"reason":null}"#),
      "{json}"
    );
  }

  #[test]
  fn ask_and_warn_map_to_their_outcomes() {
    let r = record("s1", bash("git stash &&"));
    assert_eq!(r.outcome, Outcome::Ask);
    assert_eq!(r.rule, Some(RuleName::from("parse-error")));
    assert!(
      r.reason
        .unwrap()
        .as_ref()
        .starts_with("claude-guard could not parse")
    );

    let r = record("s1", bash("echo $(date)"));
    assert_eq!(r.outcome, Outcome::Warn);
    assert_eq!(r.rule, Some(RuleName::from("uninspected")));
  }

  #[test]
  fn every_tool_has_a_typed_subject() {
    let subject = |tool: Tool| serde_json::to_value(record("s1", tool).subject).unwrap();
    assert_eq!(
      subject(Tool::Write {
        path: "/tmp/x".into()
      }),
      serde_json::json!({"path": "/tmp/x"})
    );
    assert_eq!(
      subject(Tool::Read {
        path: "/etc/hosts".into()
      }),
      serde_json::json!({"path": "/etc/hosts"})
    );
    assert_eq!(
      subject(Tool::WebFetch {
        url: "https://example.com".into()
      }),
      serde_json::json!({"url": "https://example.com"})
    );
    assert_eq!(
      subject(Tool::Grep {
        pattern: "todo!".into(),
        path: Some("src".into())
      }),
      serde_json::json!({"search": {"pattern": "todo!", "path": "src"}})
    );
    assert_eq!(
      subject(Tool::Glob {
        pattern: "**/*.rs".into(),
        path: None
      }),
      serde_json::json!({"search": {"pattern": "**/*.rs", "path": null}})
    );
    assert_eq!(
      subject(Tool::Other {
        name: "Agent".into(),
        input: serde_json::json!({"prompt": "look", "subagent_type": "Explore"})
      }),
      serde_json::json!({"raw": {"prompt": "look", "subagent_type": "Explore"}})
    );
    assert_eq!(
      subject(Tool::Mcp {
        name: "mcp__github__create_pull_request".into(),
        server: "github".into(),
        tool: "create_pull_request".into(),
        input: serde_json::json!({"owner": "x", "repo": "y"})
      }),
      serde_json::json!({
        "mcp": {"server": "github", "tool": "create_pull_request", "input": {"owner": "x", "repo": "y"}}
      })
    );
  }

  #[test]
  fn an_observed_event_keeps_its_payload_whole() {
    let payload = serde_json::json!({
      "session_id": "s9",
      "cwd": "/Users/x",
      "hook_event_name": "PermissionDenied",
      "tool_name": "Bash",
      "tool_use_id": "toolu_9",
      "something_undocumented": {"we": "keep"}
    });
    let envelope = crate::input::envelope(&payload.to_string()).unwrap();
    let r = Record::observed(&envelope, at("2026-09-05T10:00:00Z"));
    assert_eq!(r.event, Event::PermissionDenied);
    assert_eq!(r.outcome, Outcome::Observed);
    assert_eq!(r.tool, Some(ToolName::from("Bash")));
    assert_eq!(r.tool_use_id, Some(ToolUseId::from("toolu_9")));
    assert_eq!(r.subject, Subject::Raw(payload));
    assert_eq!((&r.rule, &r.pattern, &r.reason), (&None, &None, &None));

    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains(r#""event":"permission_denied""#), "{json}");
    assert!(json.contains(r#""outcome":"observed""#), "{json}");
  }

  #[test]
  fn a_session_start_record_has_no_tool() {
    let payload = serde_json::json!({
      "session_id": "s9",
      "cwd": "/Users/x",
      "hook_event_name": "SessionStart",
      "source": "compact"
    });
    let envelope = crate::input::envelope(&payload.to_string()).unwrap();
    let r = Record::observed(&envelope, at("2026-09-05T10:00:00Z"));
    assert_eq!(r.event, Event::SessionStart);
    assert_eq!((&r.tool, &r.tool_use_id), (&None, &None));
    let json = serde_json::to_string(&r).unwrap();
    assert!(
      json.contains(r#""tool_use_id":null,"agent_id":null,"cwd":"/Users/x","tool":null"#),
      "{json}"
    );
  }

  #[test]
  fn a_compound_command_is_logged_segmented() {
    let r = record("s1", bash("ls && git stash > out 2>&1 | head $(nproc)"));
    let Subject::Bash {
      command,
      commands,
      elaborated,
      uninspected,
      parse_error,
    } = r.subject
    else {
      panic!("not a bash subject");
    };
    assert_eq!(command, "ls && git stash > out 2>&1 | head $(nproc)");
    assert_eq!(commands.len(), 3);
    assert_eq!(elaborated.len(), 3);
    assert_eq!(elaborated[1].flatten(), commands[1].words);
    assert_eq!(
      serde_json::to_value(&commands[1]).unwrap(),
      serde_json::json!({
        "words": [{"literal": "git"}, {"literal": "stash"}],
        "redirects": [{"kind": "write", "target": {"literal": "out"}}]
      })
    );
    assert_eq!(
      serde_json::to_value(&commands[2]).unwrap(),
      serde_json::json!({
        "words": [{"literal": "head"}, {"dynamic": "$(nproc)"}],
        "redirects": []
      })
    );
    assert_eq!(uninspected, vec!["nproc".to_string()]);
    assert_eq!(parse_error, None);
  }

  #[test]
  fn a_parse_failure_keeps_the_text_and_the_error() {
    let r = record("s1", bash("git stash &&"));
    assert_eq!(
      r.subject,
      Subject::Bash {
        command: "git stash &&".into(),
        commands: Vec::new(),
        elaborated: Vec::new(),
        uninspected: Vec::new(),
        parse_error: Some("syntax error at end of input".into()),
      }
    );
    assert_eq!(r.outcome, Outcome::Ask);
  }

  #[test]
  fn a_record_round_trips() {
    let r = record("s1", bash("git stash"));
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), r);
  }

  #[test]
  fn long_strings_in_observed_payloads_become_sizes() {
    let big = "x".repeat(300);
    let three_lines = format!("{big}\n{big}\n{big}");
    let payload = serde_json::json!({
      "session_id": "s9",
      "cwd": "/Users/x",
      "hook_event_name": "PostToolUse",
      "tool_name": "Bash",
      "tool_use_id": "toolu_9",
      "tool_input": {"command": "cat big", "content": big},
      "tool_response": {
        "stdout": three_lines,
        "stderr": "",
        "interrupted": false,
        "nested": [{"deep": big}, "short", 7]
      }
    });
    let envelope = crate::input::envelope(&payload.to_string()).unwrap();
    let r = Record::observed(&envelope, at("2026-09-05T10:00:00Z"));
    let Subject::Raw(raw) = r.subject else {
      panic!("not raw");
    };
    assert_eq!(raw["tool_input"]["command"], "cat big");
    assert_eq!(
      raw["tool_input"]["content"],
      serde_json::json!({"bytes": 300, "lines": 1})
    );
    assert_eq!(
      raw["tool_response"]["stdout"],
      serde_json::json!({"bytes": 902, "lines": 3})
    );
    assert_eq!(raw["tool_response"]["stderr"], "");
    assert_eq!(raw["tool_response"]["interrupted"], false);
    assert_eq!(
      raw["tool_response"]["nested"],
      serde_json::json!([{"deep": {"bytes": 300, "lines": 1}}, "short", 7])
    );
    assert_eq!(raw["tool_use_id"], "toolu_9");
  }

  #[test]
  fn a_string_at_the_threshold_is_kept_verbatim() {
    let exact = "y".repeat(DIGEST_THRESHOLD);
    let over = "y".repeat(DIGEST_THRESHOLD + 1);
    let digested = digest(serde_json::json!([exact, over]));
    assert_eq!(
      digested[0],
      serde_json::Value::String("y".repeat(DIGEST_THRESHOLD))
    );
    assert_eq!(
      digested[1],
      serde_json::json!({"bytes": DIGEST_THRESHOLD + 1, "lines": 1})
    );
  }

  // --- the store ---

  #[test]
  fn the_store_creates_the_directory_and_appends_one_line_per_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::new(dir.path());
    store.write(&record("s1", bash("git stash"))).unwrap();
    store.write(&record("s1", bash("cargo build"))).unwrap();
    store.write(&record("s2", bash("ls"))).unwrap();

    let s1 = fs::read_to_string(dir.path().join("sessions").join("s1.jsonl")).unwrap();
    assert_eq!(s1.lines().count(), 2);
    assert!(s1.ends_with('\n'));
    let s2 = fs::read_to_string(dir.path().join("sessions").join("s2.jsonl")).unwrap();
    assert_eq!(s2.lines().count(), 1);
  }

  #[test]
  fn the_store_reads_a_session_back_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::new(dir.path());
    let first = record("s1", bash("git stash"));
    let second = record("s1", bash("cargo build"));
    store.write(&first).unwrap();
    store.write(&second).unwrap();
    store.write(&record("s2", bash("ls"))).unwrap();

    assert_eq!(store.session(&"s1".into()).unwrap(), vec![first, second]);
  }

  #[test]
  fn a_missing_session_reads_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    assert_eq!(store.session(&"nope".into()).unwrap(), Vec::new());
  }

  #[test]
  fn torn_blank_and_future_lines_are_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::new(dir.path());
    let good = record("s1", bash("git stash"));
    store.write(&good).unwrap();

    let path = store.session_file(&"s1".into());
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    let future = serde_json::to_string(&good)
      .unwrap()
      .replacen("\"v\":1", "\"v\":2", 1);
    file
      .write_all(format!("{{\"v\":1,\"ts\":\"torn\n\n{future}\n").as_bytes())
      .unwrap();
    store.write(&good).unwrap();

    assert_eq!(
      store.session(&"s1".into()).unwrap(),
      vec![good.clone(), good]
    );
  }

  #[test]
  fn memory_and_store_agree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::new(dir.path());
    let mut memory = Memory::default();
    for r in [
      record("s1", bash("git stash")),
      record("s2", bash("ls")),
      record("s1", bash("cargo build")),
    ] {
      store.write(&r).unwrap();
      memory.write(&r).unwrap();
    }
    assert_eq!(
      store.session(&"s1".into()).unwrap(),
      memory.session(&"s1".into()).unwrap()
    );
    assert_eq!(memory.records().len(), 3);
  }

  // --- the state directory ---

  #[test]
  fn the_state_dir_prefers_the_override_then_xdg_then_home() {
    let some = |s: &str| Some(OsString::from(s));
    assert_eq!(
      resolve_state_dir(some("/o"), some("/x"), some("/h")).unwrap(),
      PathBuf::from("/o")
    );
    assert_eq!(
      resolve_state_dir(None, some("/x"), some("/h")).unwrap(),
      PathBuf::from("/x/claude-guard")
    );
    assert_eq!(
      resolve_state_dir(None, None, some("/h")).unwrap(),
      PathBuf::from("/h/.local/state/claude-guard")
    );
    assert!(resolve_state_dir(None, None, None).is_err());
  }

  #[test]
  fn the_session_file_is_named_by_session_id() {
    let store = Store::new("/state");
    assert_eq!(
      store.session_file(&"abc123".into()),
      PathBuf::from("/state/sessions/abc123.jsonl")
    );
  }
}
