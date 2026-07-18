//! Deferred-tool partitioning, ported from pi-ai's
//! `packages/ai/src/utils/deferred-tools.ts` at pinned commit `3da591ab`.
//!
//! [`split_deferred_tools`] splits a context's current tools into two groups:
//! *immediate* tools (sent with the request up front) and *deferred* tools
//! (definitions introduced later in the transcript, at the tool-result marker
//! that first made them available). A tool is deferred only when a
//! `toolResult`'s `addedToolNames` names it and no earlier assistant `toolCall`
//! has already used it; a tool used before its marker stays immediate. When the
//! feature is disabled every current tool is immediate.
//!
//! # Types
//!
//! pi's `Tool` is a TypeBox-schema-carrying shape not yet ported; in this crate
//! `Context.tools` is `Option<Vec<serde_json::Value>>` (see `types.ts`'s note in
//! `crate::types::Context`), so tools are handled as opaque JSON `Value`s and
//! their name is read from the `"name"` field.
//!
//! # Ordering
//!
//! pi builds `uniqueTools` as a JS `Map` keyed by normalized name: re-inserting a
//! key keeps its first position but overwrites the value (last definition wins),
//! matching OAuth canonicalization that collapses `read`/`Read`. The port
//! reproduces that with an insertion-ordered key list plus a value map, and
//! returns `immediate` as a `Vec` and `deferred` as an insertion-ordered
//! `Vec<(name, tool)>` so downstream serialization order matches pi's `Map`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::types::{ContentBlock, Context, Message};

/// The result of [`split_deferred_tools`]: tools to send immediately, and tools
/// deferred to their transcript markers keyed by normalized name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeferredToolSplit {
    /// Tools sent with the request up front, in insertion order.
    pub immediate: Vec<Value>,
    /// Deferred tool definitions keyed by normalized name, in insertion order.
    pub deferred: Vec<(String, Value)>,
}

impl DeferredToolSplit {
    /// Look up a deferred tool by its normalized name.
    pub fn deferred_get(&self, name: &str) -> Option<&Value> {
        self.deferred
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, tool)| tool)
    }
}

/// The tool-name normalizer signature (`deferred-tools.ts:3`, `ToolNameNormalizer`).
fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

/// Split current tools into immediate and transcript-deferred definitions,
/// leaving names unchanged (`deferred-tools.ts:8`, `normalizeName` defaulting to
/// identity).
pub fn split_deferred_tools(context: &Context, enabled: bool) -> DeferredToolSplit {
    split_deferred_tools_with(context, enabled, |name| name.to_string())
}

/// [`split_deferred_tools`] with an explicit name normalizer (e.g. OAuth
/// canonicalization).
pub fn split_deferred_tools_with(
    context: &Context,
    enabled: bool,
    normalize_name: impl Fn(&str) -> String,
) -> DeferredToolSplit {
    // uniqueTools: insertion-ordered, keyed by normalized name, last value wins.
    let mut order: Vec<String> = Vec::new();
    let mut unique: HashMap<String, Value> = HashMap::new();
    if let Some(tools) = &context.tools {
        for tool in tools {
            let name = normalize_name(tool_name(tool));
            if !unique.contains_key(&name) {
                order.push(name.clone());
            }
            unique.insert(name, tool.clone());
        }
    }

    if !enabled {
        return DeferredToolSplit {
            immediate: order.iter().map(|name| unique[name].clone()).collect(),
            deferred: Vec::new(),
        };
    }

    let mut deferred_names: HashSet<String> = HashSet::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for message in &context.messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let ContentBlock::ToolCall { name, .. } = block {
                        used_names.insert(normalize_name(name));
                    }
                }
            }
            Message::ToolResult(result) => {
                if let Some(added) = &result.added_tool_names {
                    for name in added {
                        let normalized = normalize_name(name);
                        if !used_names.contains(&normalized) {
                            deferred_names.insert(normalized);
                        }
                    }
                }
            }
            Message::User(_) => {}
        }
    }

    let mut immediate: Vec<Value> = Vec::new();
    let mut deferred: Vec<(String, Value)> = Vec::new();
    for name in order {
        let tool = unique[&name].clone();
        if deferred_names.contains(&name) {
            deferred.push((name, tool));
        } else {
            immediate.push(tool);
        }
    }

    DeferredToolSplit {
        immediate,
        deferred,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessage, AssistantRole, StopReason, ToolResultMessage, ToolResultRole, Usage,
        UsageCost, UserContent, UserMessage, UserRole,
    };
    use serde_json::json;

    fn make_tool(name: &str) -> Value {
        json!({ "name": name, "description": format!("The {name} tool"), "parameters": {} })
    }

    fn zero_usage() -> Usage {
        Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        }
    }

    fn user(timestamp: i64) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hello".into()),
            timestamp,
        })
    }

    fn assistant_toolcall(name: &str) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "call_1".into(),
                name: name.into(),
                arguments: json!({}),
                thought_signature: None,
            }],
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: zero_usage(),
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 2,
        })
    }

    fn tool_result(added: &[&str]) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call_1".into(),
            tool_name: "base_tool".into(),
            content: vec![ContentBlock::Text {
                text: "done".into(),
                text_signature: None,
            }],
            details: None,
            added_tool_names: Some(added.iter().map(|s| s.to_string()).collect()),
            is_error: false,
            timestamp: 3,
        })
    }

    fn context(tools: Vec<Value>, added: &[&str]) -> Context {
        Context {
            system_prompt: None,
            messages: vec![
                user(1),
                assistant_toolcall("base_tool"),
                tool_result(added),
                user(4),
            ],
            tools: Some(tools),
        }
    }

    fn names(tools: &[Value]) -> Vec<String> {
        tools.iter().map(|t| tool_name(t).to_string()).collect()
    }

    #[test]
    fn defers_a_tool_at_its_result_marker() {
        let ctx = context(
            vec![make_tool("base_tool"), make_tool("late_tool")],
            &["late_tool"],
        );
        let split = split_deferred_tools(&ctx, true);
        assert_eq!(names(&split.immediate), vec!["base_tool"]);
        assert_eq!(split.deferred.len(), 1);
        assert_eq!(split.deferred[0].0, "late_tool");
        assert!(split.deferred_get("late_tool").is_some());
    }

    #[test]
    fn disabled_keeps_every_tool_immediate() {
        let ctx = context(
            vec![make_tool("base_tool"), make_tool("late_tool")],
            &["late_tool"],
        );
        let split = split_deferred_tools(&ctx, false);
        assert_eq!(names(&split.immediate), vec!["base_tool", "late_tool"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn does_not_resurrect_a_marked_tool_missing_from_tools() {
        let ctx = context(vec![make_tool("base_tool")], &["late_tool"]);
        let split = split_deferred_tools(&ctx, true);
        assert_eq!(names(&split.immediate), vec!["base_tool"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn keeps_a_tool_immediate_when_used_before_its_marker() {
        let mut ctx = context(
            vec![make_tool("base_tool"), make_tool("late_tool")],
            &["late_tool"],
        );
        // Assistant calls late_tool before its marker → stays immediate.
        ctx.messages[1] = assistant_toolcall("late_tool");
        let split = split_deferred_tools(&ctx, true);
        assert_eq!(names(&split.immediate), vec!["base_tool", "late_tool"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn normalizes_names_before_checking_prior_usage() {
        // Marker "read" and active tool "read", but assistant used "Read".
        let ctx = context(vec![make_tool("base_tool"), make_tool("read")], &["read"]);
        let mut ctx = ctx;
        ctx.messages[1] = assistant_toolcall("Read");
        let split = split_deferred_tools_with(&ctx, true, |name| name.to_lowercase());
        assert_eq!(names(&split.immediate), vec!["base_tool", "read"]);
        assert!(split.deferred.is_empty());
    }

    #[test]
    fn deduplicates_active_tools_after_normalization() {
        // Two tools canonicalize to the same key; last definition wins, first
        // position is kept.
        let ctx = Context {
            system_prompt: None,
            messages: vec![user(1)],
            tools: Some(vec![
                make_tool("read"),
                json!({ "name": "Read", "description": "Canonical definition", "parameters": {} }),
            ]),
        };
        let split = split_deferred_tools_with(&ctx, true, |name| name.to_lowercase());
        assert_eq!(split.immediate.len(), 1);
        assert_eq!(tool_name(&split.immediate[0]), "Read");
        assert_eq!(
            split.immediate[0]["description"],
            json!("Canonical definition")
        );
    }

    #[test]
    fn no_tools_yields_empty_split() {
        let ctx = Context {
            system_prompt: None,
            messages: vec![user(1)],
            tools: None,
        };
        let split = split_deferred_tools(&ctx, true);
        assert!(split.immediate.is_empty());
        assert!(split.deferred.is_empty());
    }
}
