//! AgentHarness construction options, error type, and pending-write union,
//! mirroring the remaining harness-level types in
//! `packages/agent/src/harness/types.ts`.
//!
//! [`AgentHarnessOptions`] is the harness's construction input; like pi's
//! `AgentHarnessOptions` it holds live handles (the [`ExecutionEnv`], the
//! [`Session`], the compaction [`Models`] provider collection, and an optional
//! system-prompt callback), so it derives no traits. [`AgentHarnessError`] is
//! the public failure with pi's stable top-level [`AgentHarnessErrorCode`]
//! classification (including the `busy` phase-guard code). [`PendingSessionWrite`]
//! mirrors pi's `Omit<SessionTreeEntry, "id" | "parentId" | "timestamp">`: an
//! entry queued for append before storage assigns the base fields.
//!
//! The compaction [`Models`] trait is imported from
//! [`crate::harness::compaction`] and never redefined.

// straitjacket-allow-file:duplication — `PendingSessionWrite`'s per-variant
// draft structs are the `SessionTreeEntry` variants with their base fields
// (`id`/`parentId`/`timestamp`) removed; the parallel field shapes mirror
// `harness/types.rs` by construction, not by extractable duplication.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{AssistantMessageEvent, Context, Model};

use crate::harness::compaction::Models;
use crate::harness::env::ExecutionEnv;
use crate::harness::events::{AgentHarnessResources, AgentHarnessStreamOptions};
use crate::harness::session::Session;
use crate::harness::types::AgentMessage;
use crate::types::{AgentTool, QueueMode, ThinkingLevel};

// ---------------------------------------------------------------------------
// AgentHarnessError (`types.ts:207-227`).
// ---------------------------------------------------------------------------

/// Stable top-level AgentHarness error classification. Mirrors pi's
/// `AgentHarnessErrorCode`. `busy` is the phase-guard code returned when an
/// operation is rejected because the harness is mid-turn/mid-compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHarnessErrorCode {
    Busy,
    InvalidState,
    InvalidArgument,
    Session,
    Hook,
    Auth,
    Compaction,
    BranchSummary,
    Unknown,
}

impl AgentHarnessErrorCode {
    /// The wire string pi uses for this code (`AgentHarnessError.code`).
    pub fn as_str(self) -> &'static str {
        match self {
            AgentHarnessErrorCode::Busy => "busy",
            AgentHarnessErrorCode::InvalidState => "invalid_state",
            AgentHarnessErrorCode::InvalidArgument => "invalid_argument",
            AgentHarnessErrorCode::Session => "session",
            AgentHarnessErrorCode::Hook => "hook",
            AgentHarnessErrorCode::Auth => "auth",
            AgentHarnessErrorCode::Compaction => "compaction",
            AgentHarnessErrorCode::BranchSummary => "branch_summary",
            AgentHarnessErrorCode::Unknown => "unknown",
        }
    }
}

/// Public AgentHarness failure with a stable top-level classification. Mirrors
/// pi's `AgentHarnessError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHarnessError {
    pub code: AgentHarnessErrorCode,
    pub message: String,
}

impl AgentHarnessError {
    pub fn new(code: AgentHarnessErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// A `busy`-coded error: the harness rejected an operation because it is
    /// already running a turn/compaction/branch-summary (pi's phase guard).
    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(AgentHarnessErrorCode::Busy, message)
    }
}

impl fmt::Display for AgentHarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for AgentHarnessError {}

// ---------------------------------------------------------------------------
// PendingSessionWrite (`types.ts:496-500`).
// ---------------------------------------------------------------------------

/// `message` draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMessage {
    pub message: AgentMessage,
}

/// `thinking_level_change` draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingThinkingLevelChange {
    pub thinking_level: String,
}

/// `model_change` draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingModelChange {
    pub provider: String,
    pub model_id: String,
}

/// `active_tools_change` draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingActiveToolsChange {
    pub active_tool_names: Vec<String>,
}

/// `compaction` draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCompaction {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

/// `branch_summary` draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBranchSummary {
    pub from_id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

/// `custom` draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCustom {
    pub custom_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `custom_message` draft. Field order matches pi's append site
/// (`customType, content, display, details`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCustomMessage {
    pub custom_type: String,
    pub content: Value,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// `label` draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingLabel {
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `session_info` draft (legacy name kept for compatibility).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSessionInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `leaf` draft. `target_id` serializes as explicit `null` when cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingLeaf {
    pub target_id: Option<String>,
}

/// An entry queued for append, before storage assigns `id`/`parentId`/
/// `timestamp`. Mirrors pi's `PendingSessionWrite = Omit<SessionTreeEntry, "id"
/// | "parentId" | "timestamp">`. Serializes internally-tagged on `type`, like
/// [`SessionTreeEntry`](crate::harness::types::SessionTreeEntry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PendingSessionWrite {
    Message(PendingMessage),
    ThinkingLevelChange(PendingThinkingLevelChange),
    ModelChange(PendingModelChange),
    ActiveToolsChange(PendingActiveToolsChange),
    Compaction(PendingCompaction),
    BranchSummary(PendingBranchSummary),
    Custom(PendingCustom),
    CustomMessage(PendingCustomMessage),
    Label(PendingLabel),
    SessionInfo(PendingSessionInfo),
    Leaf(PendingLeaf),
}

impl PendingSessionWrite {
    /// The `type` discriminant string.
    pub fn type_str(&self) -> &'static str {
        match self {
            PendingSessionWrite::Message(_) => "message",
            PendingSessionWrite::ThinkingLevelChange(_) => "thinking_level_change",
            PendingSessionWrite::ModelChange(_) => "model_change",
            PendingSessionWrite::ActiveToolsChange(_) => "active_tools_change",
            PendingSessionWrite::Compaction(_) => "compaction",
            PendingSessionWrite::BranchSummary(_) => "branch_summary",
            PendingSessionWrite::Custom(_) => "custom",
            PendingSessionWrite::CustomMessage(_) => "custom_message",
            PendingSessionWrite::Label(_) => "label",
            PendingSessionWrite::SessionInfo(_) => "session_info",
            PendingSessionWrite::Leaf(_) => "leaf",
        }
    }
}

// ---------------------------------------------------------------------------
// AgentHarnessOptions (`types.ts:800-836`).
// ---------------------------------------------------------------------------

/// Context passed to a dynamic system-prompt callback. Mirrors the object pi's
/// `systemPrompt` function receives.
pub struct SystemPromptContext<'a> {
    pub env: &'a dyn ExecutionEnv,
    pub session: &'a Session,
    pub model: &'a Model,
    pub thinking_level: ThinkingLevel,
    pub active_tools: &'a [AgentTool],
    pub resources: &'a AgentHarnessResources,
}

/// A system prompt: either a fixed string or a callback computed per turn.
/// Mirrors pi's `string | ((context) => string | Promise<string>)`; the port is
/// synchronous, so the callback returns a `String` directly.
pub enum SystemPromptSource {
    /// A fixed system prompt string.
    Static(String),
    /// A callback invoked with the live [`SystemPromptContext`] per turn.
    Dynamic(Box<dyn for<'a> Fn(SystemPromptContext<'a>) -> String>),
}

impl fmt::Debug for SystemPromptSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SystemPromptSource::Static(s) => f.debug_tuple("Static").field(s).finish(),
            SystemPromptSource::Dynamic(_) => f.debug_tuple("Dynamic").field(&"<fn>").finish(),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider streaming seam (`createStreamFn` → `models.streamSimple`).
// ---------------------------------------------------------------------------

/// A provider-streaming request assembled by the harness for a single turn,
/// mirroring the option bag pi hands to `this.models.streamSimple(model,
/// context, { … })` inside `createStreamFn`.
///
/// pidgin-ai's [`StreamOptions`](pidgin_ai::StreamOptions) is a narrow subset
/// (only `sessionId`/`cacheRetention`), so the richer pi request fields
/// (`headers`, `metadata`, `timeoutMs`, `maxRetries`, `maxRetryDelayMs`,
/// `transport`, `cacheRetention`) travel on the fully-merged
/// [`AgentHarnessStreamOptions`] in [`options`](Self::options) — the value
/// produced after the harness applies every `before_provider_request` patch.
/// The two callbacks mirror pi's `onPayload`/`onResponse`: [`on_payload`] runs
/// the `before_provider_payload` hook chain over a provider payload (returning
/// the possibly-replaced payload), and [`on_response`] delivers the provider
/// status line to the `after_provider_response` subscribers.
///
/// [`on_payload`]: Self::on_payload
/// [`on_response`]: Self::on_response
pub struct ProviderStreamRequest<'a> {
    /// Model to stream.
    pub model: &'a Model,
    /// LLM-ready context (already converted by the loop's `convertToLlm`).
    pub context: &'a Context,
    /// Session id forwarded for session-scoped caching (pi's `sessionId`).
    pub session_id: &'a str,
    /// Requested reasoning level for this turn (pi's `reasoning`), or `None`
    /// when thinking is off.
    pub reasoning: Option<ThinkingLevel>,
    /// Fully-merged request options after `before_provider_request` patching.
    pub options: &'a AgentHarnessStreamOptions,
    /// Cooperative abort signal for this turn (pi's `signal`).
    pub signal: Option<&'a AbortSignal>,
    /// Runs the `before_provider_payload` hook chain (pi's `onPayload`).
    pub on_payload: &'a dyn Fn(Value) -> Value,
    /// Delivers the provider response status/headers to subscribers (pi's
    /// `onResponse`).
    pub on_response: &'a dyn Fn(i64, BTreeMap<String, String>),
}

/// The harness's provider-streaming seam — the eager analog of pi's
/// `models.streamSimple`. It turns a [`ProviderStreamRequest`] into an eager
/// [`StreamResult`] (`{ events, message }`), exactly as the low-level loop's
/// [`StreamFn`](crate::types::StreamFn) does.
///
/// Held behind the harness's single-threaded interior (never sent across
/// threads), so it is a plain `Rc<dyn Fn>` and need not be `Send + Sync`; a
/// test fake can therefore capture `Rc`/`RefCell` recorders and forward to a
/// `FauxProvider`.
pub type ProviderStream = Rc<dyn for<'a> Fn(ProviderStreamRequest<'a>) -> StreamResult>;

/// The optional incremental sibling of [`ProviderStream`]: it DRIVES the
/// provider one event at a time, invoking `sink` per pulled event so the turn
/// streams tokens with real inter-event timing instead of materializing the full
/// [`StreamResult`] up front. It internally drives a borrowed
/// [`AssistantEventReader`](pidgin_ai::utils::sse::AssistantEventReader) (the
/// borrow never escapes the closure) and returns the terminal [`StreamResult`]
/// (its `message` is final; its `events` may be empty, having been delivered
/// through `sink`).
///
/// Same single-threaded `Rc` interior as [`ProviderStream`]. When a harness is
/// built without one, each turn falls back to the buffered [`ProviderStream`]
/// with unchanged behavior.
pub type IncrementalProviderStream = Rc<
    dyn for<'a> Fn(
        ProviderStreamRequest<'a>,
        &mut dyn FnMut(&AssistantMessageEvent),
    ) -> StreamResult,
>;

/// AgentHarness construction options. Mirrors pi's `AgentHarnessOptions`
/// (specialized to the crate's concrete [`Skill`](crate::harness::skills::Skill)/
/// [`PromptTemplate`](crate::harness::prompt_templates::PromptTemplate)/
/// [`AgentTool`], as pi's default type parameters are).
///
/// Holds live handles ([`env`](Self::env), [`session`](Self::session),
/// [`models`](Self::models), and the optional system-prompt callback), so it
/// derives no traits — it is a builder input consumed once at construction.
pub struct AgentHarnessOptions {
    /// Filesystem and process execution environment.
    pub env: Box<dyn ExecutionEnv>,
    /// The conversation session the harness drives.
    pub session: Session,
    /// Provider collection used for compaction and branch summarization
    /// (`completeSimple`). Reuses the compaction [`Models`] seam.
    pub models: Box<dyn Models>,
    /// Provider streaming seam used to drive each turn's assistant response —
    /// the eager analog of routing turns through `models.streamSimple`. Kept
    /// separate because pidgin-ai does not (yet) wrap `streamSimple`, and the
    /// compaction [`Models`] trait ports only `completeSimple`.
    pub stream: ProviderStream,
    /// Optional incremental provider-streaming seam. When set, each turn DRIVES
    /// the provider one event at a time through it (real token-by-token timing);
    /// when `None`, turns use the buffered [`stream`](Self::stream) path with
    /// unchanged behavior. Additive: existing harnesses leave it `None`.
    pub stream_incremental: Option<IncrementalProviderStream>,
    /// Tools available to the model. Defaults to none.
    pub tools: Option<Vec<AgentTool>>,
    /// Concrete resources available to explicit invocation methods and
    /// system-prompt callbacks. Applications own loading/reloading and call
    /// `set_resources()` (Wave 6) with new values.
    pub resources: Option<AgentHarnessResources>,
    /// System prompt: a fixed string or a per-turn callback. Defaults to none.
    pub system_prompt: Option<SystemPromptSource>,
    /// Curated stream/provider request options. Snapshotted at turn start.
    pub stream_options: Option<AgentHarnessStreamOptions>,
    /// The active model.
    pub model: Model,
    /// The active thinking level. Defaults per the harness (Wave 6).
    pub thinking_level: Option<ThinkingLevel>,
    /// The active tool names. Defaults per the harness (Wave 6).
    pub active_tool_names: Option<Vec<String>>,
    /// Steering queue drain mode.
    pub steering_mode: Option<QueueMode>,
    /// Follow-up queue drain mode.
    pub follow_up_mode: Option<QueueMode>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn error_codes_map_to_pi_strings() {
        assert_eq!(AgentHarnessErrorCode::Busy.as_str(), "busy");
        assert_eq!(
            AgentHarnessErrorCode::InvalidState.as_str(),
            "invalid_state"
        );
        assert_eq!(
            AgentHarnessErrorCode::InvalidArgument.as_str(),
            "invalid_argument"
        );
        assert_eq!(AgentHarnessErrorCode::Session.as_str(), "session");
        assert_eq!(AgentHarnessErrorCode::Hook.as_str(), "hook");
        assert_eq!(AgentHarnessErrorCode::Auth.as_str(), "auth");
        assert_eq!(AgentHarnessErrorCode::Compaction.as_str(), "compaction");
        assert_eq!(
            AgentHarnessErrorCode::BranchSummary.as_str(),
            "branch_summary"
        );
        assert_eq!(AgentHarnessErrorCode::Unknown.as_str(), "unknown");
    }

    #[test]
    fn busy_constructor_sets_code() {
        let err = AgentHarnessError::busy("harness is running a turn");
        assert_eq!(err.code, AgentHarnessErrorCode::Busy);
        assert_eq!(err.to_string(), "harness is running a turn");
    }

    #[test]
    fn pending_write_serializes_type_tag_first() {
        let write = PendingSessionWrite::ModelChange(PendingModelChange {
            provider: "anthropic".into(),
            model_id: "claude".into(),
        });
        assert_eq!(write.type_str(), "model_change");
        let wire = serde_json::to_value(&write).unwrap();
        assert_eq!(
            wire,
            json!({
                "type": "model_change",
                "provider": "anthropic",
                "modelId": "claude",
            })
        );
        let back: PendingSessionWrite = serde_json::from_value(wire).unwrap();
        assert_eq!(back, write);
    }

    #[test]
    fn pending_leaf_keeps_explicit_null_target() {
        let write = PendingSessionWrite::Leaf(PendingLeaf { target_id: None });
        let wire = serde_json::to_value(&write).unwrap();
        assert_eq!(wire, json!({ "type": "leaf", "targetId": null }));
    }

    #[test]
    fn pending_custom_message_field_order_matches_append_site() {
        let write = PendingSessionWrite::CustomMessage(PendingCustomMessage {
            custom_type: "note".into(),
            content: json!("hello"),
            display: true,
            details: None,
        });
        let wire = serde_json::to_string(&write).unwrap();
        assert_eq!(
            wire,
            r#"{"type":"custom_message","customType":"note","content":"hello","display":true}"#
        );
    }
}
