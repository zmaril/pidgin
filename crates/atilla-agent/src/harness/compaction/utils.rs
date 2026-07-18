//! File-operation accumulation and conversation serialization, mirroring
//! `packages/agent/src/harness/compaction/utils.ts`.
//!
//! [`AgentMessage`](crate::harness::types::AgentMessage) is opaque JSON in this
//! port, so the message-shape reads here (`role`, `content`, tool-call blocks)
//! go through [`serde_json::Value`] accessors rather than typed fields, matching
//! how the rest of the harness handles agent messages.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::harness::types::AgentMessage;

/// File paths touched by a session branch or compaction range. Mirrors pi's
/// `FileOperations`. pi uses `Set<string>` (insertion order); this port uses
/// [`BTreeSet`] since [`compute_file_lists`] sorts the output anyway, so the
/// serialized result is identical.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    /// Files read but not necessarily modified.
    pub read: BTreeSet<String>,
    /// Files written by full-file write operations.
    pub written: BTreeSet<String>,
    /// Files modified by edit operations.
    pub edited: BTreeSet<String>,
}

/// Create an empty file-operation accumulator. Mirrors pi's `createFileOps`.
pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// Add file operations from assistant tool calls to an accumulator. Mirrors
/// pi's `extractFileOpsFromMessage`.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return;
    };

    for block in content {
        let Some(block) = block.as_object() else {
            continue;
        };
        if block.get("type").and_then(Value::as_str) != Some("toolCall") {
            continue;
        }
        if !block.contains_key("arguments") || !block.contains_key("name") {
            continue;
        }
        let Some(args) = block.get("arguments").and_then(Value::as_object) else {
            continue;
        };
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            continue;
        };
        match block.get("name").and_then(Value::as_str) {
            Some("read") => {
                file_ops.read.insert(path.to_string());
            }
            Some("write") => {
                file_ops.written.insert(path.to_string());
            }
            Some("edit") => {
                file_ops.edited.insert(path.to_string());
            }
            _ => {}
        }
    }
}

/// Compute sorted read-only and modified file lists from accumulated
/// operations. Mirrors pi's `computeFileLists`.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let mut modified: BTreeSet<String> = BTreeSet::new();
    modified.extend(file_ops.edited.iter().cloned());
    modified.extend(file_ops.written.iter().cloned());

    let read_files: Vec<String> = file_ops
        .read
        .iter()
        .filter(|f| !modified.contains(*f))
        .cloned()
        .collect();
    let modified_files: Vec<String> = modified.into_iter().collect();
    (read_files, modified_files)
}

/// Format file lists as summary metadata tags. Mirrors pi's
/// `formatFileOperations`.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

/// pi's `TOOL_RESULT_MAX_CHARS` (`utils.ts`): the tool-result truncation cap.
pub const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Serialize a JSON value like `JSON.stringify`, falling back to
/// `[unserializable]`. Mirrors pi's `safeJsonStringify` (utils.ts). For a
/// [`serde_json::Value`] serialization cannot fail (map keys are always
/// strings, numbers are always finite), so the fallback is defensive parity.
pub fn safe_json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".to_string())
}

/// Number of UTF-16 code units in `text`, mirroring JavaScript's
/// `String.prototype.length`. pi's char math (`slice`, `length`) is UTF-16, so
/// this port measures the same units for byte-faithful truncation and token
/// estimation.
pub fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `end_units` UTF-16 code units of `text` as a `String`, mirroring
/// JavaScript's `String.prototype.slice(0, end)`.
fn js_slice_to(text: &str, end_units: usize) -> String {
    if js_len(text) <= end_units {
        return text.to_string();
    }
    let units: Vec<u16> = text.encode_utf16().take(end_units).collect();
    String::from_utf16_lossy(&units)
}

/// Truncate `text` to `max_chars` UTF-16 code units with a truncation notice.
/// Mirrors pi's `truncateForSummary`.
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let len = js_len(text);
    if len <= max_chars {
        return text.to_string();
    }
    let truncated_chars = len - max_chars;
    format!(
        "{}\n\n[... {truncated_chars} more characters truncated]",
        js_slice_to(text, max_chars)
    )
}

/// Concatenate the `text` blocks of a content list (string or block array).
/// Mirrors the repeated `content.filter(text).map(text).join("")` pattern.
fn join_text_content(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                out.push_str(text);
            }
        }
    }
    out
}

/// Serialize LLM messages to plain text for summarization prompts. Mirrors pi's
/// `serializeConversation`. pi takes `Message[]` (the output of `convertToLlm`,
/// which only produces `user`/`assistant`/`toolResult` roles); this port reads
/// the same opaque [`AgentMessage`] shape.
pub fn serialize_conversation(messages: &[AgentMessage]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages {
        match msg.get("role").and_then(Value::as_str) {
            Some("user") => {
                let content = match msg.get("content") {
                    Some(c) => join_text_content(c),
                    None => String::new(),
                };
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Some("assistant") => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut thinking_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();

                if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) = block.get("text").and_then(Value::as_str) {
                                    text_parts.push(t.to_string());
                                }
                            }
                            Some("thinking") => {
                                if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                                    thinking_parts.push(t.to_string());
                                }
                            }
                            Some("toolCall") => {
                                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                                let args_str =
                                    match block.get("arguments").and_then(Value::as_object) {
                                        Some(args) => args
                                            .iter()
                                            .map(|(k, v)| format!("{k}={}", safe_json_stringify(v)))
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                        None => String::new(),
                                    };
                                tool_calls.push(format!("{name}({args_str})"));
                            }
                            _ => {}
                        }
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!(
                        "[Assistant thinking]: {}",
                        thinking_parts.join("\n")
                    ));
                }
                if !text_parts.is_empty() {
                    parts.push(format!("[Assistant]: {}", text_parts.join("\n")));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Some("toolResult") => {
                let content = match msg.get("content") {
                    Some(c) => join_text_content(c),
                    None => String::new(),
                };
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}

// ---------------------------------------------------------------------------
// convertToLlm and its constants (`packages/agent/src/harness/messages.ts`).
//
// Not part of the compaction public surface, but the summarization paths depend
// on it and it is not yet ported elsewhere in atilla, so it lives here as a
// crate-internal helper. It mirrors pi's `convertToLlm`, `bashExecutionToText`,
// and the summary prefix/suffix constants byte-for-byte.
// ---------------------------------------------------------------------------

/// pi's `COMPACTION_SUMMARY_PREFIX` (messages.ts).
pub(crate) const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
/// pi's `COMPACTION_SUMMARY_SUFFIX` (messages.ts).
pub(crate) const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
/// pi's `BRANCH_SUMMARY_PREFIX` (messages.ts).
pub(crate) const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
/// pi's `BRANCH_SUMMARY_SUFFIX` (messages.ts).
pub(crate) const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// pi's `bashExecutionToText` (messages.ts).
fn bash_execution_to_text(msg: &Value) -> String {
    let command = msg.get("command").and_then(Value::as_str).unwrap_or("");
    let output = msg.get("output").and_then(Value::as_str).unwrap_or("");
    let mut text = format!("Ran `{command}`\n");
    if !output.is_empty() {
        text.push_str(&format!("```\n{output}\n```"));
    } else {
        text.push_str("(no output)");
    }
    let cancelled = msg
        .get("cancelled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if cancelled {
        text.push_str("\n\n(command cancelled)");
    } else {
        // pi: exitCode !== null && exitCode !== undefined && exitCode !== 0
        if let Some(exit_code) = msg.get("exitCode").and_then(Value::as_i64) {
            if exit_code != 0 {
                text.push_str(&format!("\n\nCommand exited with code {exit_code}"));
            }
        }
    }
    let truncated = msg
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if truncated {
        if let Some(full_output_path) = msg.get("fullOutputPath").and_then(Value::as_str) {
            text.push_str(&format!(
                "\n\n[Output truncated. Full output: {full_output_path}]"
            ));
        }
    }
    text
}

/// Build an LLM `user` message with a single text block, preserving the source
/// message's `timestamp`. Mirrors the shape `convertToLlm` emits.
fn text_user_message(text: String, timestamp: &Value) -> Value {
    serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
        "timestamp": timestamp.clone(),
    })
}

/// pi's `convertToLlm` (messages.ts): project agent messages to the LLM message
/// shape summarization consumes. `bashExecution`, `custom`, `branchSummary`, and
/// `compactionSummary` are rewritten to `user` text messages;
/// `user`/`assistant`/`toolResult` pass through; anything else is dropped.
pub(crate) fn convert_to_llm(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    let mut out: Vec<AgentMessage> = Vec::new();
    for m in messages {
        let ts = m.get("timestamp").cloned().unwrap_or(Value::Null);
        match m.get("role").and_then(Value::as_str) {
            Some("bashExecution") => {
                if m.get("excludeFromContext")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    continue;
                }
                out.push(text_user_message(bash_execution_to_text(m), &ts));
            }
            Some("custom") => {
                let content = m.get("content");
                let blocks = match content {
                    Some(Value::String(s)) => {
                        serde_json::json!([{ "type": "text", "text": s }])
                    }
                    Some(other) => other.clone(),
                    None => serde_json::json!([]),
                };
                out.push(serde_json::json!({
                    "role": "user",
                    "content": blocks,
                    "timestamp": ts,
                }));
            }
            Some("branchSummary") => {
                let summary = m.get("summary").and_then(Value::as_str).unwrap_or("");
                let text = format!("{BRANCH_SUMMARY_PREFIX}{summary}{BRANCH_SUMMARY_SUFFIX}");
                out.push(text_user_message(text, &ts));
            }
            Some("compactionSummary") => {
                let summary = m.get("summary").and_then(Value::as_str).unwrap_or("");
                let text =
                    format!("{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}");
                out.push(text_user_message(text, &ts));
            }
            Some("user") | Some("assistant") | Some("toolResult") => {
                out.push(m.clone());
            }
            _ => {}
        }
    }
    out
}
