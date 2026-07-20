//! Coding-agent compaction-utils surface (`serializeConversation`): drives pi's
//! `serializeConversation` message-to-text serializer natively.
//!
//! Scope of the native flip is the single pure function `serializeConversation`
//! from `packages/coding-agent/src/core/compaction/utils.ts`, ported to
//! [`pidgin_coding::core::compaction::serialize_conversation`]. pi takes
//! `Message[]` (the `convertToLlm` output, whose messages are opaque JSON to the
//! Rust port — `AgentMessage` is a `serde_json::Value` alias) and returns the
//! serialized string; this bridge crosses the boundary as a JSON array string in
//! and a plain string out. The remaining `utils.ts` exports (file-op helpers and
//! `SUMMARIZATION_SYSTEM_PROMPT`) are re-exported unchanged by the shim from pi's
//! preserved original — they are not part of this flip.

use napi_derive::napi;
use serde_json::Value;

/// `serializeConversation` (compaction/utils.ts): serialize a list of LLM
/// messages to plain text for summarization prompts.
///
/// `messages_json` is `JSON.stringify(messages)` — a JSON array of message
/// objects. Returns the serialized conversation text. A malformed payload throws
/// a JS error rather than silently producing empty output.
///
/// Exported to JavaScript as `serializeConversation`.
#[napi(js_name = "serializeConversation")]
pub fn serialize_conversation(messages_json: String) -> napi::Result<String> {
    let messages: Vec<Value> = serde_json::from_str(&messages_json).map_err(|e| {
        napi::Error::from_reason(format!("serializeConversation: invalid messages JSON: {e}"))
    })?;
    Ok(pidgin_coding::core::compaction::serialize_conversation(
        &messages,
    ))
}
