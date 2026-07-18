// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `types.ts`: the per-provider compat structs are walls of near-identical
// optional fields, and every content/message struct shares the same
// skip-serializing serde attribute shape. The clone detector reads these as
// duplicates; they are distinct, load-bearing boundary declarations kept
// verbatim to mirror the upstream wire format exactly.
//! Boundary types ported from pi-ai's `packages/ai/src/types.ts`.
//!
//! This is the provider-agnostic core surface every wire dialect converges on:
//! content blocks, messages, the streaming event union, usage/cost accounting,
//! and the model catalog shape (including the per-provider `compat` map). The
//! JSON wire format mirrors pi exactly — field names stay camelCase via serde
//! rename, discriminated unions become internally-tagged enums, and every
//! provider-boundary union carries an `Unknown` catch-all so a new upstream
//! block type does not hard-fail a live stream.
//!
//! Source of truth: `vendor/pi/packages/ai/src/types.ts` at pinned commit
//! `3da591ab`. Line citations below point into that file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Thinking-effort levels a caller can request (`types.ts:77`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// A model's thinking level, extending [`ThinkingLevel`] with `off` (`types.ts:78`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Maps model thinking levels to provider-specific values; `null` marks a level
/// unsupported, so the value is `Option<String>` (`types.ts:79`).
pub type ThinkingLevelMap = BTreeMap<ModelThinkingLevel, Option<String>>;

/// Prompt-cache retention preference (`types.ts:99`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

/// Input/output modality of a model (`types.ts:718`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

// ---------------------------------------------------------------------------
// Content blocks (`types.ts:327-355`)
// ---------------------------------------------------------------------------

/// A single block of assistant/user/tool content.
///
/// Internally tagged by `type`, mirroring pi's discriminated union. The
/// [`ContentBlock::Unknown`] catch-all absorbs any tag we do not model yet so a
/// new provider block type does not break deserialization at the boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContentBlock {
    /// `types.ts:327` — plain text, optionally carrying a provider signature.
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    /// `types.ts:333` — a reasoning/thinking block.
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    /// `types.ts:343` — a base64-encoded image with its MIME type.
    Image { data: String, mime_type: String },
    /// `types.ts:349` — a tool invocation. `arguments` stays an opaque JSON
    /// value because it is provider-shaped and repaired incrementally mid-stream.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Catch-all for provider block types not modelled here.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Usage & cost (`types.ts:357-378`)
// ---------------------------------------------------------------------------

/// The cost breakdown attached to [`Usage`] (`types.ts:371-377`). All values are
/// US dollars.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

/// Token and cost accounting for a single assistant turn (`types.ts:357`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Subset of `cache_write` written with 1h retention. Anthropic-only.
    #[serde(rename = "cacheWrite1h", skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    /// Reasoning tokens, when reported. A subset of `output`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: UsageCost,
}

/// Why a stream stopped (`types.ts:380`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

// ---------------------------------------------------------------------------
// Messages (`types.ts:382-419`)
// ---------------------------------------------------------------------------

/// A user message's content: either a bare string or a block list (`types.ts:384`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// The literal `role` discriminant for a [`UserMessage`]. Only accepts `"user"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum UserRole {
    #[default]
    User,
}

/// The literal `role` discriminant for an [`AssistantMessage`]. Only accepts
/// `"assistant"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AssistantRole {
    #[default]
    Assistant,
}

/// The literal `role` discriminant for a [`ToolResultMessage`]. Only accepts
/// `"toolResult"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ToolResultRole {
    #[default]
    ToolResult,
}

/// A user-authored message (`types.ts:382`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub role: UserRole,
    pub content: UserContent,
    pub timestamp: i64,
}

/// A tool-result message returned to the model (`types.ts:403`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub role: ToolResultRole,
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Names from `Context.tools` that became available after this result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: bool,
    pub timestamp: i64,
}

/// A message in a conversation (`types.ts:419`).
///
/// Untagged: each variant struct carries a strict `role` marker enum that
/// rejects the other roles, so the marker acts as the effective discriminant
/// while [`AssistantMessage`] keeps its own `role` field for reuse inside stream
/// events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

/// The full assistant message (`types.ts:388`). Also carried as the terminal
/// payload of `done`/`error` stream events and the `partial` accumulator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub role: AssistantRole,
    pub content: Vec<ContentBlock>,
    pub api: String,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Redacted provider/runtime diagnostics. Kept opaque in Stage 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<Value>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// Request context & stream options (`types.ts:450`, `types.ts:113`)
// ---------------------------------------------------------------------------

/// The conversation a provider is asked to continue (`types.ts:450`).
///
/// `tools` is kept as opaque JSON (pi's `Tool[]`, a TypeBox-schema-carrying
/// shape not yet ported) so it round-trips verbatim and serializes exactly as
/// pi's `JSON.stringify(context.tools)` for the faux provider's prompt
/// accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
}

/// Per-request stream controls (`types.ts:113`).
///
/// This is the subset of pi's `StreamOptions` the ported seams read today: the
/// session/cache fields the faux provider uses for its prompt-cache accounting.
/// The remaining pi fields (temperature, maxTokens, transport, callbacks,
/// headers, retry/timeout tuning, metadata, env) are additive future work; every
/// field here is optional and skips serialization when absent so the wire shape
/// stays a strict subset of pi's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StreamOptions {
    /// Session identifier enabling session-scoped prompt caching (`types.ts:132`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Prompt-cache retention preference; `none` disables caching (`types.ts:127`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
}

// ---------------------------------------------------------------------------
// Streaming event union (`types.ts:464-476`)
// ---------------------------------------------------------------------------

/// The uniform streaming event every provider driver converges on.
///
/// Internally tagged by `type`. Non-terminal events carry `partial`, the
/// accumulating [`AssistantMessage`]. Terminal events are `done` (success) or
/// `error` — per pi's contract, failures after stream start are encoded as an
/// `error` event, never thrown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AssistantMessageEvent {
    Start {
        partial: AssistantMessage,
    },
    TextStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    TextDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    TextEnd {
        content_index: u32,
        content: String,
        partial: AssistantMessage,
    },
    ThinkingStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    ThinkingDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    ThinkingEnd {
        content_index: u32,
        content: String,
        partial: AssistantMessage,
    },
    ToolcallStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    ToolcallDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    ToolcallEnd {
        content_index: u32,
        tool_call: ContentBlock,
        partial: AssistantMessage,
    },
    /// Terminal success. `reason` is one of `stop | length | toolUse`.
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    /// Terminal failure. `reason` is one of `aborted | error`.
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

// ---------------------------------------------------------------------------
// Model cost & catalog (`types.ts:688-731`)
// ---------------------------------------------------------------------------

/// Per-token pricing, in US dollars per million tokens (`types.ts:688`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// A pricing tier keyed by an input-token threshold (`types.ts:695`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    /// Applies to requests whose total input usage exceeds this token count.
    pub input_tokens_above: u64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// A model's full pricing, base rates plus optional request-wide tiers
/// (`types.ts:700`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

impl ModelCost {
    /// The base rates as a [`ModelCostRates`] view.
    pub fn base_rates(&self) -> ModelCostRates {
        ModelCostRates {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        }
    }
}

impl ModelCostTier {
    /// This tier's rates as a [`ModelCostRates`] view.
    pub fn rates(&self) -> ModelCostRates {
        ModelCostRates {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
        }
    }
}

/// A model in the unified catalog (`types.ts:706`).
///
/// Generic over the `compat` shape: in pi the compat map's type is selected by
/// the model's `api` (Anthropic/OpenAI-completions/OpenAI-responses). Callers
/// pick the concrete compat struct (e.g. [`AnthropicMessagesCompat`]); the
/// default [`serde_json::Value`] keeps an untyped view available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model<C = Value> {
    pub id: String,
    pub name: String,
    pub api: String,
    pub provider: String,
    pub base_url: String,
    pub reasoning: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<Modality>,
    pub cost: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<C>,
}

// ---------------------------------------------------------------------------
// Per-provider compatibility maps (`types.ts:482-599`)
// ---------------------------------------------------------------------------

/// Session-affinity header format (`types.ts:106`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionAffinityFormat {
    Openai,
    OpenaiNosession,
    Openrouter,
}

/// Which field carries the max-tokens value (`types.ts:492`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    MaxCompletionTokens,
    MaxTokens,
}

/// Reasoning/thinking parameter dialect (`types.ts:502`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingFormat {
    Openai,
    Openrouter,
    Deepseek,
    Together,
    Zai,
    Qwen,
    ChatTemplate,
    QwenChatTemplate,
    StringThinking,
    AntLing,
}

/// Cache-control convention for prompt caching (`types.ts:524`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheControlFormat {
    Anthropic,
}

/// Provider-specific deferred tool serialization mode (`types.ts:528`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeferredToolsMode {
    Kimi,
}

/// OpenRouter provider-routing preferences (`types.ts:607`).
///
/// The genuinely irregular union subfields (`sort`, `max_price`,
/// `preferred_min_throughput`, `preferred_max_latency`) are kept as opaque
/// [`serde_json::Value`] — each is a `string | number | object` union with no
/// clean Rust shape and no Stage-1 consumer. They round-trip verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OpenRouterRouting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce_distillable_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_price: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_min_throughput: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_max_latency: Option<Value>,
}

/// Vercel AI Gateway routing preferences (`types.ts:681`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct VercelGatewayRouting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
}

/// Compat settings for OpenAI-compatible completions APIs (`types.ts:482`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OpenAICompletionsCompat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<BTreeMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<OpenRouterRouting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<VercelGatewayRouting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<CacheControlFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
}

/// Compat settings for OpenAI Responses APIs (`types.ts:536`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OpenAIResponsesCompat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
}

/// Compat settings for Anthropic Messages-compatible APIs (`types.ts:548`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicMessagesCompat {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_eager_tool_input_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_cache_control_on_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_adaptive_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_empty_signature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tool_references: Option<bool>,
}
