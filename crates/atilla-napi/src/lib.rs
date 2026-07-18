//! Node-API bridge for atilla, built with napi-rs as a `cdylib` addon.
//!
//! This crate exposes the Rust engine to JavaScript. napi's generated `.d.ts`
//! cannot express pi's rich discriminated-union types, so the generated types
//! stay internal (crossing the boundary as JSON strings) and the public JS
//! surface is fronted by pi's own type declarations in the hand-written shims;
//! export names are pinned per-symbol with `#[napi(js_name = …)]`.

use napi_derive::napi;

// Stage 3: the faux-provider surface (`FauxCore`), driving the Rust faux
// provider's deterministic streaming and cache accounting from JS. Additive.
mod faux;

/// Returns the crate version. Proves the native addon builds and loads.
///
/// Exported to JavaScript as `atillaNativeVersion`.
#[napi(js_name = "atillaNativeVersion")]
pub fn atilla_native_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Parse an Anthropic Messages SSE body into the uniform assistant-message event
/// stream and final message, backed by
/// [`atilla_ai::api::anthropic::parse_sse_stream_to_json`].
///
/// Exported to JavaScript as `anthropicParseSseStream`. The boundary is JSON on
/// both sides: `model` is the JSON-serialized pi `Model`, and the return value
/// is a JSON string `{ "events": [...], "message": {...} }` matching pi's
/// `AssistantMessageEvent[]` and `AssistantMessage` shapes. The `anthropic-messages`
/// shim reads the SSE bytes from the injected transport, calls this, and replays
/// the events into pi's `AssistantMessageEventStream`.
///
/// `isOAuth` selects Claude-Code tool-name normalization (the shim passes `false`
/// on the injected-transport path); `timestamp` is the message timestamp
/// (`Date.now()`).
#[napi(js_name = "anthropicParseSseStream")]
pub fn anthropic_parse_sse_stream(
    body: String,
    model: String,
    is_oauth: bool,
    timestamp: f64,
) -> napi::Result<String> {
    atilla_ai::api::anthropic::parse_sse_stream_to_json(&body, &model, is_oauth, timestamp as i64)
        .map_err(napi::Error::from_reason)
}

// --- tui width layer (packages/tui/src/utils.ts) ---------------------------
//
// Thin wrappers over `atilla_tui::width`, backing the native `utils.ts` shim.
// Each mirrors the pi export it replaces; the shim supplies pi's default
// arguments so the JS-facing signatures stay byte-for-byte pi's.

/// `visibleWidth` (utils.ts): display width of a string, ANSI-aware.
#[napi(js_name = "visibleWidth")]
pub fn visible_width(s: String) -> i64 {
    atilla_tui::visible_width(&s) as i64
}

/// `normalizeTerminalOutput` (utils.ts): canonicalize ANSI/control sequences.
#[napi(js_name = "normalizeTerminalOutput")]
pub fn normalize_terminal_output(s: String) -> String {
    atilla_tui::normalize_terminal_output(&s)
}

/// `truncateToWidth` (utils.ts): clip to `max_width` columns, ANSI-preserving.
#[napi(js_name = "truncateToWidth")]
pub fn truncate_to_width(text: String, max_width: i64, ellipsis: String, pad: bool) -> String {
    atilla_tui::truncate_to_width(&text, max_width, &ellipsis, pad)
}

/// `wrapTextWithAnsi` (utils.ts): hard-wrap to `width` columns, ANSI-preserving.
#[napi(js_name = "wrapTextWithAnsi")]
pub fn wrap_text_with_ansi(text: String, width: i64) -> Vec<String> {
    atilla_tui::wrap_text_with_ansi(&text, width.max(0) as usize)
}

/// Result of [`slice_with_width`]; serialized to `{ text, width }`.
#[napi(object)]
pub struct SliceWithWidth {
    pub text: String,
    pub width: i64,
}

/// `sliceWithWidth` (utils.ts): slice `length` columns from `start_col`.
#[napi(js_name = "sliceWithWidth")]
pub fn slice_with_width(line: String, start_col: i64, length: i64, strict: bool) -> SliceWithWidth {
    let (text, width) = atilla_tui::slice_with_width(&line, start_col, length, strict);
    SliceWithWidth { text, width }
}

/// Result of [`extract_segments`]; serialized to
/// `{ before, beforeWidth, after, afterWidth }`.
#[napi(object)]
pub struct ExtractSegmentsResult {
    pub before: String,
    pub before_width: i64,
    pub after: String,
    pub after_width: i64,
}

/// `extractSegments` (utils.ts): single-pass before/after overlay split.
#[napi(js_name = "extractSegments")]
pub fn extract_segments(
    line: String,
    before_end: i64,
    after_start: i64,
    after_len: i64,
    strict_after: bool,
) -> ExtractSegmentsResult {
    let r = atilla_tui::extract_segments(&line, before_end, after_start, after_len, strict_after);
    ExtractSegmentsResult {
        before: r.before,
        before_width: r.before_width,
        after: r.after,
        after_width: r.after_width,
    }
}

// --- tui key layer (packages/tui/src/keys.ts) ------------------------------
//
// Thin wrappers over `atilla_tui::keys`, backing the native `keys.ts` shim.
// The kitty-protocol flag lives in a Rust static, so overriding `parseKey`,
// the decoders, and `setKittyProtocolActive` together keeps the read/write
// pair consistent within the single addon instance.

/// `parseKey` (keys.ts): decode a raw key sequence to its canonical id.
#[napi(js_name = "parseKey")]
pub fn parse_key(data: String) -> Option<String> {
    atilla_tui::parse_key(&data)
}

/// `matchesKey` (keys.ts): does `data` decode to `key_id`?
#[napi(js_name = "matchesKey")]
pub fn matches_key(data: String, key_id: String) -> bool {
    atilla_tui::matches_key(&data, &key_id)
}

/// `decodeKittyPrintable` (keys.ts): printable char from a kitty sequence.
#[napi(js_name = "decodeKittyPrintable")]
pub fn decode_kitty_printable(data: String) -> Option<String> {
    atilla_tui::decode_kitty_printable(&data)
}

/// `decodePrintableKey` (keys.ts): printable char from a key sequence.
#[napi(js_name = "decodePrintableKey")]
pub fn decode_printable_key(data: String) -> Option<String> {
    atilla_tui::decode_printable_key(&data)
}

/// `setKittyProtocolActive` (keys.ts): toggle kitty-protocol decoding.
#[napi(js_name = "setKittyProtocolActive")]
pub fn set_kitty_protocol_active(active: bool) {
    atilla_tui::set_kitty_protocol_active(active);
}
