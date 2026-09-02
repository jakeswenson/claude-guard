//! What the guard writes to stdout when a rule speaks.
//!
//! Two types on purpose. [`Decision`] is what a rule means. [`HookOutput`]
//! is the JSON Claude Code reads, mirrored field for field from the hooks
//! reference as of 2026-09-01. `From<Decision>` is the only bridge.

use serde::Serialize;

/// A rule's opinion about a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The call does not run. The model sees `reason`.
    Deny { reason: String },
    /// The user is prompted. `reason` appears in the permission dialog.
    Ask { reason: String },
    /// The call runs. The model sees `context` and may act on it.
    Warn { context: String },
}

impl Decision {
    /// The compact JSON line for stdout.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&HookOutput::from(self.clone()))
            .expect("hook output has no non-serializable fields")
    }
}

/// Top-level hook output envelope.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    hook_specific_output: PreToolUseOutput,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreToolUseOutput {
    hook_event_name: HookEventName,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission_decision: Option<PermissionDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission_decision_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_context: Option<String>,
}

#[derive(Debug, Serialize)]
enum HookEventName {
    PreToolUse,
}

/// Every value Claude Code accepts. `Allow` is part of the wire format but
/// the guard never emits it; see the design spec.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum PermissionDecision {
    Allow,
    Deny,
    Ask,
}

impl From<Decision> for HookOutput {
    fn from(decision: Decision) -> Self {
        let (permission_decision, permission_decision_reason, additional_context) = match decision
        {
            Decision::Deny { reason } => (Some(PermissionDecision::Deny), Some(reason), None),
            Decision::Ask { reason } => (Some(PermissionDecision::Ask), Some(reason), None),
            Decision::Warn { context } => (None, None, Some(context)),
        };
        Self {
            hook_specific_output: PreToolUseOutput {
                hook_event_name: HookEventName::PreToolUse,
                permission_decision,
                permission_decision_reason,
                additional_context,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_matches_the_wire_format() {
        let json = Decision::Deny {
            reason: "Use `jj log`.".into(),
        }
        .to_json();
        assert_eq!(
            json,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Use `jj log`."}}"#
        );
    }

    #[test]
    fn ask_matches_the_wire_format() {
        let json = Decision::Ask {
            reason: "Claude retried after a jj-repo deny: git log".into(),
        }
        .to_json();
        assert_eq!(
            json,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"Claude retried after a jj-repo deny: git log"}}"#
        );
    }

    #[test]
    fn warn_sends_context_and_no_decision() {
        let json = Decision::Warn {
            context: "Prefer rg over grep.".into(),
        }
        .to_json();
        assert_eq!(
            json,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"Prefer rg over grep."}}"#
        );
    }

    #[test]
    fn output_is_a_single_line() {
        let json = Decision::Deny {
            reason: "line one\nline two".into(),
        }
        .to_json();
        assert_eq!(json.lines().count(), 1);
    }
}
