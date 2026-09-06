//! The PreToolUse input Claude Code writes to the hook's stdin.
//!
//! Only the fields the guard uses are modeled. Everything else in the JSON
//! is ignored, so a new field from Claude Code never breaks parsing.
//! Field names verified against the hooks reference on 2026-09-01.
//!
//! The identifiers are newtypes. A `SessionId` names a log file and a
//! `WorkingDir` is where a jj repo is looked for, and neither should ever
//! be passed where the other is expected.

use std::fmt;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

/// A string newtype: transparent in JSON, opaque in code. Shared with the
/// other modules that mint identifiers.
macro_rules! string_id {
  ($(#[$doc:meta])* $name:ident) => {
    $(#[$doc])*
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
    #[serde(transparent)]
    pub struct $name(String);

    impl From<String> for $name {
      fn from(value: String) -> Self {
        Self(value)
      }
    }

    impl From<&str> for $name {
      fn from(value: &str) -> Self {
        Self(value.to_string())
      }
    }

    impl AsRef<str> for $name {
      fn as_ref(&self) -> &str {
        &self.0
      }
    }

    impl ::std::fmt::Display for $name {
      fn fmt(
        &self,
        f: &mut ::std::fmt::Formatter<'_>,
      ) -> ::std::fmt::Result {
        f.write_str(&self.0)
      }
    }
  };
}
pub(crate) use string_id;

string_id! {
  /// One Claude Code session. Also the name of that session's log file.
  SessionId
}

string_id! {
  /// One tool call within a session.
  ToolUseId
}

string_id! {
  /// The subagent making the call, when there is one.
  AgentId
}

string_id! {
  /// A tool name as Claude Code spells it: `Bash`, `Write`, `Read`, or
  /// `mcp__<server>__<tool>` for an MCP tool.
  ToolName
}

string_id! {
  /// The MCP server half of `mcp__<server>__<tool>`.
  McpServer
}

string_id! {
  /// The tool half of `mcp__<server>__<tool>`.
  McpTool
}

impl ToolName {
  /// Split `mcp__<server>__<tool>`. The server ends at the first `__`
  /// after the prefix, so a tool name may itself contain `__`. Anything
  /// else, including `mcp__` with no second separator, is not MCP.
  pub fn mcp(&self) -> Option<(McpServer, McpTool)> {
    let rest = self.0.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
      return None;
    }
    Some((McpServer::from(server), McpTool::from(tool)))
  }
}

/// The directory Claude Code reports for the call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkingDir(PathBuf);

impl From<PathBuf> for WorkingDir {
  fn from(value: PathBuf) -> Self {
    Self(value)
  }
}

impl From<&str> for WorkingDir {
  fn from(value: &str) -> Self {
    Self(PathBuf::from(value))
  }
}

impl AsRef<Path> for WorkingDir {
  fn as_ref(&self) -> &Path {
    &self.0
  }
}

impl fmt::Display for WorkingDir {
  fn fmt(
    &self,
    f: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    write!(f, "{}", self.0.display())
  }
}

/// Which hook event Claude Code is reporting, from `hook_event_name`.
/// Only PreToolUse gets a decision; the rest are recorded for study.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
  PreToolUse,
  PermissionRequest,
  PermissionDenied,
  PostToolUse,
  SessionStart,
  SessionEnd,
}

/// The fields every hook event shares, plus the payload whole. Parsed
/// first so the hook can dispatch on the event before it commits to a
/// shape for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
  pub event: Event,
  pub session_id: SessionId,
  pub cwd: WorkingDir,
  /// Present on tool events; absent on SessionStart.
  pub tool_use_id: Option<ToolUseId>,
  pub tool_name: Option<ToolName>,
  pub agent_id: Option<AgentId>,
  pub payload: serde_json::Value,
}

/// Parse the common fields. Fails on an event name the guard does not
/// know, which means a hook was registered for an event this version
/// does not handle.
pub fn envelope(json: &str) -> Result<Envelope> {
  let payload: serde_json::Value =
    serde_json::from_str(json).wrap_err("parse hook input as JSON")?;
  let common: Common =
    serde_json::from_value(payload.clone()).wrap_err("parse the hook event's common fields")?;
  Ok(Envelope {
    event: common.hook_event_name,
    session_id: common.session_id,
    cwd: common.cwd,
    tool_use_id: common.tool_use_id,
    tool_name: common.tool_name,
    agent_id: common.agent_id,
    payload,
  })
}

/// The wire shape shared by every event. Claude Code's PascalCase event
/// names are mapped onto [`Event`] here.
#[derive(Deserialize)]
struct Common {
  #[serde(deserialize_with = "event_from_wire")]
  hook_event_name: Event,
  session_id: SessionId,
  cwd: WorkingDir,
  #[serde(default)]
  tool_use_id: Option<ToolUseId>,
  #[serde(default)]
  tool_name: Option<ToolName>,
  #[serde(default)]
  agent_id: Option<AgentId>,
}

fn event_from_wire<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Event, D::Error> {
  let name = String::deserialize(d)?;
  match name.as_str() {
    "PreToolUse" => Ok(Event::PreToolUse),
    "PermissionRequest" => Ok(Event::PermissionRequest),
    "PermissionDenied" => Ok(Event::PermissionDenied),
    "PostToolUse" => Ok(Event::PostToolUse),
    "SessionStart" => Ok(Event::SessionStart),
    "SessionEnd" => Ok(Event::SessionEnd),
    other => Err(serde::de::Error::custom(format!(
      "unknown hook event {other:?}"
    ))),
  }
}

/// One PreToolUse invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInput {
  pub session_id: SessionId,
  pub cwd: WorkingDir,
  pub tool_use_id: ToolUseId,
  /// Present only when the hook fires inside a subagent.
  pub agent_id: Option<AgentId>,
  pub tool: Tool,
}

/// The tool about to run, with the part of its input the guard keeps: what
/// the rules read, and what the log records as the subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
  Bash {
    command: String,
  },
  Write {
    path: PathBuf,
  },
  Edit {
    path: PathBuf,
  },
  MultiEdit {
    path: PathBuf,
  },
  Read {
    path: PathBuf,
  },
  WebFetch {
    url: String,
  },
  Glob {
    pattern: String,
    path: Option<PathBuf>,
  },
  Grep {
    pattern: String,
    path: Option<PathBuf>,
  },
  /// An MCP tool. Inputs are per server, so the whole input is kept.
  Mcp {
    name: ToolName,
    server: McpServer,
    tool: McpTool,
    input: serde_json::Value,
  },
  /// Any tool the guard has not modeled. The whole input is kept so the
  /// log record still explains itself.
  Other {
    name: ToolName,
    input: serde_json::Value,
  },
}

impl Tool {
  /// The tool name as Claude Code spells it.
  pub fn name(&self) -> ToolName {
    match self {
      Self::Bash { .. } => ToolName::from("Bash"),
      Self::Write { .. } => ToolName::from("Write"),
      Self::Edit { .. } => ToolName::from("Edit"),
      Self::MultiEdit { .. } => ToolName::from("MultiEdit"),
      Self::Read { .. } => ToolName::from("Read"),
      Self::WebFetch { .. } => ToolName::from("WebFetch"),
      Self::Glob { .. } => ToolName::from("Glob"),
      Self::Grep { .. } => ToolName::from("Grep"),
      Self::Mcp { name, .. } | Self::Other { name, .. } => name.clone(),
    }
  }
}

/// Parse the raw hook JSON.
pub fn parse(json: &str) -> Result<HookInput> {
  let raw: Raw = serde_json::from_str(json).wrap_err("parse PreToolUse hook input")?;
  let tool = match raw.tool_name.as_ref() {
    "Bash" => {
      let BashInput { command } = tool_input(&raw)?;
      Tool::Bash { command }
    }
    "Write" => Tool::Write {
      path: tool_input::<FileInput>(&raw)?.file_path,
    },
    "Edit" => Tool::Edit {
      path: tool_input::<FileInput>(&raw)?.file_path,
    },
    "MultiEdit" => Tool::MultiEdit {
      path: tool_input::<FileInput>(&raw)?.file_path,
    },
    "Read" => Tool::Read {
      path: tool_input::<FileInput>(&raw)?.file_path,
    },
    "WebFetch" => Tool::WebFetch {
      url: tool_input::<UrlInput>(&raw)?.url,
    },
    "Glob" => {
      let SearchInput { pattern, path } = tool_input(&raw)?;
      Tool::Glob { pattern, path }
    }
    "Grep" => {
      let SearchInput { pattern, path } = tool_input(&raw)?;
      Tool::Grep { pattern, path }
    }
    _ => match raw.tool_name.mcp() {
      Some((server, tool)) => Tool::Mcp {
        name: raw.tool_name.clone(),
        server,
        tool,
        input: raw.tool_input.clone(),
      },
      None => Tool::Other {
        name: raw.tool_name.clone(),
        input: raw.tool_input.clone(),
      },
    },
  };
  Ok(HookInput {
    session_id: raw.session_id,
    cwd: raw.cwd,
    tool_use_id: raw.tool_use_id,
    agent_id: raw.agent_id,
    tool,
  })
}

/// The wire shape. `tool_input` stays untyped here because its shape
/// depends on `tool_name`, and that dispatch happens in [`parse`].
#[derive(Deserialize)]
struct Raw {
  session_id: SessionId,
  cwd: WorkingDir,
  tool_use_id: ToolUseId,
  #[serde(default)]
  agent_id: Option<AgentId>,
  tool_name: ToolName,
  tool_input: serde_json::Value,
}

#[derive(Deserialize)]
struct BashInput {
  command: String,
}

#[derive(Deserialize)]
struct FileInput {
  file_path: PathBuf,
}

#[derive(Deserialize)]
struct UrlInput {
  url: String,
}

/// Glob and Grep share this shape: a pattern and an optional directory.
#[derive(Deserialize)]
struct SearchInput {
  pattern: String,
  #[serde(default)]
  path: Option<PathBuf>,
}

fn tool_input<T: for<'de> Deserialize<'de>>(raw: &Raw) -> Result<T> {
  serde_json::from_value(raw.tool_input.clone())
    .wrap_err_with(|| format!("parse tool_input for {}", raw.tool_name))
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn envelope_json(
    tool_name: &str,
    tool_input: serde_json::Value,
  ) -> String {
    json!({
        "session_id": "sess-1",
        "prompt_id": "550e8400-e29b-41d4-a716-446655440000",
        "transcript_path": "/Users/x/.claude/projects/p/t.jsonl",
        "cwd": "/Users/x/code/proj",
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input,
        "tool_use_id": "toolu_01"
    })
    .to_string()
  }

  #[test]
  fn bash_carries_the_command_and_ignores_extra_fields() {
    let input = parse(&envelope_json(
      "Bash",
      json!({"command": "git stash", "description": "stash", "timeout": 1000}),
    ))
    .unwrap();
    assert_eq!(input.session_id, SessionId::from("sess-1"));
    assert_eq!(input.cwd, WorkingDir::from("/Users/x/code/proj"));
    assert_eq!(input.tool_use_id, ToolUseId::from("toolu_01"));
    assert_eq!(input.agent_id, None);
    assert_eq!(
      input.tool,
      Tool::Bash {
        command: "git stash".into()
      }
    );
  }

  #[test]
  fn file_tools_carry_the_path() {
    for (name, expected) in [
      (
        "Write",
        Tool::Write {
          path: "/tmp/x".into(),
        },
      ),
      (
        "Edit",
        Tool::Edit {
          path: "/tmp/x".into(),
        },
      ),
      (
        "MultiEdit",
        Tool::MultiEdit {
          path: "/tmp/x".into(),
        },
      ),
    ] {
      let input = parse(&envelope_json(
        name,
        json!({"file_path": "/tmp/x", "content": "hi"}),
      ))
      .unwrap();
      assert_eq!(input.tool, expected, "{name}");
      assert_eq!(input.tool.name(), ToolName::from(name));
    }
  }

  #[test]
  fn read_fetch_glob_and_grep_keep_their_subject() {
    let input = parse(&envelope_json(
      "Read",
      json!({"file_path": "/etc/hosts", "limit": 5}),
    ))
    .unwrap();
    assert_eq!(
      input.tool,
      Tool::Read {
        path: "/etc/hosts".into()
      }
    );

    let input = parse(&envelope_json(
      "WebFetch",
      json!({"url": "https://example.com/x", "prompt": "summarize"}),
    ))
    .unwrap();
    assert_eq!(
      input.tool,
      Tool::WebFetch {
        url: "https://example.com/x".into()
      }
    );

    let input = parse(&envelope_json("Glob", json!({"pattern": "**/*.rs"}))).unwrap();
    assert_eq!(
      input.tool,
      Tool::Glob {
        pattern: "**/*.rs".into(),
        path: None
      }
    );

    let input = parse(&envelope_json(
      "Grep",
      json!({"pattern": "todo!", "path": "src", "output_mode": "content"}),
    ))
    .unwrap();
    assert_eq!(
      input.tool,
      Tool::Grep {
        pattern: "todo!".into(),
        path: Some("src".into())
      }
    );
    assert_eq!(input.tool.name(), ToolName::from("Grep"));
  }

  #[test]
  fn mcp_tools_split_into_server_and_tool() {
    let input = parse(&envelope_json(
      "mcp__github__create_pull_request",
      json!({"owner": "x", "repo": "y", "title": "t"}),
    ))
    .unwrap();
    assert_eq!(
      input.tool,
      Tool::Mcp {
        name: ToolName::from("mcp__github__create_pull_request"),
        server: McpServer::from("github"),
        tool: McpTool::from("create_pull_request"),
        input: json!({"owner": "x", "repo": "y", "title": "t"}),
      }
    );
    assert_eq!(
      input.tool.name(),
      ToolName::from("mcp__github__create_pull_request")
    );
  }

  #[test]
  fn mcp_names_split_at_the_first_separator_after_the_prefix() {
    let split = |name: &str| ToolName::from(name).mcp();
    assert_eq!(
      split("mcp__claude_ai_Gmail__complete__auth"),
      Some((
        McpServer::from("claude_ai_Gmail"),
        McpTool::from("complete__auth")
      ))
    );
    assert_eq!(split("mcp__only"), None);
    assert_eq!(split("mcp____tool"), None);
    assert_eq!(split("mcp__server__"), None);
    assert_eq!(split("Bash"), None);
  }

  #[test]
  fn unknown_tool_keeps_its_name_and_whole_input() {
    let input = parse(&envelope_json(
      "Agent",
      json!({"prompt": "look around", "model": "haiku"}),
    ))
    .unwrap();
    assert_eq!(
      input.tool,
      Tool::Other {
        name: ToolName::from("Agent"),
        input: json!({"prompt": "look around", "model": "haiku"}),
      }
    );
    assert_eq!(input.tool.name().to_string(), "Agent");
  }

  #[test]
  fn agent_id_is_read_when_present() {
    let mut v: serde_json::Value =
      serde_json::from_str(&envelope_json("Bash", json!({"command": "ls"}))).unwrap();
    v["agent_id"] = json!("agent-7");
    v["agent_type"] = json!("Explore");
    let input = parse(&v.to_string()).unwrap();
    assert_eq!(input.agent_id, Some(AgentId::from("agent-7")));
  }

  #[test]
  fn ids_are_transparent_in_json() {
    let id: SessionId = serde_json::from_str("\"abc\"").unwrap();
    assert_eq!(id, SessionId::from("abc"));
    assert_eq!(serde_json::to_string(&id).unwrap(), "\"abc\"");
    let dir: WorkingDir = serde_json::from_str("\"/x/y\"").unwrap();
    assert_eq!(serde_json::to_string(&dir).unwrap(), "\"/x/y\"");
  }

  #[test]
  fn the_envelope_carries_the_event_and_the_whole_payload() {
    let env = envelope(&envelope_json("Bash", json!({"command": "ls"}))).unwrap();
    assert_eq!(env.event, Event::PreToolUse);
    assert_eq!(env.session_id, SessionId::from("sess-1"));
    assert_eq!(env.tool_use_id, Some(ToolUseId::from("toolu_01")));
    assert_eq!(env.tool_name, Some(ToolName::from("Bash")));
    assert_eq!(env.payload["tool_input"]["command"], "ls");

    let session_start = json!({
      "session_id": "sess-2",
      "cwd": "/Users/x",
      "hook_event_name": "SessionStart",
      "source": "startup"
    })
    .to_string();
    let env = envelope(&session_start).unwrap();
    assert_eq!(env.event, Event::SessionStart);
    assert_eq!(env.tool_use_id, None);
    assert_eq!(env.tool_name, None);
    assert_eq!(env.payload["source"], "startup");
  }

  #[test]
  fn every_documented_event_name_maps() {
    for (wire, event) in [
      ("PreToolUse", Event::PreToolUse),
      ("PermissionRequest", Event::PermissionRequest),
      ("PermissionDenied", Event::PermissionDenied),
      ("PostToolUse", Event::PostToolUse),
      ("SessionStart", Event::SessionStart),
      ("SessionEnd", Event::SessionEnd),
    ] {
      let json = json!({"session_id": "s", "cwd": "/", "hook_event_name": wire}).to_string();
      assert_eq!(envelope(&json).unwrap().event, event, "{wire}");
    }
    let json = json!({"session_id": "s", "cwd": "/", "hook_event_name": "Stop"}).to_string();
    let err = envelope(&json).unwrap_err();
    assert!(err.to_string().contains("common fields"), "{err}");
  }

  #[test]
  fn bash_without_command_is_an_error() {
    let err = parse(&envelope_json("Bash", json!({"description": "no command"}))).unwrap_err();
    assert!(err.to_string().contains("tool_input for Bash"), "{err}");
  }

  #[test]
  fn missing_session_id_is_an_error() {
    let mut v: serde_json::Value =
      serde_json::from_str(&envelope_json("Bash", json!({"command": "ls"}))).unwrap();
    v.as_object_mut().unwrap().remove("session_id");
    assert!(parse(&v.to_string()).is_err());
  }

  #[test]
  fn garbage_is_an_error() {
    assert!(parse("not json {{{").is_err());
  }
}
