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

/// Per-thinking-level token budgets, for token-based providers only
/// (`types.ts:91`). Every field is an optional token count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medium: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high: Option<u64>,
}

/// Prompt-cache retention preference (`types.ts:99`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

/// Preferred transport for providers that support multiple transports
/// (`types.ts:101`). Providers that do not support a given transport ignore it.
///
/// Modeled as an enum, mirroring the other provider-boundary string unions
/// (`StopReason`, `CacheRetention`). Not yet wired into [`StreamOptions`]: the
/// per-request `transport` field is deferred to the providers lane (see the
/// port-deferral note on [`StreamOptions`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Sse,
    Websocket,
    WebsocketCached,
    Auto,
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

/// The phase a [`TextSignatureV1`] applies to (`types.ts:324`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSignaturePhase {
    Commentary,
    FinalAnswer,
}

/// The structured form of a `textSignature` payload (`types.ts:321`).
///
/// When a provider's text-block signature is not a legacy id string it is the
/// JSON encoding of this shape (see [`ContentBlock::Text::text_signature`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSignatureV1 {
    /// Schema version. pi types this as the literal `1`; kept as a plain integer
    /// because the port avoids a `serde_repr` dependency for a lone numeric
    /// literal.
    pub v: u8,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextSignaturePhase>,
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

/// A tool a model may call (`types.ts:444`).
///
/// pi's `Tool` is generic over a TypeBox `TSchema` for `parameters`; the port
/// keeps `parameters` as opaque [`serde_json::Value`] (the JSON-Schema document)
/// since TypeBox has no faithful Rust analog and the schema round-trips verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// The conversation a provider is asked to continue (`types.ts:450`).
///
/// `tools` is kept as opaque JSON (pi's `Tool[]`) so it round-trips verbatim and
/// serializes exactly as pi's `JSON.stringify(context.tools)` for the faux
/// provider's prompt accounting. The faithful [`Tool`] type now exists, but
/// migrating this field to `Option<Vec<Tool>>` reshapes [`Context`] (a
/// serialization-affecting change other lanes depend on) and is deferred to a
/// follow-up so this PR stays purely additive.
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
/// The session/cache fields the faux provider uses for prompt-cache accounting,
/// the request-auth fields (`apiKey`, `headers`, `env`) that `Models`'s
/// `applyAuth` (`models.ts:463`) threads into the provider request, and the
/// plain-data request tuning fields (`temperature`, `maxTokens`, `timeoutMs`,
/// `websocketConnectTimeoutMs`, `maxRetries`, `maxRetryDelayMs`, `metadata`).
/// Every field is optional and skips serialization when absent, so the wire
/// shape stays a strict subset of pi's. The `transport`, callback (`onPayload`,
/// `onResponse`) and `signal` fields are deliberately not ported here — see the
/// port-deferral note below the struct.
///
/// # Port additions / deviations
///
/// - `headers` mirrors pi's `StreamOptions.headers`, but pi types it as
///   `ProviderHeaders` (`Record<string, string | null>`, where a `null` value
///   suppresses a provider default header). The Rust seam carries plain
///   `string` values only; suppression via `null` is not representable here. The
///   env-API-key / ambient auth path this crate resolves never yields a
///   suppressing `null`, so the collapse `applyAuth` performs is lossless in
///   practice (documented on [`crate::providers::Models::stream`]).
/// - `base_url` has no pi `StreamOptions` counterpart. It is the seam-level
///   carrier a directly-constructed backend (e.g. the Anthropic messages
///   backend) reads to target a host without an auth context; `applyAuth`
///   threads a per-credential base URL onto the request *model* instead (pi's
///   `requestModel = auth.baseUrl ? {...model, baseUrl} : model`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct StreamOptions {
    /// Session identifier enabling session-scoped prompt caching (`types.ts:132`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Prompt-cache retention preference; `none` disables caching (`types.ts:127`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    /// The provider credential to send with the request (pi's
    /// `StreamOptions.apiKey`, `types.ts:117`). Resolved and threaded by
    /// `applyAuth`; wins over the provider's stored/ambient key per-field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Caller-supplied request headers, merged over the resolved auth headers
    /// (pi's `StreamOptions.headers`, `types.ts:152`). See the type-level note on
    /// the `string`-only value deviation from pi's `ProviderHeaders`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// A per-request base-URL override for the seam (a Rust-port addition; see
    /// the type-level note). `None` leaves the request model's base URL intact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Provider-scoped environment values, taking precedence over `process.env`
    /// for provider configuration (pi's `StreamOptions.env`, `types.ts:270`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Sampling temperature (pi's `StreamOptions.temperature`, `types.ts:114`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Maximum output tokens (pi's `StreamOptions.maxTokens`, `types.ts:115`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// HTTP request timeout in milliseconds, for providers/SDKs that support it
    /// (pi's `StreamOptions.timeoutMs`, `types.ts:157`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// WebSocket connect-handshake timeout in milliseconds, for providers with
    /// WebSocket transports (pi's `StreamOptions.websocketConnectTimeoutMs`,
    /// `types.ts:163`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_connect_timeout_ms: Option<u64>,
    /// Maximum client-side retry attempts (pi's `StreamOptions.maxRetries`,
    /// `types.ts:168`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Cap in milliseconds on a server-requested retry delay; `0` disables the
    /// cap (pi's `StreamOptions.maxRetryDelayMs`, `types.ts:176`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    /// Request metadata; providers extract the fields they understand (pi's
    /// `StreamOptions.metadata`, `types.ts:182`). pi types the values as
    /// `unknown`, kept as opaque [`serde_json::Value`] here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
}

// PORT DEFERRAL (`types.ts` StreamOptions / provider seam):
// The following pi symbols are intentionally NOT ported as data types here.
//   - `transport` (`StreamOptions.transport`, types.ts:122): owned by the
//     providers/* lane, which injects the HTTP transport and centralizes
//     per-request transport-override precedence. The `Transport` string union
//     itself is ported above; only the StreamOptions field is deferred to that
//     lane. (Field itself deferred.)
//   - `signal` (`AbortSignal`, types.ts:116): no synchronous-Rust analog; ties
//     to the not-yet-ported `abort-signals.ts` cancellation surface.
//   - `onPayload` / `onResponse` callbacks (types.ts:138, 143): function-type
//     aliases, not data; represented by the `seams/provider.rs` trait seam.
//   - `ProviderResponse` (types.ts:108): the callback argument shape, deferred
//     with the callbacks it serves.
//   - Function/callback aliases `ProviderResponse`, `ProviderStreams`,
//     `StreamFunction` (types.ts:108-244, 309-319): behavior contracts modeled
//     by the provider trait seams, not by serializable data. (The image-side
//     `ProviderImages` / `ImagesFunction` are ported below the image-generation
//     types — see [`ProviderImages`] and [`ImagesFunction`].)
//   - Mapped/conditional type aliases `ApiOptionsMap`, `ApiStreamOptions`,
//     `ProviderStreamOptions` (types.ts:191-217): TypeScript mapped/conditional
//     types keyed by API string with no faithful Rust analog; the per-provider
//     option seam already carries this distinction structurally.

/// [`StreamOptions`] plus the unified reasoning controls passed to pi's
/// `streamSimple()` / `completeSimple()` (`types.ts:295`).
///
/// pi expresses this as `interface SimpleStreamOptions extends StreamOptions`;
/// Rust has no struct inheritance, so the base options are embedded via
/// `#[serde(flatten)]` — the JSON stays a flat object identical to pi's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SimpleStreamOptions {
    /// The shared [`StreamOptions`] fields, flattened into this object.
    #[serde(flatten)]
    pub base: StreamOptions,
    /// Requested reasoning/thinking level (`types.ts:296`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ThinkingLevel>,
    /// Custom token budgets per thinking level, for token-based providers only
    /// (`types.ts:298`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<ThinkingBudgets>,
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

/// The pi-controlled `$var` sentinel used inside `chat_template_kwargs`
/// (`types.ts:85`). When a kwarg value is this object, pi substitutes the live
/// thinking state at request time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatTemplateVar {
    #[serde(rename = "thinking.enabled")]
    ThinkingEnabled,
    #[serde(rename = "thinking.effort")]
    ThinkingEffort,
}

/// The object form of a [`ChatTemplateKwargValue`] (`types.ts:85`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTemplateKwargVar {
    #[serde(rename = "$var")]
    pub var: ChatTemplateVar,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omit_when_off: Option<bool>,
}

/// A single `chat_template_kwargs` value (`types.ts:80`).
///
/// pi's union is `string | number | boolean | null | { $var, omitWhenOff? }`.
/// The `$var` sentinel object is ported as the typed [`ChatTemplateKwargVar`];
/// the scalar arms (`string | number | boolean | null`) are kept as opaque
/// [`serde_json::Value`] so integer/float/null distinctions round-trip verbatim
/// without a `serde_repr` dependency. Deserialization tries the sentinel object
/// first, then falls back to the scalar.
///
/// This type is not yet wired into [`OpenAICompletionsCompat::chat_template_kwargs`]
/// (still `BTreeMap<String, Value>`): that field is read by the openai-completions
/// compat-detection path in the api lane, and retyping it would reshape that
/// shared surface. Migrating the field is deferred to keep this PR additive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatTemplateKwargValue {
    /// The pi-controlled `{ "$var": ... }` sentinel object.
    Var(ChatTemplateKwargVar),
    /// A JSON scalar (`string | number | boolean | null`), kept wire-exact.
    Scalar(Value),
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

// ---------------------------------------------------------------------------
// Image generation (`types.ts:30-33`, `types.ts:73-75`, `types.ts:421-440`,
// `types.ts:733-738`)
// ---------------------------------------------------------------------------

/// pi's `ImagesApi` string union (`KnownImagesApi | (string & {})`,
/// `types.ts:32`; known value: `"openrouter-images"`). Modeled as a plain
/// `String`, matching how the core `Api`/`ProviderId` unions are inlined.
pub type ImagesApi = String;

/// pi's `ImagesProviderId` string union (`KnownImagesProvider | string`,
/// `types.ts:75`; known value: `"openrouter"`). Modeled as a plain `String`,
/// matching how the core `Api`/`ProviderId` unions are inlined.
pub type ImagesProviderId = String;

/// A content block accepted as image-generation input (`ImagesInputContent`,
/// `types.ts:421`): pi's `TextContent | ImageContent`.
///
/// A closed two-variant projection of [`ContentBlock`] (text and image only),
/// internally tagged by `type` with the same wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ImagesInputContent {
    /// `types.ts:327` — plain text, optionally carrying a provider signature.
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    /// `types.ts:343` — a base64-encoded image with its MIME type.
    Image { data: String, mime_type: String },
}

/// A content block produced as image-generation output (`ImagesOutputContent`,
/// `types.ts:422`). Identical union to [`ImagesInputContent`] in pi.
pub type ImagesOutputContent = ImagesInputContent;

/// The input handed to an image-generation call (`types.ts:424`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImagesContext {
    pub input: Vec<ImagesInputContent>,
}

/// Why an image-generation call stopped (`types.ts:428`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImagesStopReason {
    Stop,
    Error,
    Aborted,
}

/// The result of an image-generation call (`types.ts:430`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantImages {
    pub api: ImagesApi,
    pub provider: ImagesProviderId,
    pub model: String,
    pub output: Vec<ImagesOutputContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub stop_reason: ImagesStopReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: i64,
}

/// An image-generation model (`types.ts:733`).
///
/// pi derives this as `Omit<Model, "api" | "provider" | "reasoning" |
/// "contextWindow" | "maxTokens" | "compat">` plus image-specific `api`,
/// `provider`, and `output`. The dropped fields are transcribed out here rather
/// than reusing [`Model`], matching pi's structural `Omit`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagesModel {
    pub id: String,
    pub name: String,
    pub api: ImagesApi,
    pub provider: ImagesProviderId,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<Modality>,
    pub cost: ModelCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    pub output: Vec<Modality>,
}

/// Per-request controls for an image-generation call (`types.ts:246`).
///
/// The plain-data subset of pi's `ImagesOptions`; every field is optional and
/// skips serialization when absent. The `signal`, `onPayload`, and `onResponse`
/// members are function/`AbortSignal` types with no serializable analog and are
/// deferred (see the StreamOptions port-deferral note above).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ImagesOptions {
    /// The provider credential to send with the request (`types.ts:248`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Provider-scoped environment values, taking precedence over `process.env`
    /// (`types.ts:253`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Custom request headers merged over provider defaults (`types.ts:268`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// HTTP request timeout in milliseconds (`types.ts:272`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Maximum client-side retry attempts (`types.ts:276`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Cap in milliseconds on a server-requested retry delay; `0` disables the
    /// cap (`types.ts:284`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    /// Request metadata; providers extract the fields they understand
    /// (`types.ts:289`). pi types the values as `unknown`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
}

/// pi's known image-generation api discriminant (`KnownImagesApi`,
/// `types.ts:32`). The single value the [`ImagesApi`] string union carries today.
pub const KNOWN_IMAGES_API: &str = "openrouter-images";

/// pi's known image-generation provider id (`KnownImagesProvider`,
/// `types.ts:73`). The single value the [`ImagesProviderId`] union carries today.
pub const KNOWN_IMAGES_PROVIDER: &str = "openrouter";

/// The uniform contract of an image-generation API implementation module
/// (`ProviderImages`, `types.ts:238`).
///
/// Every image API module under `api/` exports exactly one image-generation
/// entry point, so the module itself satisfies this interface; the lazy wrappers
/// and image-provider factories pass these around as values. pi types the sole
/// method as an async function returning `Promise<AssistantImages>`; the sync
/// port returns [`AssistantImages`] directly and threads network I/O through the
/// injected [`HttpTransport`](crate::seams::http::HttpTransport) inside the
/// concrete implementation.
///
/// # Port note — `signal`
///
/// pi carries the per-request `AbortSignal` inside `ImagesOptions.signal`. The
/// serializable [`ImagesOptions`] port defers that field (see the deferral note
/// above), so — exactly as the chat [`StreamOptions`] defers `signal` and the
/// [`Provider::stream`](crate::seams::provider::Provider::stream) seam takes it
/// as a separate parameter — this trait carries `signal` as its own parameter
/// and threads it end-to-end through the dispatch layers to the concrete HTTP
/// entry point ([`crate::api::openrouter_images::generate_images`]).
pub trait ProviderImages: Send + Sync {
    /// Generate images for `model` from `context`, applying the plain-data
    /// `options` and honoring `signal` for cooperative abort. Must not throw:
    /// request/model/runtime failures are encoded in the returned
    /// [`AssistantImages`] with a `stopReason` of `error`/`aborted`.
    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
        signal: Option<&crate::seams::provider::AbortSignal>,
    ) -> AssistantImages;
}

/// pi's `ImagesFunction` value-level contract (`types.ts:315`): the callable an
/// image API module exports and the image-provider factories pass around.
///
/// pi expresses it as `type ImagesFunction = (model, context, options?) =>
/// Promise<AssistantImages>`. The Rust analog is a shareable boxed closure of the
/// same shape; the runtime dispatch prefers the [`ProviderImages`] trait object,
/// so this alias is the faithful value-level mirror for callers that hold the
/// bare function.
pub type ImagesFunction = std::sync::Arc<
    dyn Fn(&ImagesModel, &ImagesContext, Option<&ImagesOptions>) -> AssistantImages + Send + Sync,
>;
