//! Agent-loop boundary types ported from `packages/agent/src/types.ts`.
//!
//! This module mirrors pi-agent-core's `types.ts` — the public contract shared
//! by the agent, the low-level agent loop, and the UI. The pi module imports its
//! LLM primitives (`AssistantMessage`, `Message`, `Context`, `Model`, …) from
//! `@earendil-works/pi-ai`; here those map onto the ported [`pidgin_ai`] types,
//! and the handful of shapes pi-ai does not expose (`Tool`, `TextContent`,
//! `ImageContent`, `SimpleStreamOptions`) are mirrored locally.
//!
//! # Streaming adaptation
//!
//! pi's agent loop consumes an async `AssistantMessageEventStream` (an
//! async-iterable produced by `StreamFn`). pidgin-ai re-presents streaming
//! **eagerly**: [`pidgin_ai::seams::Provider::stream`] returns a
//! [`StreamResult`] (`{ events: Vec<AssistantMessageEvent>, message:
//! AssistantMessage }`) with no async/await — timing is reintroduced only at the
//! napi edge. The ported agent loop is therefore synchronous and iterates the
//! event `Vec`. Accordingly:
//!
//! - [`StreamFn`] mirrors [`pidgin_ai::seams::Provider::stream`]'s signature and
//!   returns a [`StreamResult`] directly (no future). An `pidgin-ai` provider
//!   satisfies this shape, exactly as pi's `Models.streamSimple` satisfies its
//!   `StreamFn`.
//! - Every hook that pi types as returning a `Promise<T>` is ported as a
//!   synchronous closure returning `T`. The "must not throw / must not reject"
//!   contracts documented on the pi hooks still apply: implementations return a
//!   safe fallback rather than panicking.
//!
//! # Serde conventions
//!
//! Data-carrying types that cross the wire (serialized by pi's `JSON.stringify`)
//! derive serde with `camelCase` fields and internally-tagged unions, and field
//! declaration order equals pi's object-literal order so the byte layout matches.
//! Types that carry closures ([`AgentTool`], [`AgentLoopConfig`], [`AgentState`],
//! [`AgentContext`], and the hook-context structs) are runtime-only and do not
//! implement serde.
//!
//! Source of truth: `vendor/pi/packages/agent/src/types.ts`.

// straitjacket-allow-file:duplication faithful mirror of pi's `types.ts`; its
// `AgentEvent` union is intentionally redeclared verbatim by the coding-agent
// `AgentSessionEvent` (crates/pidgin-coding/src/core/agent_session/events.rs),
// which pi defines as `Exclude<AgentEvent, {type:"agent_end"}> | …`. This file
// sorts first of that clone pair, so the marker lives here.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pidgin_ai::seams::{AbortSignal, StreamResult};
use pidgin_ai::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, Message, Model,
    ModelThinkingLevel, StreamOptions, ToolResultMessage,
};

// ---------------------------------------------------------------------------
// Streaming function (`types.ts:26-31`)
// ---------------------------------------------------------------------------

/// Stream function used by the agent loop; the eager-model analog of pi's
/// `StreamFn` (`types.ts:26`).
///
/// pi's `StreamFn` returns an async `AssistantMessageEventStream`; in pidgin the
/// provider seam returns a [`StreamResult`] eagerly, so this closure mirrors
/// [`pidgin_ai::seams::Provider::stream`] and returns that result directly. The
/// pi contract holds: request/model/runtime failures must be encoded in the
/// returned result (a terminal `error` event and an `AssistantMessage` with
/// `stopReason` `error`/`aborted` and an `errorMessage`), never surfaced as a
/// panic.
pub type StreamFn = Arc<
    dyn Fn(&Model, &Context, Option<&StreamOptions>, Option<&AbortSignal>) -> StreamResult
        + Send
        + Sync,
>;

/// Optional incremental sibling of [`StreamFn`] used by the agent loop to DRIVE
/// the provider one event at a time.
///
/// [`StreamFn`] hands back an already-materialized [`StreamResult`] (its
/// `events` `Vec` is fully built before the loop iterates it), so a `-p --mode
/// json` turn prints every token at once. This closure instead PULLS the
/// provider incrementally: it drives a borrowed
/// [`AssistantEventReader`](pidgin_ai::utils::sse::AssistantEventReader)
/// internally (the borrow never escapes the closure) and invokes `sink` once per
/// event as it arrives, so downstream subscribers observe real inter-event
/// timing. The returned [`StreamResult`] is the terminal: its `message` is the
/// final [`AssistantMessage`], and its `events` MAY be empty because each event
/// was already delivered through `sink`.
///
/// Mirrors [`StreamFn`]'s argument shape (`Arc<... + Send + Sync>`) plus the
/// trailing `sink`. The same "must not panic; encode failures as a terminal
/// `error`" contract holds. It is optional: when absent the loop uses the
/// buffered [`StreamFn`] path with unchanged behavior.
pub type IncrementalStreamFn = Arc<
    dyn Fn(
            &Model,
            &Context,
            Option<&StreamOptions>,
            Option<&AbortSignal>,
            &mut dyn FnMut(&AssistantMessageEvent),
        ) -> StreamResult
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Execution / queue modes (`types.ts:39`, `types.ts:48`)
// ---------------------------------------------------------------------------

/// How tool calls from a single assistant message are executed (`types.ts:39`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// Each tool call is prepared, executed, and finalized before the next.
    Sequential,
    /// Tool calls are prepared sequentially, then allowed tools run concurrently.
    Parallel,
}

/// How many queued user messages are injected at a queue drain point
/// (`types.ts:48`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    /// Drain and inject every queued message.
    All,
    /// Drain and inject only the oldest queued message.
    OneAtATime,
}

// ---------------------------------------------------------------------------
// Thinking level (`types.ts:296`)
// ---------------------------------------------------------------------------

/// Thinking/reasoning level for models that support it (`types.ts:296`).
///
/// pi-agent defines its own `ThinkingLevel` = `off | minimal | low | medium |
/// high | xhigh | max`, which is byte-identical to pi-ai's `ModelThinkingLevel`.
/// The port reuses [`pidgin_ai::ModelThinkingLevel`] rather than redefining it.
pub type ThinkingLevel = ModelThinkingLevel;

// ---------------------------------------------------------------------------
// Tool-call content block (`types.ts:51`)
// ---------------------------------------------------------------------------

/// The literal `type` discriminant for an [`AgentToolCall`]. Only accepts
/// `"toolCall"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallType {
    #[default]
    ToolCall,
}

/// A single tool-call content block emitted by an assistant message
/// (`types.ts:51`).
///
/// pi types this as `Extract<AssistantMessage["content"][number], { type:
/// "toolCall" }>` — the `toolCall` narrowing of pi-ai's content-block union.
/// Rust cannot narrow an enum variant to a standalone type, so this is a
/// dedicated struct whose fields and `type` tag reproduce
/// [`pidgin_ai::ContentBlock::ToolCall`] byte-for-byte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCall {
    #[serde(rename = "type")]
    pub kind: ToolCallType,
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool-call hook results (`types.ts:59`, `types.ts:72`)
// ---------------------------------------------------------------------------

/// Result returned from `beforeToolCall` (`types.ts:59`).
///
/// Returning `block: Some(true)` prevents the tool from executing; the loop
/// emits an error tool result instead, with `reason` as its text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BeforeToolCallResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Partial override returned from `afterToolCall` (`types.ts:72`).
///
/// Merge semantics are field-by-field: a `Some` field replaces the corresponding
/// executed-tool-result field in full; a `None` field keeps the original. There
/// is no deep merge for `content` or `details`.
///
/// `content` mirrors pi's `(TextContent | ImageContent)[]` as a
/// `Vec<ContentBlock>`; pi-ai exposes no narrowed content types, and the `text`
/// and `image` variants serialize identically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AfterToolCallResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Hint that the agent should stop after the current tool batch. Early
    /// termination happens only when every finalized result in the batch sets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool results & callbacks (`types.ts:341`, `types.ts:357`)
// ---------------------------------------------------------------------------

/// Final or partial result produced by a tool (`types.ts:341`).
///
/// `details` is pi's generic `T`; the port keeps it opaque as
/// [`serde_json::Value`], matching the harness convention. `content` mirrors
/// pi's `(TextContent | ImageContent)[]` as a `Vec<ContentBlock>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResult {
    pub content: Vec<ContentBlock>,
    pub details: Value,
    /// Names of tools introduced by this result, available from this transcript
    /// point onward.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    /// Hint that the agent should stop after the current tool batch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Callback used by tools to stream partial execution updates (`types.ts:357`).
///
/// Scoped to the current `execute` invocation; calls made after the tool
/// settles are ignored by the loop.
pub type AgentToolUpdateCallback = Arc<dyn Fn(&AgentToolResult) + Send + Sync>;

// ---------------------------------------------------------------------------
// Tool definition (`types.ts:360`)
// ---------------------------------------------------------------------------

/// Optional compatibility shim that rewrites raw tool-call arguments before
/// schema validation (pi's `AgentTool.prepareArguments`, `types.ts:369`).
pub type PrepareArguments = Arc<dyn Fn(&Value) -> Value + Send + Sync>;

/// Executes a tool call (pi's `AgentTool.execute`, `types.ts:371`).
///
/// pi returns `Promise<AgentToolResult>`; the eager port returns it directly.
/// Parameters mirror pi: the tool-call id, the validated arguments (`Static`
/// value → [`Value`]), the optional abort signal, and the optional update
/// callback. pi's "throw on failure instead of encoding errors in `content`"
/// contract is preserved by returning the error out of band at the loop layer.
pub type AgentToolExecute = Arc<
    dyn Fn(&str, &Value, Option<&AbortSignal>, Option<&AgentToolUpdateCallback>) -> AgentToolResult
        + Send
        + Sync,
>;

/// Tool definition used by the agent runtime (`types.ts:360`).
///
/// Extends pi-ai's `Tool` (`name`, `description`, `parameters`) with the agent
/// additions. pi-ai's `Tool` is not yet ported, so its three fields are mirrored
/// inline; `parameters` holds the TypeBox `TSchema` opaquely as [`Value`].
/// Runtime-only (carries closures); not serde.
#[derive(Clone)]
pub struct AgentTool {
    /// Tool name (from pi-ai `Tool`).
    pub name: String,
    /// Tool description (from pi-ai `Tool`).
    pub description: String,
    /// TypeBox parameter schema (from pi-ai `Tool`), kept opaque.
    pub parameters: Value,
    /// Human-readable label for UI display.
    pub label: String,
    /// Optional shim for raw tool-call arguments before schema validation.
    pub prepare_arguments: Option<PrepareArguments>,
    /// Execute the tool call.
    pub execute: AgentToolExecute,
    /// Per-tool execution-mode override; falls back to the loop default.
    pub execution_mode: Option<ToolExecutionMode>,
}

// ---------------------------------------------------------------------------
// Agent messages (`types.ts:322`, `types.ts:335`)
// ---------------------------------------------------------------------------

/// Extension point for custom app messages (pi's `CustomAgentMessages`,
/// `types.ts:322`).
///
/// pi apps extend this empty interface via declaration merging, and the extra
/// variants join [`AgentMessage`]. The port keeps custom messages opaque, so the
/// extension surface collapses to [`serde_json::Value`].
pub type CustomAgentMessages = Value;

/// Union of LLM messages plus custom messages (pi's `AgentMessage`,
/// `types.ts:335`).
///
/// pi's `AgentMessage = Message | CustomAgentMessages[keyof …]`. Because custom
/// messages are app-defined and opaque, the port represents the union as
/// [`serde_json::Value`], matching `harness::types::AgentMessage`. A pi-ai
/// [`Message`] round-trips through this value unchanged.
pub type AgentMessage = Value;

// ---------------------------------------------------------------------------
// Public agent state (`types.ts:340`)
// ---------------------------------------------------------------------------

/// Public agent state (`types.ts:340`).
///
/// In pi `tools`/`messages` are accessor properties that copy on assignment and
/// the streaming fields are read-only; the port exposes plain fields. Runtime
/// state, not serialized — carries [`AgentTool`] closures — so it is not serde.
#[derive(Clone)]
pub struct AgentState {
    /// System prompt sent with each model request.
    pub system_prompt: String,
    /// Active model used for future turns.
    pub model: Model,
    /// Requested reasoning level for future turns.
    pub thinking_level: ThinkingLevel,
    /// Available tools.
    pub tools: Vec<AgentTool>,
    /// Conversation transcript.
    pub messages: Vec<AgentMessage>,
    /// True while the agent is processing a prompt or continuation.
    pub is_streaming: bool,
    /// Partial assistant message for the current streamed response, if any.
    pub streaming_message: Option<AgentMessage>,
    /// Tool-call ids currently executing.
    pub pending_tool_calls: BTreeSet<String>,
    /// Error message from the most recent failed or aborted assistant turn.
    pub error_message: Option<String>,
}

// ---------------------------------------------------------------------------
// Agent context (`types.ts:388`)
// ---------------------------------------------------------------------------

/// Context snapshot passed into the low-level agent loop (`types.ts:388`).
///
/// Runtime-only (carries [`AgentTool`] closures); not serde.
#[derive(Clone)]
pub struct AgentContext {
    /// System prompt included with the request.
    pub system_prompt: String,
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for this run.
    pub tools: Option<Vec<AgentTool>>,
}

// ---------------------------------------------------------------------------
// Hook contexts (`types.ts:88`, `types.ts:106`, `types.ts:125`, `types.ts:143`)
// ---------------------------------------------------------------------------

/// Context passed to `beforeToolCall` (`types.ts:88`). Runtime-only; not serde.
#[derive(Clone)]
pub struct BeforeToolCallContext {
    /// The assistant message that requested the tool call.
    pub assistant_message: AssistantMessage,
    /// The raw tool-call block from `assistant_message.content`.
    pub tool_call: AgentToolCall,
    /// Validated tool arguments for the target tool schema.
    pub args: Value,
    /// Current agent context at the time the tool call is prepared.
    pub context: AgentContext,
}

/// Context passed to `afterToolCall` (`types.ts:106`). Runtime-only; not serde.
#[derive(Clone)]
pub struct AfterToolCallContext {
    /// The assistant message that requested the tool call.
    pub assistant_message: AssistantMessage,
    /// The raw tool-call block from `assistant_message.content`.
    pub tool_call: AgentToolCall,
    /// Validated tool arguments for the target tool schema.
    pub args: Value,
    /// The executed tool result before any `afterToolCall` overrides.
    pub result: AgentToolResult,
    /// Whether the executed tool result is currently treated as an error.
    pub is_error: bool,
    /// Current agent context at the time the tool call is finalized.
    pub context: AgentContext,
}

/// Context passed to `shouldStopAfterTurn` (`types.ts:125`). Runtime-only; not
/// serde.
#[derive(Clone)]
pub struct ShouldStopAfterTurnContext {
    /// The assistant message that completed the turn.
    pub message: AssistantMessage,
    /// Tool-result messages passed to the preceding `turn_end` event.
    pub tool_results: Vec<ToolResultMessage>,
    /// Current agent context after the turn's message and results were appended.
    pub context: AgentContext,
    /// Messages this loop invocation will return if it exits at this point.
    pub new_messages: Vec<AgentMessage>,
}

/// Context passed to `prepareNextTurn` (`types.ts:143`).
///
/// pi declares `interface PrepareNextTurnContext extends
/// ShouldStopAfterTurnContext {}`, so the port aliases it.
pub type PrepareNextTurnContext = ShouldStopAfterTurnContext;

/// Replacement runtime state the loop applies before the next provider request
/// (pi's `AgentLoopTurnUpdate`, `types.ts:136`). Runtime-only; not serde.
#[derive(Clone, Default)]
pub struct AgentLoopTurnUpdate {
    /// Context for the next provider request.
    pub context: Option<AgentContext>,
    /// Model for the next provider request.
    pub model: Option<Model>,
    /// Thinking level for the next provider request.
    pub thinking_level: Option<ThinkingLevel>,
}

// ---------------------------------------------------------------------------
// Agent-loop hook function types (`types.ts:145`)
// ---------------------------------------------------------------------------

/// Converts `AgentMessage[]` to LLM-compatible `Message[]` before each LLM call
/// (pi's `convertToLlm`, `types.ts:172`). Must not panic; return a safe fallback.
pub type ConvertToLlm = Arc<dyn Fn(&[AgentMessage]) -> Vec<Message> + Send + Sync>;

/// Optional transform applied to the context before `convertToLlm`
/// (pi's `transformContext`, `types.ts:191`). Must not panic.
pub type TransformContext =
    Arc<dyn Fn(&[AgentMessage], Option<&AbortSignal>) -> Vec<AgentMessage> + Send + Sync>;

/// Resolves an API key dynamically for each LLM call (pi's `getApiKey`,
/// `types.ts:201`). Returns `None` when no key is available.
pub type GetApiKey = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Called after each turn to request a graceful stop (pi's `shouldStopAfterTurn`,
/// `types.ts:213`). Must not panic.
pub type ShouldStopAfterTurn = Arc<dyn Fn(&ShouldStopAfterTurnContext) -> bool + Send + Sync>;

/// Called after `turn_end` to optionally replace context/model/thinking for the
/// next turn (pi's `prepareNextTurn`, `types.ts:220`). `None` keeps the current
/// state.
pub type PrepareNextTurn =
    Arc<dyn Fn(&PrepareNextTurnContext) -> Option<AgentLoopTurnUpdate> + Send + Sync>;

/// Returns steering messages to inject mid-run (pi's `getSteeringMessages`,
/// `types.ts:235`). Returns an empty vec when none are available.
pub type GetSteeringMessages = Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>;

/// Returns follow-up messages to process after the agent would otherwise stop
/// (pi's `getFollowUpMessages`, `types.ts:247`). Returns an empty vec when none.
pub type GetFollowUpMessages = Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>;

/// Called before a tool executes, after arguments are validated (pi's
/// `beforeToolCall`, `types.ts:265`). Return `Some(result)` with `block:
/// Some(true)` to prevent execution.
///
/// The context is passed by `&mut` so the hook can mutate `ctx.args` in place,
/// faithfully mirroring pi: pi's hook mutates the validated-args object in place
/// and the loop re-reads that same reference for `execute` (see
/// `agent-loop.test.ts` "should execute mutated beforeToolCall args without
/// revalidation"). `BeforeToolCallResult` itself carries only `block`/`reason`;
/// updated arguments flow back through `ctx.args`, not the return value.
pub type BeforeToolCall = Arc<
    dyn Fn(&mut BeforeToolCallContext, Option<&AbortSignal>) -> Option<BeforeToolCallResult>
        + Send
        + Sync,
>;

/// Called after a tool finishes, before `tool_execution_end` and result events
/// (pi's `afterToolCall`, `types.ts:277`). Return `Some(override)` to override
/// parts of the executed result.
pub type AfterToolCall = Arc<
    dyn Fn(&AfterToolCallContext, Option<&AbortSignal>) -> Option<AfterToolCallResult>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Agent-loop configuration (`types.ts:145`)
// ---------------------------------------------------------------------------

/// Configuration for the low-level agent loop (pi's `AgentLoopConfig`,
/// `types.ts:145`).
///
/// pi's `AgentLoopConfig extends SimpleStreamOptions`. pidgin-ai ports a subset
/// of `SimpleStreamOptions` as [`StreamOptions`] (`sessionId`, `cacheRetention`);
/// its `reasoning` field is surfaced here as [`reasoning`](Self::reasoning), and
/// its `maxRetryDelayMs` rides on [`stream_options`](Self::stream_options)
/// ([`StreamOptions::max_retry_delay_ms`](pidgin_ai::StreamOptions)). The
/// remaining pi stream-option fields (temperature, maxTokens, signal, headers,
/// timeout tuning, transport, callbacks, metadata, env, thinkingBudgets) are
/// additive future work. Runtime-only (carries closures); not serde.
#[derive(Clone)]
pub struct AgentLoopConfig {
    /// Inherited [`SimpleStreamOptions`](pidgin_ai::StreamOptions) subset.
    pub stream_options: StreamOptions,
    /// Requested reasoning level (pi's `SimpleStreamOptions.reasoning`).
    pub reasoning: Option<ThinkingLevel>,
    /// Active model for the run.
    pub model: Model,
    /// Converts `AgentMessage[]` to `Message[]` before each LLM call (required).
    pub convert_to_llm: ConvertToLlm,
    /// Optional AgentMessage-level context transform applied before conversion.
    pub transform_context: Option<TransformContext>,
    /// Optional dynamic API-key resolver.
    pub get_api_key: Option<GetApiKey>,
    /// Optional graceful-stop check after each turn.
    pub should_stop_after_turn: Option<ShouldStopAfterTurn>,
    /// Optional next-turn state override.
    pub prepare_next_turn: Option<PrepareNextTurn>,
    /// Optional mid-run steering-message source.
    pub get_steering_messages: Option<GetSteeringMessages>,
    /// Optional follow-up-message source.
    pub get_follow_up_messages: Option<GetFollowUpMessages>,
    /// Tool-execution mode. pi default is `parallel`.
    pub tool_execution: Option<ToolExecutionMode>,
    /// Optional pre-execution tool hook.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Optional post-execution tool hook.
    pub after_tool_call: Option<AfterToolCall>,
}

// ---------------------------------------------------------------------------
// Agent UI events (`types.ts:397`)
// ---------------------------------------------------------------------------

/// Events emitted by the agent for UI updates (pi's `AgentEvent`,
/// `types.ts:397`).
///
/// Internally tagged by `type`; tag strings are snake_case and field names are
/// camelCase, matching pi's discriminated union. `message`/`args`/`result`/
/// `partialResult` are opaque [`Value`]s (pi's `AgentMessage` / `any`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    /// A run started.
    AgentStart,
    /// A run ended; carries the messages produced by the run.
    AgentEnd { messages: Vec<AgentMessage> },
    /// A turn (one assistant response plus any tool calls/results) started.
    TurnStart,
    /// A turn ended.
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    /// A user/assistant/tool-result message started.
    MessageStart { message: AgentMessage },
    /// An assistant message received a streamed update.
    ///
    /// `assistant_message_event` is boxed to keep the enum's variants
    /// size-balanced (the event embeds a full [`AssistantMessage`]); `Box<T>`
    /// serializes transparently, so the wire shape is unchanged.
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: Box<AssistantMessageEvent>,
    },
    /// A message finished.
    MessageEnd { message: AgentMessage },
    /// A tool execution started.
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    /// A tool execution produced a partial update.
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: Value,
    },
    /// A tool execution ended.
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Value,
        is_error: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_execution_mode_serialization() {
        assert_eq!(
            serde_json::to_value(ToolExecutionMode::Sequential).unwrap(),
            json!("sequential")
        );
        assert_eq!(
            serde_json::to_value(ToolExecutionMode::Parallel).unwrap(),
            json!("parallel")
        );
    }

    #[test]
    fn queue_mode_serialization() {
        assert_eq!(serde_json::to_value(QueueMode::All).unwrap(), json!("all"));
        assert_eq!(
            serde_json::to_value(QueueMode::OneAtATime).unwrap(),
            json!("one-at-a-time")
        );
    }

    #[test]
    fn thinking_level_reuses_model_thinking_level() {
        // pi-agent's ThinkingLevel includes "off"; it aliases pi-ai's
        // ModelThinkingLevel.
        let level: ThinkingLevel = ModelThinkingLevel::Off;
        assert_eq!(serde_json::to_value(level).unwrap(), json!("off"));
        assert_eq!(
            serde_json::to_value(ModelThinkingLevel::Xhigh).unwrap(),
            json!("xhigh")
        );
    }

    #[test]
    fn agent_tool_call_matches_content_block_wire_shape() {
        let call = AgentToolCall {
            kind: ToolCallType::default(),
            id: "call_1".into(),
            name: "read".into(),
            arguments: json!({ "path": "a.txt" }),
            thought_signature: None,
        };
        let value = serde_json::to_value(&call).unwrap();
        assert_eq!(
            value,
            json!({
                "type": "toolCall",
                "id": "call_1",
                "name": "read",
                "arguments": { "path": "a.txt" }
            })
        );

        // Round-trips through pi-ai's ContentBlock::ToolCall.
        let block: ContentBlock = serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(block, ContentBlock::ToolCall { .. }));
        assert_eq!(serde_json::to_value(&block).unwrap(), value);
    }

    #[test]
    fn agent_event_lifecycle_wire_shape() {
        assert_eq!(
            serde_json::to_value(AgentEvent::AgentStart).unwrap(),
            json!({ "type": "agent_start" })
        );
        assert_eq!(
            serde_json::to_value(AgentEvent::TurnStart).unwrap(),
            json!({ "type": "turn_start" })
        );

        let event = AgentEvent::ToolExecutionEnd {
            tool_call_id: "call_1".into(),
            tool_name: "read".into(),
            result: json!({ "ok": true }),
            is_error: false,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "tool_execution_end",
                "toolCallId": "call_1",
                "toolName": "read",
                "result": { "ok": true },
                "isError": false
            })
        );
    }

    #[test]
    fn after_tool_call_result_omits_absent_fields() {
        let result = AfterToolCallResult {
            terminate: Some(true),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({ "terminate": true })
        );
    }

    #[test]
    fn agent_tool_result_wire_shape() {
        let result = AgentToolResult {
            content: vec![ContentBlock::Text {
                text: "done".into(),
                text_signature: None,
            }],
            details: json!(null),
            added_tool_names: None,
            terminate: None,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "content": [{ "type": "text", "text": "done" }],
                "details": null
            })
        );
    }
}
