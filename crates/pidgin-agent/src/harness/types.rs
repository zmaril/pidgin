//! Session-tree types mirroring `packages/agent/src/harness/types.ts`.
//!
//! `AgentMessage` and the free-form payloads (`data`, `details`,
//! `custom_message.content`, header `metadata`) are typed as
//! [`serde_json::Value`] so arbitrary pi payloads round-trip byte-for-byte.
//! Struct field declaration order equals pi's object-literal insertion order,
//! which is exactly what `JSON.stringify` emits and what byte-exactness
//! depends on.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Free-form agent message payload. Mirrors pi's `AgentMessage` union, which
/// this port keeps opaque (the concrete message shapes live in pi-ai).
pub type AgentMessage = Value;

/// `packages/agent/src/harness/types.ts` `SessionErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionErrorCode {
    NotFound,
    InvalidSession,
    InvalidEntry,
    InvalidForkTarget,
    Storage,
    Unknown,
}

impl SessionErrorCode {
    /// The wire string pi uses for this code (`SessionError.code`).
    pub fn as_str(self) -> &'static str {
        match self {
            SessionErrorCode::NotFound => "not_found",
            SessionErrorCode::InvalidSession => "invalid_session",
            SessionErrorCode::InvalidEntry => "invalid_entry",
            SessionErrorCode::InvalidForkTarget => "invalid_fork_target",
            SessionErrorCode::Storage => "storage",
            SessionErrorCode::Unknown => "unknown",
        }
    }
}

/// Error thrown by session storage, repositories, and tree operations.
/// Mirrors pi's `SessionError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub code: SessionErrorCode,
    pub message: String,
}

impl SessionError {
    pub fn new(code: SessionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// A `storage`-coded error (I/O and filesystem failures).
    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::Storage, message)
    }

    /// A `not_found`-coded error for a missing tree entry.
    pub fn entry_not_found(id: &str) -> Self {
        Self::new(SessionErrorCode::NotFound, format!("Entry {id} not found"))
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for SessionError {}

/// `message` entry — carries an `AgentMessage`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: AgentMessage,
}

/// `thinking_level_change` entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub thinking_level: String,
}

/// `model_change` entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub provider: String,
    pub model_id: String,
}

/// `active_tools_change` entry (agent-core only).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub active_tool_names: Vec<String>,
}

/// `compaction` entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

/// `branch_summary` entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub from_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

/// `custom` entry — application-defined, omitted from model context by default.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `custom_message` entry. Field order matches pi's append site
/// (`customType, content, display, details`), not the interface declaration.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    pub content: Value,
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// `label` entry. A cleared label omits the `label` key entirely.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `session_info` entry (legacy name kept for compatibility).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `leaf` entry — the persisted active-leaf pointer (agent-core only).
/// `target_id` serializes as explicit `null` when the leaf is cleared.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LeafEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: Option<String>,
}

/// The session-tree entry union. Serializes internally-tagged on `type`; the
/// tag is emitted first, then the variant's fields in declaration order.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionTreeEntry {
    Message(MessageEntry),
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    ModelChange(ModelChangeEntry),
    ActiveToolsChange(ActiveToolsChangeEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
    CustomMessage(CustomMessageEntry),
    Label(LabelEntry),
    SessionInfo(SessionInfoEntry),
    Leaf(LeafEntry),
}

impl SessionTreeEntry {
    /// The `type` discriminant string.
    pub fn type_str(&self) -> &'static str {
        match self {
            SessionTreeEntry::Message(_) => "message",
            SessionTreeEntry::ThinkingLevelChange(_) => "thinking_level_change",
            SessionTreeEntry::ModelChange(_) => "model_change",
            SessionTreeEntry::ActiveToolsChange(_) => "active_tools_change",
            SessionTreeEntry::Compaction(_) => "compaction",
            SessionTreeEntry::BranchSummary(_) => "branch_summary",
            SessionTreeEntry::Custom(_) => "custom",
            SessionTreeEntry::CustomMessage(_) => "custom_message",
            SessionTreeEntry::Label(_) => "label",
            SessionTreeEntry::SessionInfo(_) => "session_info",
            SessionTreeEntry::Leaf(_) => "leaf",
        }
    }

    /// The entry `id`.
    pub fn id(&self) -> &str {
        match self {
            SessionTreeEntry::Message(e) => &e.id,
            SessionTreeEntry::ThinkingLevelChange(e) => &e.id,
            SessionTreeEntry::ModelChange(e) => &e.id,
            SessionTreeEntry::ActiveToolsChange(e) => &e.id,
            SessionTreeEntry::Compaction(e) => &e.id,
            SessionTreeEntry::BranchSummary(e) => &e.id,
            SessionTreeEntry::Custom(e) => &e.id,
            SessionTreeEntry::CustomMessage(e) => &e.id,
            SessionTreeEntry::Label(e) => &e.id,
            SessionTreeEntry::SessionInfo(e) => &e.id,
            SessionTreeEntry::Leaf(e) => &e.id,
        }
    }

    /// The entry `parentId` (`None` serializes as JSON `null`).
    pub fn parent_id(&self) -> Option<&str> {
        let parent = match self {
            SessionTreeEntry::Message(e) => &e.parent_id,
            SessionTreeEntry::ThinkingLevelChange(e) => &e.parent_id,
            SessionTreeEntry::ModelChange(e) => &e.parent_id,
            SessionTreeEntry::ActiveToolsChange(e) => &e.parent_id,
            SessionTreeEntry::Compaction(e) => &e.parent_id,
            SessionTreeEntry::BranchSummary(e) => &e.parent_id,
            SessionTreeEntry::Custom(e) => &e.parent_id,
            SessionTreeEntry::CustomMessage(e) => &e.parent_id,
            SessionTreeEntry::Label(e) => &e.parent_id,
            SessionTreeEntry::SessionInfo(e) => &e.parent_id,
            SessionTreeEntry::Leaf(e) => &e.parent_id,
        };
        parent.as_deref()
    }

    /// The leaf id after this entry: a leaf line yields its `targetId`,
    /// any other entry yields its own `id`.
    pub fn leaf_id_after(&self) -> Option<String> {
        match self {
            SessionTreeEntry::Leaf(e) => e.target_id.clone(),
            other => Some(other.id().to_string()),
        }
    }
}

/// A reference to the active model, as derived into a [`SessionContext`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub model_id: String,
}

/// Rebuilt conversation context. Mirrors agent-core's `SessionContext`,
/// including `activeToolNames`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<ModelRef>,
    pub active_tool_names: Option<Vec<String>>,
}

/// Session metadata. Unifies pi's `SessionMetadata` (in-memory: `id`,
/// `createdAt`) and `JsonlSessionMetadata` (adds `cwd`, `path`,
/// `parentSessionPath`, `metadata`); the JSONL-only fields are `None` for
/// in-memory storage.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: String,
    pub cwd: Option<String>,
    pub path: Option<String>,
    pub parent_session_path: Option<String>,
    pub metadata: Option<serde_json::Map<String, Value>>,
}

impl SessionMetadata {
    /// An in-memory metadata record (no JSONL fields).
    pub fn in_memory(id: impl Into<String>, created_at: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            created_at: created_at.into(),
            cwd: None,
            path: None,
            parent_session_path: None,
            metadata: None,
        }
    }
}
