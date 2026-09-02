//! The PreToolUse input Claude Code writes to the hook's stdin.
//!
//! Only the fields the guard uses are modeled. Everything else in the JSON
//! is ignored, so a new field from Claude Code never breaks parsing.
//! Field names verified against the hooks reference on 2026-09-01.

use std::path::PathBuf;

use color_eyre::eyre::{Result, WrapErr};
use serde::Deserialize;

/// One PreToolUse invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInput {
    pub session_id: String,
    pub cwd: PathBuf,
    pub tool_use_id: String,
    /// Present only when the hook fires inside a subagent.
    pub agent_id: Option<String>,
    pub tool: Tool,
}

/// The tool about to run, with the one field of its input the rules read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    Bash { command: String },
    Write { path: PathBuf },
    Edit { path: PathBuf },
    MultiEdit { path: PathBuf },
    /// Any tool the guard has no rules for. The name is kept for the log.
    Other { name: String },
}

impl Tool {
    /// The tool name as Claude Code spells it.
    pub fn name(&self) -> &str {
        match self {
            Self::Bash { .. } => "Bash",
            Self::Write { .. } => "Write",
            Self::Edit { .. } => "Edit",
            Self::MultiEdit { .. } => "MultiEdit",
            Self::Other { name } => name,
        }
    }
}

/// Parse the raw hook JSON.
pub fn parse(json: &str) -> Result<HookInput> {
    let raw: Raw = serde_json::from_str(json).wrap_err("parse PreToolUse hook input")?;
    let tool = match raw.tool_name.as_str() {
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
        _ => Tool::Other {
            name: raw.tool_name.clone(),
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
    session_id: String,
    cwd: PathBuf,
    tool_use_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    tool_name: String,
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

fn tool_input<T: for<'de> Deserialize<'de>>(raw: &Raw) -> Result<T> {
    serde_json::from_value(raw.tool_input.clone())
        .wrap_err_with(|| format!("parse tool_input for {}", raw.tool_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(tool_name: &str, tool_input: serde_json::Value) -> String {
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
        let input = parse(&envelope(
            "Bash",
            json!({"command": "git stash", "description": "stash", "timeout": 1000}),
        ))
        .unwrap();
        assert_eq!(input.session_id, "sess-1");
        assert_eq!(input.cwd, PathBuf::from("/Users/x/code/proj"));
        assert_eq!(input.tool_use_id, "toolu_01");
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
            ("Write", Tool::Write { path: "/tmp/x".into() }),
            ("Edit", Tool::Edit { path: "/tmp/x".into() }),
            ("MultiEdit", Tool::MultiEdit { path: "/tmp/x".into() }),
        ] {
            let input = parse(&envelope(
                name,
                json!({"file_path": "/tmp/x", "content": "hi"}),
            ))
            .unwrap();
            assert_eq!(input.tool, expected, "{name}");
            assert_eq!(input.tool.name(), name);
        }
    }

    #[test]
    fn unknown_tool_keeps_its_name() {
        let input = parse(&envelope("Read", json!({"file_path": "/etc/hosts"}))).unwrap();
        assert_eq!(
            input.tool,
            Tool::Other {
                name: "Read".into()
            }
        );
        assert_eq!(input.tool.name(), "Read");
    }

    #[test]
    fn agent_id_is_read_when_present() {
        let mut v: serde_json::Value =
            serde_json::from_str(&envelope("Bash", json!({"command": "ls"}))).unwrap();
        v["agent_id"] = json!("agent-7");
        v["agent_type"] = json!("Explore");
        let input = parse(&v.to_string()).unwrap();
        assert_eq!(input.agent_id.as_deref(), Some("agent-7"));
    }

    #[test]
    fn bash_without_command_is_an_error() {
        let err = parse(&envelope("Bash", json!({"description": "no command"}))).unwrap_err();
        assert!(err.to_string().contains("tool_input for Bash"), "{err}");
    }

    #[test]
    fn missing_session_id_is_an_error() {
        let mut v: serde_json::Value =
            serde_json::from_str(&envelope("Bash", json!({"command": "ls"}))).unwrap();
        v.as_object_mut().unwrap().remove("session_id");
        assert!(parse(&v.to_string()).is_err());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse("not json {{{").is_err());
    }
}
