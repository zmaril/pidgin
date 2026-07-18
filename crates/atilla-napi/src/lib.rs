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

// --- coding-agent utils layer -----------------------------------------------
//
// Thin wrappers over `atilla_coding::utils::*`, backing the hand-written native
// shims under conformance/shims/packages/coding-agent/src/utils/. Each mirrors
// the pi export it replaces; the shims re-export the un-ported surface from the
// preserved pi original and override only these symbols.

/// `stripAnsi` (utils/ansi.ts): remove ANSI escape sequences (OSC + CSI). The
/// shim keeps pi's non-string `TypeError` guard, so only strings reach here.
#[napi(js_name = "stripAnsi")]
pub fn strip_ansi(value: String) -> String {
    atilla_coding::utils::ansi::strip_ansi(&value)
}

/// `detectSupportedImageMimeType` (utils/mime.ts): sniff a supported image MIME
/// type from magic bytes, or `null`.
#[napi(js_name = "detectSupportedImageMimeType")]
pub fn detect_supported_image_mime_type(buffer: napi::bindgen_prelude::Uint8Array) -> Option<String> {
    atilla_coding::utils::mime::detect_supported_image_mime_type(&buffer).map(|s| s.to_string())
}

/// `normalizeChangelogLinks` (utils/changelog.ts): rewrite inline markdown links
/// for a release. `version_json` is the JSON-serialized `string | ChangelogEntry`
/// the shim passes; a bare JSON string is a raw version, a JSON object is a
/// `ChangelogEntry`.
#[napi(js_name = "normalizeChangelogLinks")]
pub fn normalize_changelog_links(markdown: String, version_json: String) -> napi::Result<String> {
    use atilla_coding::utils::changelog::{normalize_changelog_links, ChangelogEntry};
    let value: serde_json::Value =
        serde_json::from_str(&version_json).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    match value {
        serde_json::Value::String(s) => Ok(normalize_changelog_links(&markdown, s.as_str())),
        serde_json::Value::Object(map) => {
            let entry = ChangelogEntry {
                major: map.get("major").and_then(|v| v.as_u64()).unwrap_or(0),
                minor: map.get("minor").and_then(|v| v.as_u64()).unwrap_or(0),
                patch: map.get("patch").and_then(|v| v.as_u64()).unwrap_or(0),
                content: map
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            };
            Ok(normalize_changelog_links(&markdown, &entry))
        }
        _ => Err(napi::Error::from_reason(
            "version must be a string or ChangelogEntry object",
        )),
    }
}

/// `comparePackageVersions` (utils/version-check.ts): compare two semver
/// strings, mapping `Ordering` to `-1`/`0`/`1`. `None` (incomparable) crosses as
/// JS `null`; the shim converts it to `undefined` to match pi's `number |
/// undefined`.
#[napi(js_name = "comparePackageVersions")]
pub fn compare_package_versions(left_version: String, right_version: String) -> Option<i32> {
    atilla_coding::utils::version_check::compare_package_versions(&left_version, &right_version).map(
        |ordering| match ordering {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    )
}

/// `isNewerPackageVersion` (utils/version-check.ts): is `candidate` strictly
/// newer than `current`?
#[napi(js_name = "isNewerPackageVersion")]
pub fn is_newer_package_version(candidate_version: String, current_version: String) -> bool {
    atilla_coding::utils::version_check::is_newer_package_version(
        &candidate_version,
        &current_version,
    )
}

/// `parseGitUrl` (utils/git.ts): parse a git source string into pi's `GitSource`
/// JSON shape (`{ type, repo, host, path, ref?, pinned }`), or `null`. The shim
/// `JSON.parse`s the result.
#[napi(js_name = "parseGitUrl")]
pub fn parse_git_url(source: String) -> Option<String> {
    let parsed = atilla_coding::utils::git_url::parse_git_url(&source)?;
    let mut obj = serde_json::Map::new();
    obj.insert("type".to_string(), serde_json::json!(parsed.kind));
    obj.insert("repo".to_string(), serde_json::json!(parsed.repo));
    obj.insert("host".to_string(), serde_json::json!(parsed.host));
    obj.insert("path".to_string(), serde_json::json!(parsed.path));
    if let Some(git_ref) = parsed.git_ref {
        obj.insert("ref".to_string(), serde_json::json!(git_ref));
    }
    obj.insert("pinned".to_string(), serde_json::json!(parsed.pinned));
    Some(serde_json::Value::Object(obj).to_string())
}

// --- coding-agent export-html layer -----------------------------------------
//
// Thin wrappers over `atilla_coding::core::export_html::ansi_to_html`, backing
// the native `core/export-html/ansi-to-html.ts` shim.

/// `ansiToHtml` (core/export-html/ansi-to-html.ts): convert ANSI-escaped text to
/// HTML with inline styles.
#[napi(js_name = "ansiToHtml")]
pub fn ansi_to_html(text: String) -> String {
    atilla_coding::core::export_html::ansi_to_html::ansi_to_html(&text)
}

/// `ansiLinesToHtml` (core/export-html/ansi-to-html.ts): convert an array of
/// ANSI-escaped lines to HTML, wrapping each in an `ansi-line` div.
#[napi(js_name = "ansiLinesToHtml")]
pub fn ansi_lines_to_html(lines: Vec<String>) -> String {
    atilla_coding::core::export_html::ansi_to_html::ansi_lines_to_html(&lines)
}
