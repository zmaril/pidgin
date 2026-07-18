//! RPC wire types, ported from pi's `modes/rpc/rpc-types.ts`.
//!
//! Byte-faithful to pi's protocol: commands are tagged by a `type` string,
//! responses by `type:"response"` + a `command` string. Command types are
//! snake_case; the two queue-mode enum values are kebab-case
//! (`all` / `one-at-a-time`); streaming behavior is camelCase (`steer`
//! / `followUp`). `id` is optional and echoed back for correlation, and any
//! `Option` that pi leaves out of the JSON when absent uses
//! `skip_serializing_if = "Option::is_none"` so key-absence matches
//! (`JSON.stringify` omits `undefined`, never emits `null`).

use serde::{Deserialize, Serialize};

/// Thinking-effort levels. Mirrors pi-ai's `ThinkingLevel`
/// (`"minimal"|"low"|"medium"|"high"|"xhigh"|"max"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    /// The next level in a deterministic cycle (wraps `Max` -> `Minimal`).
    pub fn next(self) -> ThinkingLevel {
        use ThinkingLevel::*;
        match self {
            Minimal => Low,
            Low => Medium,
            Medium => High,
            High => Xhigh,
            Xhigh => Max,
            Max => Minimal,
        }
    }
}

/// Pending-message queue mode. Mirrors pi's `"all" | "one-at-a-time"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

/// How a queued prompt should be delivered. Mirrors pi's
/// `streamingBehavior: "steer" | "followUp"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

// ============================================================================
// Commands (stdin)
// ============================================================================

/// The `id` + flattened command envelope. `id` is a shared optional sibling of
/// the `type` tag, so it lives on the envelope rather than inside the enum
/// (serde cannot hoist a shared field into an internally-tagged enum).
#[derive(Debug, Clone, Deserialize)]
pub struct RpcCommandEnvelope {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(flatten)]
    pub command: RpcCommand,
}

/// The RPC command union. Deserializes internally-tagged on `type`
/// (snake_case). Images are kept as opaque JSON — they only flow through the
/// (currently stubbed) prompt/steer/follow_up handlers.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcCommand {
    Prompt {
        message: String,
        #[serde(default)]
        images: Option<Vec<serde_json::Value>>,
        #[serde(default, rename = "streamingBehavior")]
        streaming_behavior: Option<StreamingBehavior>,
    },
    Steer {
        message: String,
        #[serde(default)]
        images: Option<Vec<serde_json::Value>>,
    },
    FollowUp {
        message: String,
        #[serde(default)]
        images: Option<Vec<serde_json::Value>>,
    },
    Abort,
    NewSession {
        #[serde(default, rename = "parentSession")]
        parent_session: Option<String>,
    },
    GetState,
    SetModel {
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    CycleModel,
    GetAvailableModels,
    SetThinkingLevel {
        level: ThinkingLevel,
    },
    CycleThinkingLevel,
    SetSteeringMode {
        mode: QueueMode,
    },
    SetFollowUpMode {
        mode: QueueMode,
    },
    Compact {
        #[serde(default, rename = "customInstructions")]
        custom_instructions: Option<String>,
    },
    SetAutoCompaction {
        enabled: bool,
    },
    SetAutoRetry {
        enabled: bool,
    },
    AbortRetry,
    Bash {
        command: String,
        #[serde(default, rename = "excludeFromContext")]
        exclude_from_context: Option<bool>,
    },
    AbortBash,
    GetSessionStats,
    ExportHtml {
        #[serde(default, rename = "outputPath")]
        output_path: Option<String>,
    },
    SwitchSession {
        #[serde(rename = "sessionPath")]
        session_path: String,
    },
    Fork {
        #[serde(rename = "entryId")]
        entry_id: String,
    },
    Clone,
    GetForkMessages,
    GetEntries {
        #[serde(default)]
        since: Option<String>,
    },
    GetTree,
    GetLastAssistantText,
    SetSessionName {
        name: String,
    },
    GetMessages,
    GetCommands,
}

impl RpcCommand {
    /// The `type` discriminant string pi echoes into responses.
    pub fn type_str(&self) -> &'static str {
        match self {
            RpcCommand::Prompt { .. } => "prompt",
            RpcCommand::Steer { .. } => "steer",
            RpcCommand::FollowUp { .. } => "follow_up",
            RpcCommand::Abort => "abort",
            RpcCommand::NewSession { .. } => "new_session",
            RpcCommand::GetState => "get_state",
            RpcCommand::SetModel { .. } => "set_model",
            RpcCommand::CycleModel => "cycle_model",
            RpcCommand::GetAvailableModels => "get_available_models",
            RpcCommand::SetThinkingLevel { .. } => "set_thinking_level",
            RpcCommand::CycleThinkingLevel => "cycle_thinking_level",
            RpcCommand::SetSteeringMode { .. } => "set_steering_mode",
            RpcCommand::SetFollowUpMode { .. } => "set_follow_up_mode",
            RpcCommand::Compact { .. } => "compact",
            RpcCommand::SetAutoCompaction { .. } => "set_auto_compaction",
            RpcCommand::SetAutoRetry { .. } => "set_auto_retry",
            RpcCommand::AbortRetry => "abort_retry",
            RpcCommand::Bash { .. } => "bash",
            RpcCommand::AbortBash => "abort_bash",
            RpcCommand::GetSessionStats => "get_session_stats",
            RpcCommand::ExportHtml { .. } => "export_html",
            RpcCommand::SwitchSession { .. } => "switch_session",
            RpcCommand::Fork { .. } => "fork",
            RpcCommand::Clone => "clone",
            RpcCommand::GetForkMessages => "get_fork_messages",
            RpcCommand::GetEntries { .. } => "get_entries",
            RpcCommand::GetTree => "get_tree",
            RpcCommand::GetLastAssistantText => "get_last_assistant_text",
            RpcCommand::SetSessionName { .. } => "set_session_name",
            RpcCommand::GetMessages => "get_messages",
            RpcCommand::GetCommands => "get_commands",
        }
    }
}

// ============================================================================
// Session state
// ============================================================================

/// The `get_state` payload. Mirrors pi's `RpcSessionState`. The three optional
/// fields (`model`, `sessionFile`, `sessionName`) are omitted from the JSON
/// when absent, matching pi's `?:` semantics (the client asserts
/// `sessionName === undefined` initially).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<serde_json::Value>,
    pub thinking_level: ThinkingLevel,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: u64,
    pub pending_message_count: u64,
}

/// The `bash` payload. Mirrors pi's `BashResult` (`bash-executor.ts`).
/// `exitCode` is omitted when the process produced no code (matching
/// `number | undefined`); `fullOutputPath` is omitted when unset.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashResult {
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

// ============================================================================
// Responses (stdout)
// ============================================================================

/// A successful response. Mirrors pi's `success()` helper: when `data` is
/// `None` the key is omitted (prompt/steer/abort/...); when `data` is
/// `Some(value)` the value is emitted (and may itself serialize to `null`, e.g.
/// the cycle_model "no change" case). `id` is omitted when absent.
#[derive(Debug, Serialize)]
pub struct RpcSuccess<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub type_: &'static str,
    pub command: &'a str,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A failed response. Mirrors pi's `error()` helper. `command` echoes the
/// failing command's `type` (an open string; for parse errors it is `"parse"`,
/// for unknown commands it is the received type).
#[derive(Debug, Serialize)]
pub struct RpcError<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub type_: &'static str,
    pub command: &'a str,
    pub success: bool,
    pub error: String,
}

/// Build a success response with no `data` key (`success(id, command)`).
pub fn success(id: Option<String>, command: &str) -> RpcSuccess<'_> {
    RpcSuccess {
        id,
        type_: "response",
        command,
        success: true,
        data: None,
    }
}

/// Build a success response carrying `data` (`success(id, command, data)`).
///
/// `data` is serialized as-is, so passing `serde_json::Value::Null` reproduces
/// pi's `success(id, command, null)` (`"data": null`).
pub fn success_data(id: Option<String>, command: &str, data: serde_json::Value) -> RpcSuccess<'_> {
    RpcSuccess {
        id,
        type_: "response",
        command,
        success: true,
        data: Some(data),
    }
}

/// Build an error response (`error(id, command, message)`).
pub fn error(id: Option<String>, command: &str, message: impl Into<String>) -> RpcError<'_> {
    RpcError {
        id,
        type_: "response",
        command,
        success: false,
        error: message.into(),
    }
}
