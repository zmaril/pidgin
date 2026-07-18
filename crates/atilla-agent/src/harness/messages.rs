// straitjacket-allow-file:duplication — the ported `bash_execution_to_text` and
// `convert_to_llm` test bodies mirror pi's parallel cases and build near-identical
// message literals by design; the clone detector reads these as duplicates.
//! Synthesized harness messages and LLM conversion, mirroring
//! `packages/agent/src/harness/messages.ts`.
//!
//! The `create*Message` constructors already live in
//! [`crate::harness::session::messages`] (a prior wave ported the subset
//! `buildSessionContext` needs); they are re-exported here so `harness::messages`
//! exposes the whole `messages.ts` surface without duplicating them.
//!
//! `AgentMessage` is this crate's opaque [`Value`] representation, matching how
//! `convertToLlm` inspects messages by `role` at runtime.

use serde_json::{json, Value};

use crate::harness::types::AgentMessage;
use atilla_ai::Message;

pub use crate::harness::session::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};

/// Prefix wrapping a compaction summary in the synthesized user message.
pub const COMPACTION_SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

/// Suffix closing a compaction summary.
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

/// Prefix wrapping a branch summary in the synthesized user message.
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";

/// Suffix closing a branch summary.
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// Render a `bashExecution` message to its model-visible text. Mirrors pi's
/// `bashExecutionToText`. Reads fields off the opaque message value.
pub fn bash_execution_to_text(msg: &Value) -> String {
    let command = msg.get("command").and_then(Value::as_str).unwrap_or("");
    let output = msg.get("output").and_then(Value::as_str).unwrap_or("");

    let mut text = format!("Ran `{command}`\n");
    if !output.is_empty() {
        text += &format!("```\n{output}\n```");
    } else {
        text += "(no output)";
    }

    let cancelled = msg
        .get("cancelled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if cancelled {
        text += "\n\n(command cancelled)";
    } else {
        // pi: exitCode !== null && exitCode !== undefined && exitCode !== 0
        let exit_code = msg.get("exitCode");
        if let Some(code) = exit_code {
            if !code.is_null() && code.as_i64() != Some(0) {
                let rendered = code
                    .as_i64()
                    .map_or_else(|| code.to_string(), |value| value.to_string());
                text += &format!("\n\nCommand exited with code {rendered}");
            }
        }
    }

    let truncated = msg
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if truncated {
        if let Some(path) = msg.get("fullOutputPath").and_then(Value::as_str) {
            text += &format!("\n\n[Output truncated. Full output: {path}]");
        }
    }

    text
}

/// Convert harness [`AgentMessage`]s to LLM [`Message`]s. Mirrors pi's
/// `convertToLlm`: `bashExecution`/`custom`/`branchSummary`/`compactionSummary`
/// messages are rewritten to `user` messages, `user`/`assistant`/`toolResult`
/// pass through, and everything else (including a context-excluded
/// `bashExecution`) is dropped.
///
/// Messages are assembled as [`Value`] literals mirroring pi's object shapes,
/// then parsed into the typed [`Message`]; a message that fails to parse is
/// dropped, matching this crate's `default_convert_to_llm`.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(convert_one)
        .filter_map(|value| serde_json::from_value::<Message>(value).ok())
        .collect()
}

/// Rewrite a single message to its LLM [`Value`] shape, or `None` to drop it.
fn convert_one(msg: &AgentMessage) -> Option<Value> {
    let role = msg.get("role").and_then(Value::as_str)?;
    let timestamp = msg.get("timestamp").cloned().unwrap_or(Value::Null);
    match role {
        "bashExecution" => {
            if msg.get("excludeFromContext").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            Some(json!({
                "role": "user",
                "content": [{ "type": "text", "text": bash_execution_to_text(msg) }],
                "timestamp": timestamp,
            }))
        }
        "custom" => {
            let content = match msg.get("content") {
                Some(Value::String(text)) => json!([{ "type": "text", "text": text }]),
                Some(other) => other.clone(),
                None => json!([]),
            };
            Some(json!({
                "role": "user",
                "content": content,
                "timestamp": timestamp,
            }))
        }
        "branchSummary" => {
            let summary = msg.get("summary").and_then(Value::as_str).unwrap_or("");
            let text = format!("{BRANCH_SUMMARY_PREFIX}{summary}{BRANCH_SUMMARY_SUFFIX}");
            Some(json!({
                "role": "user",
                "content": [{ "type": "text", "text": text }],
                "timestamp": timestamp,
            }))
        }
        "compactionSummary" => {
            let summary = msg.get("summary").and_then(Value::as_str).unwrap_or("");
            let text = format!("{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}");
            Some(json!({
                "role": "user",
                "content": [{ "type": "text", "text": text }],
                "timestamp": timestamp,
            }))
        }
        "user" | "assistant" | "toolResult" => Some(msg.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atilla_ai::types::{Message, UserContent};

    fn bash_message(overrides: Value) -> Value {
        let mut base = json!({
            "role": "bashExecution",
            "command": "ls",
            "output": "a\nb",
            "exitCode": 0,
            "cancelled": false,
            "truncated": false,
            "timestamp": 1000,
        });
        if let (Value::Object(base_map), Value::Object(over)) = (&mut base, overrides) {
            for (key, value) in over {
                base_map.insert(key, value);
            }
        }
        base
    }

    #[test]
    fn bash_execution_to_text_renders_command_and_output() {
        let text = bash_execution_to_text(&bash_message(json!({})));
        assert_eq!(text, "Ran `ls`\n```\na\nb\n```");
    }

    #[test]
    fn bash_execution_to_text_handles_no_output() {
        let text = bash_execution_to_text(&bash_message(json!({ "output": "" })));
        assert_eq!(text, "Ran `ls`\n(no output)");
    }

    #[test]
    fn bash_execution_to_text_reports_nonzero_exit_code() {
        let text = bash_execution_to_text(&bash_message(json!({ "exitCode": 2 })));
        assert_eq!(
            text,
            "Ran `ls`\n```\na\nb\n```\n\nCommand exited with code 2"
        );
    }

    #[test]
    fn bash_execution_to_text_prefers_cancelled_over_exit_code() {
        let text =
            bash_execution_to_text(&bash_message(json!({ "cancelled": true, "exitCode": 2 })));
        assert_eq!(text, "Ran `ls`\n```\na\nb\n```\n\n(command cancelled)");
    }

    #[test]
    fn bash_execution_to_text_appends_full_output_path_when_truncated() {
        let text = bash_execution_to_text(&bash_message(json!({
            "truncated": true,
            "fullOutputPath": "/tmp/bash-1.log",
        })));
        assert_eq!(
            text,
            "Ran `ls`\n```\na\nb\n```\n\n[Output truncated. Full output: /tmp/bash-1.log]"
        );
    }

    #[test]
    fn convert_to_llm_rewrites_bash_execution_to_user_message() {
        let messages = vec![bash_message(json!({ "output": "hi" }))];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 1);
        match &converted[0] {
            Message::User(user) => match &user.content {
                UserContent::Blocks(blocks) => assert_eq!(blocks.len(), 1),
                UserContent::Text(_) => panic!("expected block content"),
            },
            other => panic!("expected user message, got {other:?}"),
        }
    }

    #[test]
    fn convert_to_llm_drops_context_excluded_bash_execution() {
        let messages = vec![bash_message(json!({ "excludeFromContext": true }))];
        assert!(convert_to_llm(&messages).is_empty());
    }

    #[test]
    fn convert_to_llm_wraps_branch_and_compaction_summaries() {
        let messages = vec![
            json!({ "role": "branchSummary", "summary": "branch work", "fromId": "x", "timestamp": 1 }),
            json!({ "role": "compactionSummary", "summary": "older turns", "tokensBefore": 5, "timestamp": 2 }),
        ];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 2);
        let texts: Vec<String> = converted
            .iter()
            .map(|message| {
                serde_json::to_value(message).unwrap()["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(texts[0].starts_with(BRANCH_SUMMARY_PREFIX));
        assert!(texts[0].ends_with(BRANCH_SUMMARY_SUFFIX));
        assert!(texts[1].starts_with(COMPACTION_SUMMARY_PREFIX));
        assert!(texts[1].ends_with(COMPACTION_SUMMARY_SUFFIX));
    }

    #[test]
    fn convert_to_llm_passes_through_core_roles_and_drops_unknown() {
        let messages = vec![
            json!({ "role": "user", "content": "hi", "timestamp": 1 }),
            json!({ "role": "somethingElse", "timestamp": 2 }),
        ];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 1);
        assert!(matches!(&converted[0], Message::User(_)));
    }

    #[test]
    fn convert_to_llm_string_custom_content_becomes_text_block() {
        let messages = vec![json!({
            "role": "custom",
            "customType": "note",
            "content": "just text",
            "display": true,
            "timestamp": 7,
        })];
        let converted = convert_to_llm(&messages);
        assert_eq!(converted.len(), 1);
        let value = serde_json::to_value(&converted[0]).unwrap();
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][0]["text"], "just text");
    }
}
