//! Client-side reconstruction of proxied assistant streams, ported from
//! `packages/agent/src/proxy.ts`.
//!
//! # What pi's `proxy.ts` does
//!
//! Apps that route LLM calls through their own server (for auth and provider
//! fan-out) receive a **bandwidth-stripped** SSE stream: the server sends
//! [`ProxyAssistantMessageEvent`]s with the heavy `partial` accumulator removed
//! from every delta event. pi's `streamProxy()` fetches `POST {proxyUrl}/api/stream`,
//! reads the `data: …` SSE lines, `JSON.parse`s each into a
//! `ProxyAssistantMessageEvent`, and feeds it through `processProxyEvent()`, a
//! state machine that rebuilds the full `AssistantMessage` client-side and
//! re-emits the standard [`AssistantMessageEvent`] union (with `partial`
//! restored). The reconstructed events flow out of a `ProxyMessageEventStream`.
//!
//! # Streaming adaptation
//!
//! Per the crate convention (see [`crate::types`]), atilla is
//! synchronous/eager: there is no `tokio`, no async-iterable stream. The network
//! half of pi's `streamProxy()` (`fetch`, `TextDecoder`, the `reader.read()`
//! loop, SSE line-splitting) has no portable analog here and is intentionally
//! **not** reproduced — those are the platform-edge concerns pi keeps in the same
//! function only because JS couples them. What ports faithfully is the pure
//! transform: [`stream_proxy`] consumes an already-decoded sequence of
//! [`ProxyAssistantMessageEvent`]s and returns a [`StreamResult`]
//! (`{ events, message }`) — the same eager shape every atilla provider seam
//! converges on ([`atilla_ai::seams::StreamResult`]). [`ProxyPartial::process_proxy_event`]
//! mirrors pi's `processProxyEvent()` case-for-case.
//!
//! Where pi holds a single mutable `partial` object and hands the *same
//! reference* to every emitted event (relying on immediate async consumption),
//! the eager port snapshots `partial` into each event at emission time — the
//! exact idiom atilla-ai's own wire parsers use (see
//! `atilla_ai::api::anthropic`'s `render_partial`).
//!
//! # The `partialJson` side channel
//!
//! pi stashes an extra `partialJson: string` field on the in-flight `toolCall`
//! block to accumulate streamed argument fragments, then `delete`s it at
//! `toolcall_end`. atilla-ai's [`ContentBlock::ToolCall`] has no such field, so
//! the accumulator lives in a side map keyed by content index and is dropped at
//! `toolcall_end` — behaviourally identical, without polluting the wire type.
//! Argument text is repaired incrementally with
//! [`atilla_ai::utils::json_parse::parse_streaming_json`], atilla-ai's port of
//! pi-ai's `parseStreamingJson` (which pi's `proxy.ts` imports for exactly this).
//!
//! Source of truth: `vendor/pi/packages/agent/src/proxy.ts`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use atilla_ai::seams::clock::Clock;
use atilla_ai::seams::provider::{AbortSignal, StreamResult};
use atilla_ai::utils::json_parse::parse_streaming_json;
use atilla_ai::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ContentBlock, Model, StopReason, Usage,
    UsageCost,
};

// ---------------------------------------------------------------------------
// Proxy event union (`proxy.ts:34-56`)
// ---------------------------------------------------------------------------

/// The bandwidth-stripped events the proxy server emits over SSE
/// (`proxy.ts:34`).
///
/// Mirrors pi's `ProxyAssistantMessageEvent` union verbatim: the server removes
/// the `partial` field from every delta so it does not re-send the growing
/// message on each chunk; [`ProxyPartial::process_proxy_event`] restores it
/// client-side.
///
/// Internally tagged by `type` with snake_case tags (`text_start`,
/// `toolcall_delta`, …) and camelCase fields (`contentIndex`, `toolName`,
/// `contentSignature`, `errorMessage`), matching pi's `JSON.stringify` output so
/// a `data:` payload deserializes byte-for-byte. Unknown tags fall through to
/// [`ProxyAssistantMessageEvent::Unknown`], reproducing pi's `default` arm, which
/// warns and skips (`proxy.ts:361-365`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProxyAssistantMessageEvent {
    /// `{ type: "start" }` — stream opened (`proxy.ts:35`).
    Start,
    /// `{ type: "text_start"; contentIndex }` (`proxy.ts:36`).
    TextStart { content_index: u32 },
    /// `{ type: "text_delta"; contentIndex; delta }` (`proxy.ts:37`).
    TextDelta { content_index: u32, delta: String },
    /// `{ type: "text_end"; contentIndex; contentSignature? }` (`proxy.ts:38`).
    TextEnd {
        content_index: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    /// `{ type: "thinking_start"; contentIndex }` (`proxy.ts:39`).
    ThinkingStart { content_index: u32 },
    /// `{ type: "thinking_delta"; contentIndex; delta }` (`proxy.ts:40`).
    ThinkingDelta { content_index: u32, delta: String },
    /// `{ type: "thinking_end"; contentIndex; contentSignature? }` (`proxy.ts:41`).
    ThinkingEnd {
        content_index: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    /// `{ type: "toolcall_start"; contentIndex; id; toolName }` (`proxy.ts:42`).
    ToolcallStart {
        content_index: u32,
        id: String,
        tool_name: String,
    },
    /// `{ type: "toolcall_delta"; contentIndex; delta }` (`proxy.ts:43`).
    ToolcallDelta { content_index: u32, delta: String },
    /// `{ type: "toolcall_end"; contentIndex }` (`proxy.ts:44`).
    ToolcallEnd { content_index: u32 },
    /// `{ type: "done"; reason; usage }` — terminal success; `reason` is one of
    /// `stop | length | toolUse` (`proxy.ts:45-49`).
    Done { reason: StopReason, usage: Usage },
    /// `{ type: "error"; reason; errorMessage?; usage }` — terminal failure;
    /// `reason` is one of `aborted | error` (`proxy.ts:50-55`).
    Error {
        reason: StopReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        usage: Usage,
    },
    /// Catch-all for an unrecognised proxy event tag (pi's `default` arm). Ignored
    /// by [`ProxyPartial::process_proxy_event`], which returns `None` for it.
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Reconstruction state machine (`proxy.ts:229-367`)
// ---------------------------------------------------------------------------

/// The partial-message accumulator threaded through
/// [`ProxyPartial::process_proxy_event`].
///
/// Bundles the in-flight [`AssistantMessage`] with the tool-call `partialJson`
/// side channel pi keeps on the block itself (see the module docs). Construct
/// with [`ProxyPartial::new`], drive it with [`ProxyPartial::process_proxy_event`],
/// and read the reconstructed message from [`ProxyPartial::message`].
#[derive(Debug, Clone)]
pub struct ProxyPartial {
    /// The message rebuilt so far — pi's `partial` (`proxy.ts:120-137`).
    pub message: AssistantMessage,
    /// Streamed tool-call argument text, keyed by content index. pi stores this
    /// as `partialJson` on the `toolCall` block; here it is external and dropped
    /// at `toolcall_end`.
    tool_call_json: BTreeMap<u32, String>,
}

impl ProxyPartial {
    /// Initialise the accumulator exactly as pi's `streamProxy` does
    /// (`proxy.ts:120-137`): an empty `assistant` message stamped with the
    /// model's `api`/`provider`/`id`, `stopReason: "stop"`, zeroed usage, and
    /// `timestamp: Date.now()` (here read from the injected [`Clock`]).
    pub fn new(model: &Model, clock: &dyn Clock) -> Self {
        Self {
            message: AssistantMessage {
                role: AssistantRole::Assistant,
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                diagnostics: None,
                usage: Usage {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    cache_write_1h: None,
                    reasoning: None,
                    total_tokens: 0,
                    cost: UsageCost::default(),
                },
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: clock.now_ms(),
            },
            tool_call_json: BTreeMap::new(),
        }
    }

    /// Assign `block` at `index`, growing `content` with [`ContentBlock::Unknown`]
    /// fillers for any gap.
    ///
    /// pi assigns `partial.content[contentIndex] = …` directly, which in JS
    /// silently creates holes for skipped indices; a later delta on such a hole
    /// hits `content?.type` on `undefined` and takes the error branch. The
    /// `Unknown` filler reproduces that: a delta targeting it fails every type
    /// check and yields the same "non-<kind> content" error.
    fn set_content(&mut self, index: u32, block: ContentBlock) {
        let i = index as usize;
        if i >= self.message.content.len() {
            self.message.content.resize(i + 1, ContentBlock::Unknown);
        }
        self.message.content[i] = block;
    }

    /// Process one proxy event, mutating [`self.message`](Self::message) and
    /// returning the reconstructed [`AssistantMessageEvent`] to emit — or `None`
    /// when the event produces no output (pi's `undefined`).
    ///
    /// Faithful to pi's `processProxyEvent` (`proxy.ts:229-367`). The `Err(String)`
    /// arms mirror pi's `throw new Error(...)` for a delta/end that targets a
    /// block of the wrong type; [`stream_proxy`] catches these into a terminal
    /// `error` event exactly as pi's `try/catch` does (`proxy.ts:213-224`).
    pub fn process_proxy_event(
        &mut self,
        proxy_event: ProxyAssistantMessageEvent,
    ) -> Result<Option<AssistantMessageEvent>, String> {
        match proxy_event {
            // case "start": return { type: "start", partial };
            ProxyAssistantMessageEvent::Start => Ok(Some(AssistantMessageEvent::Start {
                partial: self.message.clone(),
            })),

            // case "text_start":
            //   partial.content[i] = { type: "text", text: "" };
            //   return { type: "text_start", contentIndex: i, partial };
            ProxyAssistantMessageEvent::TextStart { content_index } => {
                self.set_content(
                    content_index,
                    ContentBlock::Text {
                        text: String::new(),
                        text_signature: None,
                    },
                );
                Ok(Some(AssistantMessageEvent::TextStart {
                    content_index,
                    partial: self.message.clone(),
                }))
            }

            // case "text_delta": content.text += delta;  (else throw)
            ProxyAssistantMessageEvent::TextDelta {
                content_index,
                delta,
            } => {
                match self.message.content.get_mut(content_index as usize) {
                    Some(ContentBlock::Text { text, .. }) => text.push_str(&delta),
                    _ => return Err("Received text_delta for non-text content".to_string()),
                }
                Ok(Some(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial: self.message.clone(),
                }))
            }

            // case "text_end": content.textSignature = contentSignature;
            //   return { ..., content: content.text, partial };  (else throw)
            ProxyAssistantMessageEvent::TextEnd {
                content_index,
                content_signature,
            } => {
                let content = match self.message.content.get_mut(content_index as usize) {
                    Some(ContentBlock::Text {
                        text,
                        text_signature,
                    }) => {
                        *text_signature = content_signature;
                        text.clone()
                    }
                    _ => return Err("Received text_end for non-text content".to_string()),
                };
                Ok(Some(AssistantMessageEvent::TextEnd {
                    content_index,
                    content,
                    partial: self.message.clone(),
                }))
            }

            // case "thinking_start":
            //   partial.content[i] = { type: "thinking", thinking: "" };
            ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
                self.set_content(
                    content_index,
                    ContentBlock::Thinking {
                        thinking: String::new(),
                        thinking_signature: None,
                        redacted: None,
                    },
                );
                Ok(Some(AssistantMessageEvent::ThinkingStart {
                    content_index,
                    partial: self.message.clone(),
                }))
            }

            // case "thinking_delta": content.thinking += delta;  (else throw)
            ProxyAssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                match self.message.content.get_mut(content_index as usize) {
                    Some(ContentBlock::Thinking { thinking, .. }) => thinking.push_str(&delta),
                    _ => return Err("Received thinking_delta for non-thinking content".to_string()),
                }
                Ok(Some(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: self.message.clone(),
                }))
            }

            // case "thinking_end": content.thinkingSignature = contentSignature;
            //   return { ..., content: content.thinking, partial };  (else throw)
            ProxyAssistantMessageEvent::ThinkingEnd {
                content_index,
                content_signature,
            } => {
                let content = match self.message.content.get_mut(content_index as usize) {
                    Some(ContentBlock::Thinking {
                        thinking,
                        thinking_signature,
                        ..
                    }) => {
                        *thinking_signature = content_signature;
                        thinking.clone()
                    }
                    _ => return Err("Received thinking_end for non-thinking content".to_string()),
                };
                Ok(Some(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial: self.message.clone(),
                }))
            }

            // case "toolcall_start":
            //   partial.content[i] = { type: "toolCall", id, name: toolName,
            //     arguments: {}, partialJson: "" };
            ProxyAssistantMessageEvent::ToolcallStart {
                content_index,
                id,
                tool_name,
            } => {
                self.set_content(
                    content_index,
                    ContentBlock::ToolCall {
                        id,
                        name: tool_name,
                        arguments: serde_json::Value::Object(Default::default()),
                        thought_signature: None,
                    },
                );
                self.tool_call_json.insert(content_index, String::new());
                Ok(Some(AssistantMessageEvent::ToolcallStart {
                    content_index,
                    partial: self.message.clone(),
                }))
            }

            // case "toolcall_delta":
            //   content.partialJson += delta;
            //   content.arguments = parseStreamingJson(content.partialJson) || {};
            //   (else throw)
            ProxyAssistantMessageEvent::ToolcallDelta {
                content_index,
                delta,
            } => {
                if !matches!(
                    self.message.content.get(content_index as usize),
                    Some(ContentBlock::ToolCall { .. })
                ) {
                    return Err("Received toolcall_delta for non-toolCall content".to_string());
                }
                let accumulated = self.tool_call_json.entry(content_index).or_default();
                accumulated.push_str(&delta);
                // parse_streaming_json already falls back to `{}` on failure, so
                // pi's `|| {}` is subsumed.
                let parsed = parse_streaming_json(Some(accumulated));
                if let Some(ContentBlock::ToolCall { arguments, .. }) =
                    self.message.content.get_mut(content_index as usize)
                {
                    *arguments = parsed;
                }
                Ok(Some(AssistantMessageEvent::ToolcallDelta {
                    content_index,
                    delta,
                    partial: self.message.clone(),
                }))
            }

            // case "toolcall_end":
            //   if toolCall: delete content.partialJson;
            //     return { ..., toolCall: content, partial };
            //   else return undefined;   (NOTE: no throw)
            ProxyAssistantMessageEvent::ToolcallEnd { content_index } => {
                match self.message.content.get(content_index as usize) {
                    Some(ContentBlock::ToolCall { .. }) => {
                        self.tool_call_json.remove(&content_index);
                        let tool_call = self.message.content[content_index as usize].clone();
                        Ok(Some(AssistantMessageEvent::ToolcallEnd {
                            content_index,
                            tool_call,
                            partial: self.message.clone(),
                        }))
                    }
                    _ => Ok(None),
                }
            }

            // case "done": partial.stopReason = reason; partial.usage = usage;
            //   return { type: "done", reason, message: partial };
            ProxyAssistantMessageEvent::Done { reason, usage } => {
                self.message.stop_reason = reason;
                self.message.usage = usage;
                Ok(Some(AssistantMessageEvent::Done {
                    reason,
                    message: self.message.clone(),
                }))
            }

            // case "error": partial.stopReason = reason;
            //   partial.errorMessage = errorMessage; partial.usage = usage;
            //   return { type: "error", reason, error: partial };
            ProxyAssistantMessageEvent::Error {
                reason,
                error_message,
                usage,
            } => {
                self.message.stop_reason = reason;
                self.message.error_message = error_message;
                self.message.usage = usage;
                Ok(Some(AssistantMessageEvent::Error {
                    reason,
                    error: self.message.clone(),
                }))
            }

            // default: console.warn(...); return undefined;
            ProxyAssistantMessageEvent::Unknown => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Eager transform (`proxy.ts:113-227`)
// ---------------------------------------------------------------------------

/// Reconstruct a full assistant stream from bandwidth-stripped proxy events.
///
/// The eager analog of pi's `streamProxy()` (`proxy.ts:113`), with the network
/// half omitted (see the module docs). Given the `model` (for the initial
/// `api`/`provider`/`id`), a `clock` for the message timestamp, an already-decoded
/// `proxy_events` sequence, and an optional abort `signal`, it drives
/// [`ProxyPartial::process_proxy_event`] over each event, collecting the emitted
/// [`AssistantMessageEvent`]s and the final reconstructed [`AssistantMessage`]
/// into a [`StreamResult`].
///
/// Error handling mirrors pi's `try/catch` (`proxy.ts:213-224`):
///
/// - A server-sent `error` proxy event flows through normally as a terminal
///   `error` [`AssistantMessageEvent`] — pi does not throw for it.
/// - A `throw` from `process_proxy_event` (a delta/end on a wrong-typed block),
///   or a tripped abort `signal`, is caught: the partial's `stopReason` becomes
///   `aborted` (if the signal is set) or `error`, its `errorMessage` is set, and
///   a synthesised terminal `error` event is appended. Processing then stops.
///
/// `signal` is checked before each event and once more after the loop, matching
/// pi's mid-read `if (options.signal?.aborted) throw …` guards
/// (`proxy.ts:186-188`, `proxy.ts:203-205`); a trip yields the message
/// `"Request aborted by user"` and reason `aborted`.
pub fn stream_proxy(
    model: &Model,
    clock: &dyn Clock,
    proxy_events: impl IntoIterator<Item = ProxyAssistantMessageEvent>,
    signal: Option<&AbortSignal>,
) -> StreamResult {
    fn is_aborted(signal: Option<&AbortSignal>) -> bool {
        signal.is_some_and(AbortSignal::is_aborted)
    }

    let mut state = ProxyPartial::new(model, clock);
    let mut events: Vec<AssistantMessageEvent> = Vec::new();

    let outcome: Result<(), String> = (|| {
        for proxy_event in proxy_events {
            if is_aborted(signal) {
                return Err("Request aborted by user".to_string());
            }
            if let Some(event) = state.process_proxy_event(proxy_event)? {
                events.push(event);
            }
        }
        if is_aborted(signal) {
            return Err("Request aborted by user".to_string());
        }
        Ok(())
    })();

    if let Err(error_message) = outcome {
        let reason = if is_aborted(signal) {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        state.message.stop_reason = reason;
        state.message.error_message = Some(error_message);
        events.push(AssistantMessageEvent::Error {
            reason,
            error: state.message.clone(),
        });
    }

    StreamResult {
        events,
        message: state.message,
    }
}

#[cfg(test)]
mod tests;
