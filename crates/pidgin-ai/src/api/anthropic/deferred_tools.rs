//! Deferred-tool splitting, ported from pi-ai's
//! `packages/ai/src/utils/deferred-tools.ts` at pinned commit `3da591ab`.
//!
//! A pi `Tool` is `{ name, description, parameters }`; here it stays an opaque
//! [`serde_json::Value`] (matching the ported [`crate::types::Context`] whose
//! `tools` is `Vec<Value>`), so tools round-trip and serialize exactly as pi's.
//! Name normalization is borrowed from [`super::tools`]
//! ([`normalize_tool_name`](super::tools::normalize_tool_name)), matching pi,
//! where `splitDeferredTools` takes a `normalizeName` callback.

use std::collections::HashSet;

use serde_json::Value;

use crate::types::Message;

use super::tools::normalize_tool_name;

/// Read a tool's `name` field (opaque `Value`), defaulting to `""` as pi's
/// property access would yield for a malformed tool.
fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

/// The result of [`split_deferred_tools`]: the immediately-declared tools and
/// the transcript-loaded (deferred) tools, keyed by normalized name in insertion
/// order.
#[derive(Debug, Clone, Default)]
pub struct ToolPlacement {
    pub immediate: Vec<Value>,
    /// Deferred tools as `(normalized_name, tool)` pairs, preserving pi's
    /// `Map` insertion order.
    pub deferred: Vec<(String, Value)>,
}

/// Split current tools into prefix (immediate) and transcript-loaded (deferred)
/// definitions, mirroring pi's `splitDeferredTools`
/// (`utils/deferred-tools.ts:8`). `normalize_name` is applied via
/// [`normalize_tool_name`] with the given `is_oauth` flag.
pub fn split_deferred_tools(
    tools: &[Value],
    messages: &[Message],
    enabled: bool,
    is_oauth: bool,
) -> ToolPlacement {
    // uniqueTools: keyed by normalized name, later entries overwrite the value
    // but keep the first-seen position (JS `Map` semantics).
    let mut order: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    for tool in tools {
        let key = normalize_tool_name(tool_name(tool), is_oauth);
        if let Some(pos) = order.iter().position(|k| k == &key) {
            values[pos] = tool.clone();
        } else {
            order.push(key);
            values.push(tool.clone());
        }
    }

    if !enabled {
        return ToolPlacement {
            immediate: values,
            deferred: Vec::new(),
        };
    }

    let mut deferred_names: HashSet<String> = HashSet::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let crate::types::ContentBlock::ToolCall { name, .. } = block {
                        used_names.insert(normalize_tool_name(name, is_oauth));
                    }
                }
            }
            Message::ToolResult(result) => {
                for name in result.added_tool_names.iter().flatten() {
                    let normalized = normalize_tool_name(name, is_oauth);
                    if !used_names.contains(&normalized) {
                        deferred_names.insert(normalized);
                    }
                }
            }
            Message::User(_) => {}
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = Vec::new();
    for (name, tool) in order.into_iter().zip(values) {
        if deferred_names.contains(&name) {
            deferred.push((name, tool));
        } else {
            immediate.push(tool);
        }
    }
    ToolPlacement {
        immediate,
        deferred,
    }
}
