//! The faux provider: a scripted, deterministic provider ported from pi-ai's
//! `packages/ai/src/providers/faux.ts` at pinned commit `3da591ab`.
//!
//! pi's tests fake an LLM with `registerFauxProvider()`: a test queues canned
//! assistant messages, then drives the *real* streaming path, which simulates
//! streaming deltas plus token and cache accounting. This is the friendliest
//! injection seam for the bridge and the primary mechanism behind the agent and
//! coding-agent suites. This module reproduces it against the
//! [`Provider`](crate::seams::provider::Provider) seam.
//!
//! # Byte-compatibility with pi
//!
//! The emitted [`AssistantMessageEvent`] sequence and final [`AssistantMessage`]
//! are byte-for-byte pi's:
//!
//! - Token estimation is `ceil(len / 4)` over UTF-16 code units, exactly pi's
//!   `estimateTokens` (`faux.ts:140-142`).
//! - Prompt serialization (`serializeContext`, `faux.ts:190-202`) and the
//!   assistant/user/tool text extractors (`faux.ts:148-188`) are reproduced so
//!   the input/output token counts match.
//! - Session prompt caching (`withUsageEstimate`, `faux.ts:213-251`) uses the
//!   common-prefix accounting over UTF-16 units.
//! - Each event's `partial` reflects its block's final accumulated state. pi
//!   achieves this incidentally: `streamWithDeltas` (`faux.ts:308-401`) mutates
//!   shared block objects in place and the events are serialized only after the
//!   stream drains, so every captured `partial` sees the mutated-to-final block.
//!   The eager port builds the final blocks first and sets each event's `partial`
//!   to the finalized prefix `blocks[0..=i]`, which is the same observable output.
//!
//! # Determinism
//!
//! pi splits each block into deltas of a random size in
//! `[minTokenSize, maxTokenSize]` (`splitStringByTokenSize`, `faux.ts:253-263`).
//! The chunk *content* concatenates to the same block regardless of the split, so
//! the accumulated blocks and final message never depend on the RNG; only the
//! number and size of delta events do. The port picks the minimum token size,
//! making the delta chunking deterministic. When pi is configured with
//! `min == max` (the conformance configuration — see
//! `conformance/gen-ai-fixtures.ts`, which uses `tokenSize: { min: 4096, max: 4096 }`),
//! the port's output is byte-identical to pi's, deltas included. `tokensPerSecond`
//! only introduces inter-chunk delay in pi; timing lives at the binding boundary,
//! so it does not affect the event content produced here.

use serde_json::Value;

use crate::seams::clock::{Clock, FakeClock, SystemClock};
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention, ContentBlock, Context,
    Message, Modality, Model, ModelCost, StopReason, StreamOptions, Usage, UsageCost,
};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: u64 = 3;
const DEFAULT_MAX_TOKEN_SIZE: u64 = 5;

/// Zero usage, matching pi's `DEFAULT_USAGE` (`faux.ts:28-35`).
fn default_usage() -> Usage {
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

// ---------------------------------------------------------------------------
// Content / message builders (`faux.ts:49-94`)
// ---------------------------------------------------------------------------

/// pi's `fauxText` (`faux.ts:49-51`).
pub fn faux_text(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text {
        text: text.into(),
        text_signature: None,
    }
}

/// pi's `fauxThinking` (`faux.ts:53-55`).
pub fn faux_thinking(thinking: impl Into<String>) -> ContentBlock {
    ContentBlock::Thinking {
        thinking: thinking.into(),
        thinking_signature: None,
        redacted: None,
    }
}

/// pi's `fauxToolCall` (`faux.ts:57-64`). `id` mirrors the `options.id` override;
/// pass `None` to get a generated id (pi's `randomId("tool")`).
pub fn faux_tool_call(
    name: impl Into<String>,
    arguments: Value,
    id: Option<String>,
) -> ContentBlock {
    ContentBlock::ToolCall {
        id: id.unwrap_or_else(|| random_id("tool")),
        name: name.into(),
        arguments,
        thought_signature: None,
    }
}

/// Options for [`faux_assistant_message`], mirroring pi's builder options
/// (`faux.ts:73-80`).
#[derive(Debug, Clone, Default)]
pub struct FauxAssistantOptions {
    /// Terminal stop reason (default `stop`).
    pub stop_reason: Option<StopReason>,
    /// Error message for error/aborted terminals.
    pub error_message: Option<String>,
    /// Response id.
    pub response_id: Option<String>,
    /// Message timestamp (default: the clock's `now`).
    pub timestamp: Option<i64>,
}

/// pi's `fauxAssistantMessage` (`faux.ts:73-94`), taking already-built content
/// blocks. `timestamp` defaults to `now_ms` when unset (pi's `Date.now()`).
pub fn faux_assistant_message(
    content: Vec<ContentBlock>,
    options: FauxAssistantOptions,
    now_ms: i64,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: DEFAULT_API.to_string(),
        provider: DEFAULT_PROVIDER.to_string(),
        model: DEFAULT_MODEL_ID.to_string(),
        response_model: None,
        response_id: options.response_id,
        diagnostics: None,
        usage: default_usage(),
        stop_reason: options.stop_reason.unwrap_or(StopReason::Stop),
        error_message: options.error_message,
        timestamp: options.timestamp.unwrap_or(now_ms),
    }
}

// ---------------------------------------------------------------------------
// Text extraction & token estimation (`faux.ts:140-202`)
// ---------------------------------------------------------------------------

/// JS `String.length`: the count of UTF-16 code units.
fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// pi's `estimateTokens` (`faux.ts:140-142`): `ceil(length / 4)` over UTF-16
/// code units.
fn estimate_tokens(text: &str) -> u64 {
    let len = js_len(text) as u64;
    len.div_ceil(4)
}

/// pi's `randomId` (`faux.ts:144-146`). The deterministic port seeds from the
/// clock plus a monotonic counter; it is *not* byte-identical to pi's
/// `Math.random`-based id (pi's is non-deterministic too), so tests that assert
/// on exact output supply explicit ids, exactly as pi's fixtures do.
fn random_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}:{n:08x}")
}

/// pi's `contentToText` for user content (`faux.ts:148-160`).
fn user_content_to_text(content: &crate::types::UserContent) -> String {
    match content {
        crate::types::UserContent::Text(s) => s.clone(),
        crate::types::UserContent::Blocks(blocks) => blocks
            .iter()
            .map(block_to_user_text)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// One user-content block as text: `text` verbatim, an image as
/// `[image:{mime}:{data.length}]` (`faux.ts:153-158`).
fn block_to_user_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text { text, .. } => text.clone(),
        ContentBlock::Image { data, mime_type } => {
            format!("[image:{}:{}]", mime_type, js_len(data))
        }
        // pi's user content is only Text|Image; any other block contributes
        // nothing to the prompt text.
        _ => String::new(),
    }
}

/// pi's `assistantContentToText` (`faux.ts:162-174`).
fn assistant_content_to_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.clone(),
            ContentBlock::Thinking { thinking, .. } => thinking.clone(),
            ContentBlock::ToolCall {
                name, arguments, ..
            } => format!("{}:{}", name, json_stringify(arguments)),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// pi's `toolResultToText` (`faux.ts:176-178`).
fn tool_result_to_text(message: &crate::types::ToolResultMessage) -> String {
    let mut parts = vec![message.tool_name.clone()];
    for block in &message.content {
        parts.push(block_to_user_text(block));
    }
    parts.join("\n")
}

/// pi's `messageToText` (`faux.ts:180-188`).
fn message_to_text(message: &Message) -> String {
    match message {
        Message::User(m) => user_content_to_text(&m.content),
        Message::Assistant(m) => assistant_content_to_text(&m.content),
        Message::ToolResult(m) => tool_result_to_text(m),
    }
}

/// The role tag pi's `serializeContext` prefixes each message with
/// (`faux.ts:195`) — pi reads `message.role` directly.
fn message_role(message: &Message) -> &'static str {
    match message {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolResult(_) => "toolResult",
    }
}

/// pi's `serializeContext` (`faux.ts:190-202`).
fn serialize_context(context: &Context) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system) = &context.system_prompt {
        if !system.is_empty() {
            parts.push(format!("system:{system}"));
        }
    }
    for message in &context.messages {
        parts.push(format!(
            "{}:{}",
            message_role(message),
            message_to_text(message)
        ));
    }
    if let Some(tools) = &context.tools {
        if !tools.is_empty() {
            parts.push(format!(
                "tools:{}",
                json_stringify(&Value::Array(tools.clone()))
            ));
        }
    }
    parts.join("\n\n")
}

/// Compact JSON exactly as `JSON.stringify` emits for the shapes the faux
/// provider serializes (tool arguments and the tools list). serde_json's compact
/// form matches `JSON.stringify` for numbers, strings, arrays, and objects; key
/// order matches for the single-key argument objects the conformance fixtures use.
fn json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// pi's `commonPrefixLength` (`faux.ts:204-211`), over UTF-16 code units so the
/// derived cache-read/write token counts match pi's `slice` semantics.
fn common_prefix_len_utf16(a: &[u16], b: &[u16]) -> usize {
    let len = a.len().min(b.len());
    let mut i = 0;
    while i < len && a[i] == b[i] {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Usage estimate with prompt caching (`faux.ts:213-251`)
// ---------------------------------------------------------------------------

fn with_usage_estimate(
    message: &mut AssistantMessage,
    context: &Context,
    options: Option<&StreamOptions>,
    prompt_cache: &Mutex<std::collections::BTreeMap<String, String>>,
) {
    let prompt_text = serialize_context(context);
    let prompt_tokens = estimate_tokens(&prompt_text);
    let output_tokens = estimate_tokens(&assistant_content_to_text(&message.content));

    let mut input = prompt_tokens;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;

    let session_id = options.and_then(|o| o.session_id.clone());
    let cache_disabled = options
        .and_then(|o| o.cache_retention)
        .is_some_and(|r| r == CacheRetention::None);

    if let Some(session_id) = session_id.filter(|_| !cache_disabled) {
        let mut cache = prompt_cache.lock().unwrap();
        let cur16: Vec<u16> = prompt_text.encode_utf16().collect();
        if let Some(previous) = cache.get(&session_id) {
            let prev16: Vec<u16> = previous.encode_utf16().collect();
            let cached = common_prefix_len_utf16(&prev16, &cur16) as u64;
            cache_read = cached.div_ceil(4);
            cache_write = (cur16.len() as u64 - cached).div_ceil(4);
            input = prompt_tokens.saturating_sub(cache_read);
        } else {
            cache_write = prompt_tokens;
        }
        cache.insert(session_id, prompt_text);
    }

    message.usage = Usage {
        input,
        output: output_tokens,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output_tokens + cache_read + cache_write,
        cost: UsageCost::default(),
    };
}

// ---------------------------------------------------------------------------
// Delta chunking (`faux.ts:253-263`)
// ---------------------------------------------------------------------------

/// pi's `splitStringByTokenSize` (`faux.ts:253-263`) with the minimum token size
/// selected deterministically (see the module doc on determinism). Splits by
/// UTF-16 code units so chunk boundaries match pi's `slice` on multi-byte text.
fn split_string_by_token_size(
    text: &str,
    min_token_size: u64,
    _max_token_size: u64,
) -> Vec<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let char_size = (min_token_size * 4).max(1) as usize;
    let mut chunks: Vec<String> = Vec::new();
    let mut index = 0;
    while index < units.len() {
        let end = (index + char_size).min(units.len());
        chunks.push(String::from_utf16_lossy(&units[index..end]));
        index = end;
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

// ---------------------------------------------------------------------------
// Error / aborted messages (`faux.ts:277-298`)
// ---------------------------------------------------------------------------

fn clone_message(
    mut message: AssistantMessage,
    api: &str,
    provider: &str,
    model_id: &str,
    now_ms: i64,
) -> AssistantMessage {
    message.api = api.to_string();
    message.provider = provider.to_string();
    message.model = model_id.to_string();
    if message.timestamp == 0 {
        message.timestamp = now_ms;
    }
    message
}

fn create_error_message(
    error: &str,
    api: &str,
    provider: &str,
    model_id: &str,
    now_ms: i64,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: api.to_string(),
        provider: provider.to_string(),
        model: model_id.to_string(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: default_usage(),
        stop_reason: StopReason::Error,
        error_message: Some(error.to_string()),
        timestamp: now_ms,
    }
}

fn create_aborted_message(partial: &AssistantMessage, now_ms: i64) -> AssistantMessage {
    let mut aborted = partial.clone();
    aborted.stop_reason = StopReason::Aborted;
    aborted.error_message = Some("Request was aborted".to_string());
    aborted.timestamp = now_ms;
    aborted
}

// ---------------------------------------------------------------------------
// Eager streamWithDeltas (`faux.ts:308-401`)
// ---------------------------------------------------------------------------

/// Build the event sequence for `message`, mirroring pi's `streamWithDeltas` but
/// producing the events eagerly. Returns the events and the final message the
/// stream resolves to.
fn stream_with_deltas(
    message: AssistantMessage,
    min_token_size: u64,
    max_token_size: u64,
    signal: Option<&AbortSignal>,
    now_ms: i64,
) -> StreamResult {
    let aborted = || signal.is_some_and(AbortSignal::is_aborted);

    // partial carries every message field with content built up to the current block.
    let partial_with = |content: Vec<ContentBlock>| {
        let mut p = message.clone();
        p.content = content;
        p
    };

    if aborted() {
        let aborted_msg = create_aborted_message(&partial_with(Vec::new()), now_ms);
        return StreamResult {
            events: vec![AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted_msg.clone(),
            }],
            message: aborted_msg,
        };
    }

    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    events.push(AssistantMessageEvent::Start {
        partial: partial_with(Vec::new()),
    });

    let final_blocks = message.content.clone();
    for (index, block) in final_blocks.iter().enumerate() {
        if aborted() {
            let aborted_msg =
                create_aborted_message(&partial_with(final_blocks[..index].to_vec()), now_ms);
            events.push(AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted_msg.clone(),
            });
            return StreamResult {
                events,
                message: aborted_msg,
            };
        }

        // Each event during block `index` carries the finalized prefix
        // blocks[0..=index] as its partial content (see module doc).
        let partial = || partial_with(final_blocks[..=index].to_vec());
        let content_index = index as u32;

        match block {
            ContentBlock::Thinking { thinking, .. } => {
                events.push(AssistantMessageEvent::ThinkingStart {
                    content_index,
                    partial: partial(),
                });
                for chunk in split_string_by_token_size(thinking, min_token_size, max_token_size) {
                    events.push(AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta: chunk,
                        partial: partial(),
                    });
                }
                events.push(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content: thinking.clone(),
                    partial: partial(),
                });
            }
            ContentBlock::Text { text, .. } => {
                events.push(AssistantMessageEvent::TextStart {
                    content_index,
                    partial: partial(),
                });
                for chunk in split_string_by_token_size(text, min_token_size, max_token_size) {
                    events.push(AssistantMessageEvent::TextDelta {
                        content_index,
                        delta: chunk,
                        partial: partial(),
                    });
                }
                events.push(AssistantMessageEvent::TextEnd {
                    content_index,
                    content: text.clone(),
                    partial: partial(),
                });
            }
            ContentBlock::ToolCall { arguments, .. } => {
                events.push(AssistantMessageEvent::ToolcallStart {
                    content_index,
                    partial: partial(),
                });
                for chunk in split_string_by_token_size(
                    &json_stringify(arguments),
                    min_token_size,
                    max_token_size,
                ) {
                    events.push(AssistantMessageEvent::ToolcallDelta {
                        content_index,
                        delta: chunk,
                        partial: partial(),
                    });
                }
                events.push(AssistantMessageEvent::ToolcallEnd {
                    content_index,
                    tool_call: block.clone(),
                    partial: partial(),
                });
            }
            // pi's faux content is only text/thinking/toolCall; other blocks emit
            // no events.
            ContentBlock::Image { .. } | ContentBlock::Unknown => {}
        }
    }

    if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
        events.push(AssistantMessageEvent::Error {
            reason: message.stop_reason,
            error: message.clone(),
        });
    } else {
        events.push(AssistantMessageEvent::Done {
            reason: message.stop_reason,
            message: message.clone(),
        });
    }

    StreamResult { events, message }
}

// ---------------------------------------------------------------------------
// Faux model definition & registration options (`faux.ts:37-138`)
// ---------------------------------------------------------------------------

/// pi's `FauxModelDefinition` (`faux.ts:37-45`).
#[derive(Debug, Clone)]
pub struct FauxModelDefinition {
    /// Model id.
    pub id: String,
    /// Display name (defaults to `id`).
    pub name: Option<String>,
    /// Whether the model reasons (default false).
    pub reasoning: Option<bool>,
    /// Input modalities (default `[text, image]`).
    pub input: Option<Vec<Modality>>,
    /// Per-token cost (default all-zero).
    pub cost: Option<ModelCost>,
    /// Context window (default 128000).
    pub context_window: Option<u64>,
    /// Max tokens (default 16384).
    pub max_tokens: Option<u64>,
}

/// pi's `RegisterFauxProviderOptions` (`faux.ts:105-114`). `tokens_per_second`
/// governs only inter-chunk delay (a binding-boundary concern) and is accepted
/// for surface parity.
#[derive(Debug, Clone, Default)]
pub struct RegisterFauxProviderOptions {
    /// Api id (defaults to a generated `faux:<n>`).
    pub api: Option<String>,
    /// Provider id (defaults to `faux`).
    pub provider: Option<String>,
    /// Model catalog (defaults to a single `faux-1`).
    pub models: Option<Vec<FauxModelDefinition>>,
    /// Streaming rate; ignored for event content (see module doc).
    pub tokens_per_second: Option<f64>,
    /// Minimum delta token size (default 3).
    pub token_size_min: Option<u64>,
    /// Maximum delta token size (default 5).
    pub token_size_max: Option<u64>,
}

/// A response factory computed per call — pi's `FauxResponseFactory`
/// (`faux.ts:96-101`). It sees the call context, options, the
/// (already-incremented) call state, and the request model, and returns the
/// message to stream.
pub type FauxResponseFactory = Box<
    dyn Fn(&Context, Option<&StreamOptions>, &FauxState, &Model) -> AssistantMessage + Send + Sync,
>;

/// A queued faux response: a fixed message, or a factory computed per call.
///
/// The `Message` variant carries a full [`AssistantMessage`], which is larger
/// than the boxed factory; the size difference is accepted rather than boxing the
/// common fixed-message case that nearly every pi test queues.
#[allow(clippy::large_enum_variant)]
pub enum FauxResponseStep {
    /// A fixed assistant message.
    Message(AssistantMessage),
    /// A message computed per call.
    Factory(FauxResponseFactory),
}

impl std::fmt::Debug for FauxResponseStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FauxResponseStep::Message(m) => f.debug_tuple("Message").field(m).finish(),
            FauxResponseStep::Factory(_) => f.write_str("Factory(..)"),
        }
    }
}

impl From<AssistantMessage> for FauxResponseStep {
    fn from(message: AssistantMessage) -> Self {
        FauxResponseStep::Message(message)
    }
}

/// pi's mutable `state` object (`faux.ts:413`): the running call count.
#[derive(Debug, Default)]
pub struct FauxState {
    /// Number of `stream` calls so far.
    pub call_count: u64,
}

// ---------------------------------------------------------------------------
// The faux provider
// ---------------------------------------------------------------------------

/// A scripted, deterministic provider — the Rust port of pi's `createFauxCore`
/// (`faux.ts:403-508`). Implements the [`Provider`] seam.
///
/// Queue responses with [`FauxProvider::set_responses`] /
/// [`FauxProvider::append_responses`]; each [`FauxProvider::stream`] call pops the
/// next and replays it through the deterministic delta path. Thread-safe:
/// the response queue, call state, and prompt cache are behind mutexes, so the
/// provider can be driven from the napi threadsafe callback.
pub struct FauxProvider {
    api: String,
    provider: String,
    min_token_size: u64,
    max_token_size: u64,
    models: Vec<Model>,
    pending: Mutex<std::collections::VecDeque<FauxResponseStep>>,
    state: Mutex<FauxState>,
    prompt_cache: Mutex<std::collections::BTreeMap<String, String>>,
    clock: Arc<dyn Clock>,
}

impl FauxProvider {
    /// Construct a faux provider from `options`, using the production
    /// [`SystemClock`] for the `now` reads pi does via `Date.now()`.
    pub fn new(options: RegisterFauxProviderOptions) -> Self {
        Self::with_clock(options, Arc::new(SystemClock::new()))
    }

    /// Construct a faux provider driven by an injected [`Clock`], so tests can
    /// pin every `now` (message timestamps on the error/aborted paths and
    /// generated ids). This is the clock seam in use.
    pub fn with_clock(options: RegisterFauxProviderOptions, clock: Arc<dyn Clock>) -> Self {
        let api = options.api.unwrap_or_else(|| DEFAULT_API.to_string());
        let provider = options
            .provider
            .unwrap_or_else(|| DEFAULT_PROVIDER.to_string());

        // pi: min = max(1, min(min ?? 3, max ?? 5)); max = max(min, max ?? 5).
        let raw_min = options.token_size_min.unwrap_or(DEFAULT_MIN_TOKEN_SIZE);
        let raw_max = options.token_size_max.unwrap_or(DEFAULT_MAX_TOKEN_SIZE);
        let min_token_size = raw_min.min(raw_max).max(1);
        let max_token_size = raw_max.max(min_token_size);

        let definitions = options.models.unwrap_or_else(|| {
            vec![FauxModelDefinition {
                id: DEFAULT_MODEL_ID.to_string(),
                name: Some(DEFAULT_MODEL_NAME.to_string()),
                reasoning: Some(false),
                input: Some(vec![Modality::Text, Modality::Image]),
                cost: Some(zero_cost()),
                context_window: Some(128_000),
                max_tokens: Some(16_384),
            }]
        });

        let models = definitions
            .into_iter()
            .map(|d| Model {
                id: d.id.clone(),
                name: d.name.unwrap_or(d.id),
                api: api.clone(),
                provider: provider.clone(),
                base_url: DEFAULT_BASE_URL.to_string(),
                reasoning: d.reasoning.unwrap_or(false),
                thinking_level_map: None,
                input: d
                    .input
                    .unwrap_or_else(|| vec![Modality::Text, Modality::Image]),
                cost: d.cost.unwrap_or_else(zero_cost),
                context_window: d.context_window.unwrap_or(128_000),
                max_tokens: d.max_tokens.unwrap_or(16_384),
                headers: None,
                compat: None,
            })
            .collect();

        Self {
            api,
            provider,
            min_token_size,
            max_token_size,
            models,
            pending: Mutex::new(std::collections::VecDeque::new()),
            state: Mutex::new(FauxState::default()),
            prompt_cache: Mutex::new(std::collections::BTreeMap::new()),
            clock,
        }
    }

    /// Construct a faux provider driven by a fresh, shared [`FakeClock`],
    /// returning the provider and a clone of that clock. Both handles share the
    /// same interior state, so mutating the returned clock (e.g. `set_now_ms`)
    /// changes the `now` the provider reads. This is the ergonomic entry point
    /// for a binding that wants to inject a settable clock without assembling an
    /// `Arc<dyn Clock>` itself; the clock starts at `0`.
    pub fn with_fake_clock(options: RegisterFauxProviderOptions) -> (Self, FakeClock) {
        let clock = FakeClock::new(0);
        let provider = Self::with_clock(options, Arc::new(clock.clone()));
        (provider, clock)
    }

    /// The provider's model catalog (pi's `core.models`).
    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// pi's `getModel()` — the first model, or the model with `id`.
    pub fn get_model(&self, id: Option<&str>) -> Option<Model> {
        match id {
            None => self.models.first().cloned(),
            Some(id) => self.models.iter().find(|m| m.id == id).cloned(),
        }
    }

    /// Replace the pending response queue (pi's `setResponses`).
    pub fn set_responses(&self, responses: impl IntoIterator<Item = FauxResponseStep>) {
        let mut pending = self.pending.lock().unwrap();
        *pending = responses.into_iter().collect();
    }

    /// Append to the pending response queue (pi's `appendResponses`).
    pub fn append_responses(&self, responses: impl IntoIterator<Item = FauxResponseStep>) {
        let mut pending = self.pending.lock().unwrap();
        pending.extend(responses);
    }

    /// Pending response count (pi's `getPendingResponseCount`).
    pub fn pending_response_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// The current call count (pi's `state.callCount`).
    pub fn call_count(&self) -> u64 {
        self.state.lock().unwrap().call_count
    }

    /// Increment and return the call count (pi's `state.callCount++`), separated
    /// so a binding can bump it before resolving a JS-supplied response factory,
    /// exactly where pi does (`faux.ts:445`).
    pub fn bump_call_count(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.call_count += 1;
        state.call_count
    }

    /// Stream an already-resolved response message: apply pi's `cloneMessage`
    /// (`faux.ts:265-275`) and `withUsageEstimate` (`faux.ts:213-251`), then the
    /// eager delta path. This is the half of pi's `stream()` that runs *after* the
    /// pending step is popped and any factory resolved. The napi binding keeps the
    /// response queue and factory resolution on the JS side (so JS closures work
    /// without a threadsafe callback) and calls this for the deterministic
    /// streaming and cache accounting.
    pub fn stream_resolved(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        resolved: AssistantMessage,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        let now_ms = self.clock.now_ms();
        let mut message = clone_message(resolved, &self.api, &self.provider, &model.id, now_ms);
        with_usage_estimate(&mut message, context, options, &self.prompt_cache);
        stream_with_deltas(
            message,
            self.min_token_size,
            self.max_token_size,
            signal,
            now_ms,
        )
    }

    /// Build the error message pi streams when the response queue is empty
    /// (`faux.ts:451-460`): an `error`-stop message with the usage estimate
    /// applied. Exposed so the binding reproduces the empty-queue path.
    pub fn empty_queue_result(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> StreamResult {
        let now_ms = self.clock.now_ms();
        let mut message = create_error_message(
            "No more faux responses queued",
            &self.api,
            &self.provider,
            &model.id,
            now_ms,
        );
        with_usage_estimate(&mut message, context, options, &self.prompt_cache);
        StreamResult {
            events: vec![AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: message.clone(),
            }],
            message,
        }
    }
}

fn zero_cost() -> ModelCost {
    ModelCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        tiers: None,
    }
}

impl Provider for FauxProvider {
    fn api(&self) -> &str {
        &self.api
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> StreamResult {
        let step = self.pending.lock().unwrap().pop_front();
        let call_count = self.bump_call_count();

        let Some(step) = step else {
            // pi: no queued step -> an error message with usage estimated.
            return self.empty_queue_result(model, context, options);
        };

        let state_snapshot = FauxState { call_count };
        let resolved = match step {
            FauxResponseStep::Message(m) => m,
            FauxResponseStep::Factory(f) => f(context, options, &state_snapshot, model),
        };

        self.stream_resolved(model, context, options, resolved, signal)
    }
}

#[cfg(test)]
mod tests;
