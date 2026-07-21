//! AgentHarness own-event union and result map, mirroring the harness-level
//! portion of `packages/agent/src/harness/types.ts`.
//!
//! pi's `AgentHarnessOwnEvent` is a discriminated union of the events the
//! harness itself emits (as opposed to the loop-level [`AgentEvent`], which the
//! agent loop emits and which the harness re-exports alongside these). These
//! events are dispatched **in process** to subscriber hooks — pi never
//! `JSON.stringify`s the union as a whole — so several variants carry live
//! runtime handles: an [`AbortSignal`] (on the two `session_before_*` events)
//! and, on `session_before_compact`, compaction's non-serializable
//! [`CompactionPreparation`]. The union therefore mirrors pi as a runtime type
//! (`Debug + Clone`, with [`AgentHarnessOwnEvent::type_str`] reproducing the pi
//! `type` discriminant) rather than a serde enum.
//!
//! Every **data-carrying** payload — the majority — additionally derives
//! `Serialize`/`Deserialize`/`PartialEq` and is covered by wire-shape
//! round-trip tests; field declaration order equals pi's interface property
//! order, which is what `JSON.stringify` emits. Only the two `session_before_*`
//! payloads (which embed a live [`AbortSignal`]) are runtime-only.
//!
//! Compaction-owned payload types ([`CompactionPreparation`], the [`Models`]
//! trait used by [`crate::harness::compaction`]) are imported, never
//! redefined. [`TreePreparation`] is a harness-level type (pi's `types.ts`, not
//! the compaction module) and is defined here.
//!
//! [`AgentEvent`]: crate::types::AgentEvent
//! [`Models`]: crate::harness::compaction::Models

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use pidgin_ai::seams::AbortSignal;
use pidgin_ai::Model;

use crate::harness::compaction::CompactionPreparation;
use crate::harness::prompt_templates::PromptTemplate;
use crate::harness::skills::Skill;
use crate::harness::types::{AgentMessage, BranchSummaryEntry, CompactionEntry, SessionTreeEntry};
use crate::types::ThinkingLevel;

// ---------------------------------------------------------------------------
// Shared harness types referenced by events and options (`types.ts`).
// ---------------------------------------------------------------------------

/// Resources made available to explicit invocation methods and system-prompt
/// callbacks. Mirrors pi's `AgentHarnessResources` (specialized to the crate's
/// concrete [`Skill`]/[`PromptTemplate`], as pi's default type parameters are).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessResources {
    /// Prompt templates available for explicit invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_templates: Option<Vec<PromptTemplate>>,
    /// Skills available to the model and explicit skill invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<Skill>>,
}

/// Preferred transport forwarded to the stream function. Mirrors pi-ai's
/// `Transport` string union (`packages/ai/src/types.ts`), which pidgin-ai does
/// not yet re-export; defined here as the harness's local mirror pending that
/// port (compare the local [`Models`](crate::harness::compaction::Models) seam).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Sse,
    Websocket,
    WebsocketCached,
    Auto,
}

/// Curated provider request options owned by the harness and snapshotted per
/// turn. Mirrors pi's `AgentHarnessStreamOptions`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessStreamOptions {
    /// Preferred transport forwarded to the stream function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    /// Provider request timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    /// Maximum provider retry attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i64>,
    /// Optional cap for provider-requested retry delays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<i64>,
    /// Additional request headers merged with auth and lifecycle headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Provider metadata forwarded with requests (`SimpleStreamOptions["metadata"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// Provider cache retention hint (`SimpleStreamOptions["cacheRetention"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<String>,
}

/// Per-request stream option patch returned by provider hooks. Mirrors pi's
/// `AgentHarnessStreamOptionsPatch` (`Omit<Partial<StreamOptions>, "headers" |
/// "metadata">` plus delete-aware `headers`/`metadata`).
///
/// In the patch, a header/metadata key mapped to `None`/`Value::Null` requests
/// deletion of that key (pi's `undefined` sentinel); an explicit `headers:
/// None`/`metadata: None` at the top level leaves them untouched, matching pi's
/// "omitted" semantics (pi's "clear all" is expressed by the caller passing an
/// empty patch map — the harness merge layer, Wave 6, applies it).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessStreamOptionsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<i64>,
    /// Header patch. `None` values delete keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, Option<String>>>,
    /// Metadata patch. `Value::Null` values delete keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<String>,
}

/// Prepared inputs for a branch/tree navigation summary. Mirrors pi's
/// `TreePreparation` (a harness-level type in `types.ts`, distinct from
/// compaction's `BranchPreparation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreePreparation {
    /// Entry the tree is navigating to.
    pub target_id: String,
    /// Leaf being left, or `None` when the tree had no leaf.
    pub old_leaf_id: Option<String>,
    /// Deepest common ancestor between the old leaf and the target.
    pub common_ancestor_id: Option<String>,
    /// Entries on the abandoned branch selected for summarization.
    pub entries_to_summarize: Vec<SessionTreeEntry>,
    /// Whether the application requested a branch summary.
    pub user_wants_summary: bool,
    /// Optional instructions appended to or replacing the default prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// Replace the default prompt with custom instructions instead of appending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    /// Optional label to apply to the summarized branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Source of a model/tools update. Mirrors pi's `"set" | "restore"` literal
/// unions on `ModelUpdateEvent`/`ToolsUpdateEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateSource {
    Set,
    Restore,
}

/// Harness lifecycle phase. Mirrors pi's `AgentHarnessPhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHarnessPhase {
    Idle,
    Turn,
    Compaction,
    BranchSummary,
    Retry,
}

// ---------------------------------------------------------------------------
// Own-event payloads (`types.ts:502-634`). Field order equals pi's interface
// property order.
// ---------------------------------------------------------------------------

/// `queue_update` — the steer/follow-up/next-turn queues changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueUpdateEvent {
    pub steer: Vec<AgentMessage>,
    pub follow_up: Vec<AgentMessage>,
    pub next_turn: Vec<AgentMessage>,
}

/// `save_point` — a session save point was reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavePointEvent {
    pub had_pending_mutations: bool,
}

/// `abort` — an in-flight run was aborted; carries the drained queues.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortEvent {
    pub cleared_steer: Vec<AgentMessage>,
    pub cleared_follow_up: Vec<AgentMessage>,
}

/// `settled` — the harness settled; carries the number of queued next-turn
/// messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledEvent {
    pub next_turn_count: i64,
}

/// `before_agent_start` — emitted before the agent loop starts a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartEvent {
    pub prompt: String,
    /// Images accompanying the prompt (pi's `ImageContent[]`, kept opaque like
    /// [`AgentMessage`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<Value>>,
    pub system_prompt: String,
    pub resources: AgentHarnessResources,
}

/// `context` — emitted with the rebuilt conversation context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEvent {
    pub messages: Vec<AgentMessage>,
}

/// `before_provider_request` — emitted before a provider request is issued.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderRequestEvent {
    pub model: Model,
    pub session_id: String,
    pub stream_options: AgentHarnessStreamOptions,
}

/// `before_provider_payload` — emitted with the assembled provider payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderPayloadEvent {
    pub model: Model,
    /// Provider-shaped request payload (pi's `unknown`).
    pub payload: Value,
}

/// `after_provider_response` — emitted with the provider response status line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterProviderResponseEvent {
    pub status: i64,
    pub headers: BTreeMap<String, String>,
}

/// `tool_call` — emitted before a tool executes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    /// Tool arguments (pi's `Record<string, unknown>`).
    pub input: Map<String, Value>,
}

/// `tool_result` — emitted after a tool executes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    /// Tool arguments (pi's `Record<string, unknown>`).
    pub input: Map<String, Value>,
    /// Result content (pi's `Array<TextContent | ImageContent>`, kept opaque).
    pub content: Vec<Value>,
    /// Implementation-specific result details (pi's `unknown`).
    pub details: Value,
    pub is_error: bool,
}

/// `session_before_compact` — emitted before compaction runs. Runtime-only: it
/// carries compaction's non-serializable [`CompactionPreparation`] and a live
/// [`AbortSignal`], so (like pi) it is never serialized.
#[derive(Debug, Clone)]
pub struct SessionBeforeCompactEvent {
    pub preparation: CompactionPreparation,
    pub branch_entries: Vec<SessionTreeEntry>,
    pub custom_instructions: Option<String>,
    pub signal: AbortSignal,
}

/// `session_compact` — emitted after a compaction entry is created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactEvent {
    pub compaction_entry: CompactionEntry,
    pub from_hook: bool,
}

/// `session_before_tree` — emitted before a tree navigation. Runtime-only: it
/// carries a live [`AbortSignal`], so (like pi) it is never serialized.
#[derive(Debug, Clone)]
pub struct SessionBeforeTreeEvent {
    pub preparation: TreePreparation,
    pub signal: AbortSignal,
}

/// `session_tree` — emitted after the active leaf moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeEvent {
    pub new_leaf_id: Option<String>,
    pub old_leaf_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_entry: Option<BranchSummaryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

/// `model_update` — the active model changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUpdateEvent {
    pub model: Model,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_model: Option<Model>,
    pub source: UpdateSource,
}

/// `thinking_level_update` — the thinking level changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelUpdateEvent {
    pub level: ThinkingLevel,
    pub previous_level: ThinkingLevel,
}

/// `tools_update` — the tool set and/or active tools changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsUpdateEvent {
    pub tool_names: Vec<String>,
    pub previous_tool_names: Vec<String>,
    pub active_tool_names: Vec<String>,
    pub previous_active_tool_names: Vec<String>,
    pub source: UpdateSource,
}

/// `resources_update` — the harness resources changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesUpdateEvent {
    pub resources: AgentHarnessResources,
    pub previous_resources: AgentHarnessResources,
}

// ---------------------------------------------------------------------------
// The own-event union (`types.ts:636-658`).
// ---------------------------------------------------------------------------

/// The events the harness itself emits. Mirrors pi's `AgentHarnessOwnEvent`.
///
/// A runtime dispatch type (not a serde enum): two variants carry live
/// [`AbortSignal`] handles and compaction's non-serializable
/// [`CompactionPreparation`]. Use [`AgentHarnessOwnEvent::type_str`] for the pi
/// `type` discriminant; the data-carrying payloads round-trip individually.
#[derive(Debug, Clone)]
pub enum AgentHarnessOwnEvent {
    QueueUpdate(QueueUpdateEvent),
    SavePoint(SavePointEvent),
    Abort(AbortEvent),
    Settled(SettledEvent),
    BeforeAgentStart(BeforeAgentStartEvent),
    Context(ContextEvent),
    BeforeProviderRequest(BeforeProviderRequestEvent),
    BeforeProviderPayload(BeforeProviderPayloadEvent),
    AfterProviderResponse(AfterProviderResponseEvent),
    ToolCall(ToolCallEvent),
    ToolResult(ToolResultEvent),
    SessionBeforeCompact(SessionBeforeCompactEvent),
    SessionCompact(SessionCompactEvent),
    SessionBeforeTree(SessionBeforeTreeEvent),
    SessionTree(SessionTreeEvent),
    ModelUpdate(ModelUpdateEvent),
    ThinkingLevelUpdate(ThinkingLevelUpdateEvent),
    ResourcesUpdate(ResourcesUpdateEvent),
    ToolsUpdate(ToolsUpdateEvent),
}

impl AgentHarnessOwnEvent {
    /// The pi `type` discriminant string for this event.
    pub fn type_str(&self) -> &'static str {
        match self {
            AgentHarnessOwnEvent::QueueUpdate(_) => "queue_update",
            AgentHarnessOwnEvent::SavePoint(_) => "save_point",
            AgentHarnessOwnEvent::Abort(_) => "abort",
            AgentHarnessOwnEvent::Settled(_) => "settled",
            AgentHarnessOwnEvent::BeforeAgentStart(_) => "before_agent_start",
            AgentHarnessOwnEvent::Context(_) => "context",
            AgentHarnessOwnEvent::BeforeProviderRequest(_) => "before_provider_request",
            AgentHarnessOwnEvent::BeforeProviderPayload(_) => "before_provider_payload",
            AgentHarnessOwnEvent::AfterProviderResponse(_) => "after_provider_response",
            AgentHarnessOwnEvent::ToolCall(_) => "tool_call",
            AgentHarnessOwnEvent::ToolResult(_) => "tool_result",
            AgentHarnessOwnEvent::SessionBeforeCompact(_) => "session_before_compact",
            AgentHarnessOwnEvent::SessionCompact(_) => "session_compact",
            AgentHarnessOwnEvent::SessionBeforeTree(_) => "session_before_tree",
            AgentHarnessOwnEvent::SessionTree(_) => "session_tree",
            AgentHarnessOwnEvent::ModelUpdate(_) => "model_update",
            AgentHarnessOwnEvent::ThinkingLevelUpdate(_) => "thinking_level_update",
            AgentHarnessOwnEvent::ResourcesUpdate(_) => "resources_update",
            AgentHarnessOwnEvent::ToolsUpdate(_) => "tools_update",
        }
    }
}

// ---------------------------------------------------------------------------
// Per-event result types (`types.ts:664-726`).
// ---------------------------------------------------------------------------

/// Result of a `before_agent_start` subscriber. Mirrors pi's
/// `BeforeAgentStartResult`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAgentStartResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<AgentMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// Result of a `context` subscriber. Mirrors pi's `ContextResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextResult {
    pub messages: Vec<AgentMessage>,
}

/// Result of a `before_provider_request` subscriber. Mirrors pi's
/// `BeforeProviderRequestResult`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderRequestResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<AgentHarnessStreamOptionsPatch>,
}

/// Result of a `before_provider_payload` subscriber. Mirrors pi's
/// `BeforeProviderPayloadResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeProviderPayloadResult {
    /// Replacement provider payload (pi's `unknown`).
    pub payload: Value,
}

/// Result of a `tool_call` subscriber. Mirrors pi's `ToolCallResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of a `tool_result` subscriber. Mirrors pi's `ToolResultPatch`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultPatch {
    /// Replacement content (pi's `Array<TextContent | ImageContent>`, opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Result of a `session_before_compact` subscriber. Mirrors pi's
/// `SessionBeforeCompactResult`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeCompactResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactResult>,
}

/// Inline `{ summary, details? }` override on [`SessionBeforeTreeResult`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeSummaryOverride {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Result of a `session_before_tree` subscriber. Mirrors pi's
/// `SessionBeforeTreeResult`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeforeTreeResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<TreeSummaryOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_instructions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The per-event subscriber result, keyed by event. Mirrors pi's
/// `AgentHarnessEventResultMap`: events that produce a value carry
/// `Option<Result>` (pi's `Result | undefined`); the rest are unit (pi's bare
/// `undefined`). Variant order matches the event union.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentHarnessEventResult {
    QueueUpdate,
    SavePoint,
    Abort,
    Settled,
    BeforeAgentStart(Option<BeforeAgentStartResult>),
    Context(Option<ContextResult>),
    BeforeProviderRequest(Option<BeforeProviderRequestResult>),
    BeforeProviderPayload(Option<BeforeProviderPayloadResult>),
    AfterProviderResponse,
    ToolCall(Option<ToolCallResult>),
    ToolResult(Option<ToolResultPatch>),
    SessionBeforeCompact(Option<SessionBeforeCompactResult>),
    SessionCompact,
    SessionBeforeTree(Option<SessionBeforeTreeResult>),
    SessionTree,
    ModelUpdate,
    ThinkingLevelUpdate,
    ResourcesUpdate,
    ToolsUpdate,
}

impl AgentHarnessEventResult {
    /// The pi `type` discriminant of the event this result corresponds to.
    pub fn type_str(&self) -> &'static str {
        match self {
            AgentHarnessEventResult::QueueUpdate => "queue_update",
            AgentHarnessEventResult::SavePoint => "save_point",
            AgentHarnessEventResult::Abort => "abort",
            AgentHarnessEventResult::Settled => "settled",
            AgentHarnessEventResult::BeforeAgentStart(_) => "before_agent_start",
            AgentHarnessEventResult::Context(_) => "context",
            AgentHarnessEventResult::BeforeProviderRequest(_) => "before_provider_request",
            AgentHarnessEventResult::BeforeProviderPayload(_) => "before_provider_payload",
            AgentHarnessEventResult::AfterProviderResponse => "after_provider_response",
            AgentHarnessEventResult::ToolCall(_) => "tool_call",
            AgentHarnessEventResult::ToolResult(_) => "tool_result",
            AgentHarnessEventResult::SessionBeforeCompact(_) => "session_before_compact",
            AgentHarnessEventResult::SessionCompact => "session_compact",
            AgentHarnessEventResult::SessionBeforeTree(_) => "session_before_tree",
            AgentHarnessEventResult::SessionTree => "session_tree",
            AgentHarnessEventResult::ModelUpdate => "model_update",
            AgentHarnessEventResult::ThinkingLevelUpdate => "thinking_level_update",
            AgentHarnessEventResult::ResourcesUpdate => "resources_update",
            AgentHarnessEventResult::ToolsUpdate => "tools_update",
        }
    }
}

// ---------------------------------------------------------------------------
// Public method-level result/option types (`types.ts:728-748`).
// ---------------------------------------------------------------------------

/// Options for a prompt submission. Mirrors pi's `AgentHarnessPromptOptions`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessPromptOptions {
    /// Images accompanying the prompt (pi's `ImageContent[]`, kept opaque).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<Value>>,
}

/// The drained queues returned by an abort. Mirrors pi's `AbortResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortResult {
    pub cleared_steer: Vec<AgentMessage>,
    pub cleared_follow_up: Vec<AgentMessage>,
}

/// The data a compaction produces, as returned by a compaction override or
/// method. Mirrors pi's `CompactResult` (a harness-level type distinct from
/// compaction's [`CompactionResult`](crate::harness::compaction::CompactionResult),
/// whose `details` is concretely typed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: i64,
    /// Implementation-specific details (pi's `unknown`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// The outcome of a tree navigation. Mirrors pi's `NavigateTreeResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateTreeResult {
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_entry: Option<BranchSummaryEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transport_serializes_to_pi_strings() {
        assert_eq!(serde_json::to_value(Transport::Sse).unwrap(), json!("sse"));
        assert_eq!(
            serde_json::to_value(Transport::Websocket).unwrap(),
            json!("websocket")
        );
        assert_eq!(
            serde_json::to_value(Transport::WebsocketCached).unwrap(),
            json!("websocket-cached")
        );
        assert_eq!(
            serde_json::to_value(Transport::Auto).unwrap(),
            json!("auto")
        );
    }

    #[test]
    fn phase_serializes_to_pi_strings() {
        assert_eq!(
            serde_json::to_value(AgentHarnessPhase::BranchSummary).unwrap(),
            json!("branch_summary")
        );
        assert_eq!(
            serde_json::to_value(AgentHarnessPhase::Idle).unwrap(),
            json!("idle")
        );
    }

    #[test]
    fn update_source_serializes_to_pi_strings() {
        assert_eq!(
            serde_json::to_value(UpdateSource::Set).unwrap(),
            json!("set")
        );
        assert_eq!(
            serde_json::to_value(UpdateSource::Restore).unwrap(),
            json!("restore")
        );
    }

    #[test]
    fn queue_update_round_trips_with_camelcase_fields() {
        let wire = json!({
            "steer": [{"role": "user", "content": "steer me"}],
            "followUp": [],
            "nextTurn": [{"role": "user", "content": "later"}],
        });
        let event: QueueUpdateEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(event.steer.len(), 1);
        assert_eq!(event.next_turn.len(), 1);
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }

    #[test]
    fn save_point_round_trips() {
        let wire = json!({ "hadPendingMutations": true });
        let event: SavePointEvent = serde_json::from_value(wire.clone()).unwrap();
        assert!(event.had_pending_mutations);
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }

    #[test]
    fn settled_round_trips() {
        let wire = json!({ "nextTurnCount": 3 });
        let event: SettledEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(event.next_turn_count, 3);
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }

    #[test]
    fn before_agent_start_omits_absent_images() {
        let event = BeforeAgentStartEvent {
            prompt: "hi".into(),
            images: None,
            system_prompt: "you are helpful".into(),
            resources: AgentHarnessResources::default(),
        };
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(
            wire,
            json!({
                "prompt": "hi",
                "systemPrompt": "you are helpful",
                "resources": {},
            })
        );
        let back: BeforeAgentStartEvent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn tool_result_round_trips_with_declaration_order() {
        let wire = json!({
            "toolCallId": "call_1",
            "toolName": "bash",
            "input": { "command": "ls" },
            "content": [{ "type": "text", "text": "ok" }],
            "details": { "exitCode": 0 },
            "isError": false,
        });
        let event: ToolResultEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(event.tool_name, "bash");
        assert!(!event.is_error);
        // Serialized key order must equal pi's interface property order.
        assert_eq!(serde_json::to_string(&event).unwrap(), wire.to_string());
    }

    #[test]
    fn session_tree_omits_absent_optionals() {
        let event = SessionTreeEvent {
            new_leaf_id: Some("leaf-2".into()),
            old_leaf_id: None,
            summary_entry: None,
            from_hook: None,
        };
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(wire, json!({ "newLeafId": "leaf-2", "oldLeafId": null }));
        let back: SessionTreeEvent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn tools_update_round_trips() {
        let wire = json!({
            "toolNames": ["bash", "read"],
            "previousToolNames": ["bash"],
            "activeToolNames": ["bash"],
            "previousActiveToolNames": [],
            "source": "restore",
        });
        let event: ToolsUpdateEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(event.source, UpdateSource::Restore);
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }

    #[test]
    fn stream_options_omits_absent_fields() {
        let opts = AgentHarnessStreamOptions {
            transport: Some(Transport::Auto),
            timeout_ms: Some(30_000),
            ..Default::default()
        };
        let wire = serde_json::to_value(&opts).unwrap();
        assert_eq!(wire, json!({ "transport": "auto", "timeoutMs": 30000 }));
        let back: AgentHarnessStreamOptions = serde_json::from_value(wire).unwrap();
        assert_eq!(back, opts);
    }

    #[test]
    fn stream_options_patch_carries_delete_sentinels() {
        let mut headers = BTreeMap::new();
        headers.insert("x-keep".to_string(), Some("1".to_string()));
        headers.insert("x-drop".to_string(), None);
        let patch = AgentHarnessStreamOptionsPatch {
            headers: Some(headers),
            ..Default::default()
        };
        let wire = serde_json::to_value(&patch).unwrap();
        assert_eq!(
            wire,
            json!({ "headers": { "x-keep": "1", "x-drop": null } })
        );
        let back: AgentHarnessStreamOptionsPatch = serde_json::from_value(wire).unwrap();
        assert_eq!(back, patch);
    }

    #[test]
    fn compact_result_round_trips() {
        let wire = json!({
            "summary": "did things",
            "firstKeptEntryId": "e5",
            "tokensBefore": 1200,
            "details": { "readFiles": ["a.txt"] },
        });
        let result: CompactResult = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(result.first_kept_entry_id, "e5");
        assert_eq!(serde_json::to_value(&result).unwrap(), wire);
    }

    #[test]
    fn session_before_tree_result_round_trips() {
        let wire = json!({
            "cancel": false,
            "summary": { "summary": "branch summary" },
            "label": "explore",
        });
        let result: SessionBeforeTreeResult = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(result.summary.as_ref().unwrap().summary, "branch summary");
        assert_eq!(serde_json::to_value(&result).unwrap(), wire);
    }

    #[test]
    fn tree_preparation_round_trips() {
        let wire = json!({
            "targetId": "t1",
            "oldLeafId": "l0",
            "commonAncestorId": null,
            "entriesToSummarize": [],
            "userWantsSummary": true,
        });
        let prep: TreePreparation = serde_json::from_value(wire.clone()).unwrap();
        assert!(prep.user_wants_summary);
        assert_eq!(serde_json::to_value(&prep).unwrap(), wire);
    }

    #[test]
    fn own_event_type_str_matches_pi_discriminants() {
        let ev = AgentHarnessOwnEvent::SavePoint(SavePointEvent {
            had_pending_mutations: false,
        });
        assert_eq!(ev.type_str(), "save_point");
        let ev = AgentHarnessOwnEvent::ToolResult(ToolResultEvent {
            tool_call_id: "c".into(),
            tool_name: "t".into(),
            input: Map::new(),
            content: vec![],
            details: Value::Null,
            is_error: false,
        });
        assert_eq!(ev.type_str(), "tool_result");
    }

    #[test]
    fn event_result_type_str_matches_event() {
        assert_eq!(
            AgentHarnessEventResult::ToolCall(None).type_str(),
            "tool_call"
        );
        assert_eq!(
            AgentHarnessEventResult::AfterProviderResponse.type_str(),
            "after_provider_response"
        );
    }
}
