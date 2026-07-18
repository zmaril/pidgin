//! Minimal serde mirrors of pi's session data shapes.
//!
//! These types reproduce the JSON field names and structure that pi serializes
//! into the exported HTML (the base64-embedded `session-data` script). pi builds
//! these values from `SessionManager`, `AgentState`, and the `pi-ai` message
//! types, none of which are on main yet. Until the shared session/ai crates land,
//! this module defines local mirrors so the export can be exercised end to end.
//! Every type here migrates to those shared crates once they exist.
//!
//! Fields that pi marks optional are `Option` and skipped when absent, matching
//! `JSON.stringify`'s omission of `undefined`. A handful of provider-metadata
//! fields that pi marks required (for example an assistant message's `usage`) are
//! modeled as optional passthrough here so a caller can construct minimal session
//! data before the shared crates provide the full types; when present they
//! serialize under pi's exact field names.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Session header. Mirrors pi's `SessionHeader` (`type: "session"`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionHeader {
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

impl SessionHeader {
    /// Build a header with the constant `type: "session"` discriminant.
    pub fn new(
        id: impl Into<String>,
        timestamp: impl Into<String>,
        cwd: impl Into<String>,
    ) -> Self {
        SessionHeader {
            entry_type: "session".to_string(),
            version: None,
            id: id.into(),
            timestamp: timestamp.into(),
            cwd: cwd.into(),
            parent_session: None,
        }
    }
}

/// A content block within a message. Superset of pi's `TextContent`,
/// `ThinkingContent`, `ImageContent`, and `ToolCall`, discriminated by `type`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(rename = "textSignature", skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(rename = "thinkingSignature", skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

/// A user message's content, which pi allows to be either a plain string or an
/// array of content blocks.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// A user message. Mirrors pi's `UserMessage`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserMessage {
    pub content: MessageContent,
    pub timestamp: i64,
}

/// An assistant message. Mirrors pi's `AssistantMessage`. Provider-metadata
/// fields are optional passthrough (see the module note).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "responseModel", skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(rename = "responseId", skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(rename = "stopReason", skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Value>,
    pub timestamp: i64,
}

/// A tool-result message. Mirrors pi's `ToolResultMessage`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultMessage {
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(rename = "addedToolNames", skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    #[serde(rename = "isError")]
    pub is_error: bool,
    pub timestamp: i64,
}

/// A message on the session transcript, discriminated by `role`. Mirrors pi's
/// `AgentMessage` (which is `Message` plus app-custom messages).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum AgentMessage {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "toolResult")]
    ToolResult(ToolResultMessage),
}

/// Fields shared by every session entry (`id`, `parentId`, `timestamp`).
///
/// Flattened into each entry struct so the shared tree-structure fields are
/// declared once. Mirrors pi's `SessionEntryBase`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryBase {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub message: AgentMessage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub thinking_level: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub provider: String,
    pub model_id: String,
}

/// Optional extension metadata carried by compaction and branch-summary entries
/// (`details`, `fromHook`). Flattened into both so the shared pair is declared
/// once. Mirrors the matching optional fields on pi's `CompactionEntry` and
/// `BranchSummaryEntry`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryHookMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: i64,
    #[serde(flatten)]
    pub hook_meta: EntryHookMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub from_id: String,
    pub summary: String,
    #[serde(flatten)]
    pub hook_meta: EntryHookMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub custom_type: String,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub display: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    pub target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    #[serde(flatten)]
    pub base: EntryBase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A session entry, discriminated by `type`. Mirrors pi's `SessionEntry` union.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEntry {
    #[serde(rename = "message")]
    Message(MessageEntry),
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    #[serde(rename = "model_change")]
    ModelChange(ModelChangeEntry),
    #[serde(rename = "compaction")]
    Compaction(CompactionEntry),
    #[serde(rename = "branch_summary")]
    BranchSummary(BranchSummaryEntry),
    #[serde(rename = "custom")]
    Custom(CustomEntry),
    #[serde(rename = "custom_message")]
    CustomMessage(CustomMessageEntry),
    #[serde(rename = "label")]
    Label(LabelEntry),
    #[serde(rename = "session_info")]
    SessionInfo(SessionInfoEntry),
}

/// A tool description as embedded in the export. Mirrors pi's
/// `Pick<ToolDefinition, "name" | "description" | "parameters">`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Pre-rendered HTML for a custom tool call and result. Mirrors pi's
/// `RenderedToolHtml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedToolHtml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_html_collapsed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_html_expanded: Option<String>,
}

/// The full session payload embedded into the exported HTML. Mirrors pi's
/// `SessionData`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionData {
    /// Serialized as `null` when absent, matching pi's `SessionHeader | null`.
    pub header: Option<SessionHeader>,
    pub entries: Vec<SessionEntry>,
    /// Serialized as `null` when absent, matching pi's `string | null`.
    pub leaf_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered_tools: Option<HashMap<String, RenderedToolHtml>>,
}

/// Build a simple user-message session entry. Shared by the tests in this module
/// and the sibling `tool_renderer` / `mod` test modules.
#[cfg(test)]
pub(crate) fn user_message_entry(id: &str, text: &str) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        base: EntryBase {
            id: id.to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        },
        message: AgentMessage::User(UserMessage {
            content: MessageContent::Text(text.to_string()),
            timestamp: 0,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_data_serializes_with_pi_field_names() {
        let data = SessionData {
            header: Some(SessionHeader::new("s1", "2026-01-01T00:00:00Z", "/tmp")),
            entries: vec![user_message_entry("e1", "hi")],
            leaf_id: None,
            system_prompt: None,
            tools: None,
            rendered_tools: None,
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["header"]["type"], "session");
        assert_eq!(json["header"]["cwd"], "/tmp");
        // Absent optionals are omitted; nullable fields serialize as null.
        assert!(json["leafId"].is_null());
        assert!(json.get("systemPrompt").is_none());
        assert!(json.get("renderedTools").is_none());
        assert_eq!(json["entries"][0]["type"], "message");
        assert_eq!(json["entries"][0]["parentId"], Value::Null);
        assert_eq!(json["entries"][0]["message"]["role"], "user");
        assert_eq!(json["entries"][0]["message"]["content"], "hi");
    }

    #[test]
    fn compaction_entry_flattens_base_and_hook_meta() {
        let entry = SessionEntry::Compaction(CompactionEntry {
            base: EntryBase {
                id: "c1".to_string(),
                parent_id: Some("p0".to_string()),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            },
            summary: "did stuff".to_string(),
            first_kept_entry_id: "e5".to_string(),
            tokens_before: 1234,
            hook_meta: EntryHookMeta::default(),
        });
        let json = serde_json::to_value(&entry).unwrap();
        // The tag plus both flattened structs land at the top level.
        assert_eq!(json["type"], "compaction");
        assert_eq!(json["id"], "c1");
        assert_eq!(json["parentId"], "p0");
        assert_eq!(json["summary"], "did stuff");
        assert_eq!(json["firstKeptEntryId"], "e5");
        assert_eq!(json["tokensBefore"], 1234);
        // Absent hook metadata is omitted.
        assert!(json.get("details").is_none());
        assert!(json.get("fromHook").is_none());
    }

    #[test]
    fn tool_call_block_serializes_camelcase() {
        let block = ContentBlock::ToolCall {
            id: "tc1".to_string(),
            name: "bash".to_string(),
            arguments: serde_json::json!({ "cmd": "ls" }),
            thought_signature: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "toolCall");
        assert_eq!(json["name"], "bash");
        assert_eq!(json["arguments"]["cmd"], "ls");
        assert!(json.get("thoughtSignature").is_none());
    }

    #[test]
    fn rendered_tool_html_uses_camelcase_keys() {
        let rendered = RenderedToolHtml {
            call_html: Some("<div>call</div>".to_string()),
            result_html_collapsed: None,
            result_html_expanded: Some("<div>result</div>".to_string()),
        };
        let json = serde_json::to_value(&rendered).unwrap();
        assert_eq!(json["callHtml"], "<div>call</div>");
        assert_eq!(json["resultHtmlExpanded"], "<div>result</div>");
        assert!(json.get("resultHtmlCollapsed").is_none());
    }
}
