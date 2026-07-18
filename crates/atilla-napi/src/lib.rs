//! Node-API bridge for atilla, built with napi-rs as a `cdylib` addon.
//!
//! This crate exposes the Rust engine to JavaScript. napi's generated `.d.ts`
//! cannot express pi's rich discriminated-union types, so the generated types
//! stay internal (crossing the boundary as JSON strings) and the public JS
//! surface is fronted by pi's own type declarations in the hand-written shims;
//! export names are pinned per-symbol with `#[napi(js_name = …)]`.

use std::collections::HashMap;

use indexmap::IndexMap;
use napi_derive::napi;
use serde_json::Value;

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
pub fn detect_supported_image_mime_type(
    buffer: napi::bindgen_prelude::Uint8Array,
) -> Option<String> {
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
    atilla_coding::utils::version_check::compare_package_versions(&left_version, &right_version)
        .map(|ordering| match ordering {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        })
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

// --- coding-agent tools: truncate ------------------------------------------
//
// Thin wrappers over `atilla_coding::core::tools::truncate`, backing the native
// `core/tools/truncate.ts` shim. Structured results cross as JSON strings using
// pi's exact `TruncationResult` field names; the shim `JSON.parse`s them and
// re-adds pi's JS default arguments (which the Rust port dropped).

/// Serialize a Rust `TruncationResult` into pi's `TruncationResult` JSON shape,
/// mapping the `TruncatedBy` enum + `Option` to pi's `"lines" | "bytes" | null`.
fn truncation_result_to_json(r: &atilla_coding::core::tools::truncate::TruncationResult) -> String {
    use atilla_coding::core::tools::truncate::TruncatedBy;
    let truncated_by = match r.truncated_by {
        None => serde_json::Value::Null,
        Some(TruncatedBy::Lines) => serde_json::json!("lines"),
        Some(TruncatedBy::Bytes) => serde_json::json!("bytes"),
    };
    serde_json::json!({
        "content": r.content,
        "truncated": r.truncated,
        "truncatedBy": truncated_by,
        "totalLines": r.total_lines,
        "totalBytes": r.total_bytes,
        "outputLines": r.output_lines,
        "outputBytes": r.output_bytes,
        "lastLinePartial": r.last_line_partial,
        "firstLineExceedsLimit": r.first_line_exceeds_limit,
        "maxLines": r.max_lines,
        "maxBytes": r.max_bytes,
    })
    .to_string()
}

/// `formatSize` (core/tools/truncate.ts): format a byte count as `B`/`KB`/`MB`.
#[napi(js_name = "truncateFormatSize")]
pub fn truncate_format_size(bytes: i64) -> String {
    atilla_coding::core::tools::truncate::format_size(bytes.max(0) as usize)
}

/// `truncateHead` (core/tools/truncate.ts): keep the first N lines/bytes. The
/// shim supplies `maxLines`/`maxBytes` (its defaulted `TruncationOptions`).
#[napi(js_name = "truncateHead")]
pub fn truncate_head(content: String, max_lines: i64, max_bytes: i64) -> String {
    use atilla_coding::core::tools::truncate::{truncate_head, TruncationOptions};
    let opts = TruncationOptions {
        max_lines: max_lines.max(0) as usize,
        max_bytes: max_bytes.max(0) as usize,
    };
    truncation_result_to_json(&truncate_head(&content, opts))
}

/// `truncateTail` (core/tools/truncate.ts): keep the last N lines/bytes.
#[napi(js_name = "truncateTail")]
pub fn truncate_tail(content: String, max_lines: i64, max_bytes: i64) -> String {
    use atilla_coding::core::tools::truncate::{truncate_tail, TruncationOptions};
    let opts = TruncationOptions {
        max_lines: max_lines.max(0) as usize,
        max_bytes: max_bytes.max(0) as usize,
    };
    truncation_result_to_json(&truncate_tail(&content, opts))
}

/// `truncateLine` (core/tools/truncate.ts): truncate a single line to
/// `maxChars`, returning pi's `{ text, wasTruncated }` shape as JSON. The shim
/// supplies the `GREP_MAX_LINE_LENGTH` default for `maxChars`.
#[napi(js_name = "truncateLine")]
pub fn truncate_line(line: String, max_chars: i64) -> String {
    let r = atilla_coding::core::tools::truncate::truncate_line(&line, max_chars.max(0) as usize);
    serde_json::json!({ "text": r.text, "wasTruncated": r.was_truncated }).to_string()
}

// --- coding-agent tools: edit-diff -----------------------------------------
//
// Thin wrappers over `atilla_coding::core::tools::edit_diff`, backing the native
// `core/tools/edit-diff.ts` shim. The `LineEnding` enum crosses as pi's
// `"\r\n" | "\n"` union; structured results cross as JSON strings with pi's
// exact field names. The async `computeEditsDiff`/`computeEditDiff` are not
// ported and stay in pi's original.

fn line_ending_to_str(ending: atilla_coding::core::tools::edit_diff::LineEnding) -> &'static str {
    match ending {
        atilla_coding::core::tools::edit_diff::LineEnding::Crlf => "\r\n",
        atilla_coding::core::tools::edit_diff::LineEnding::Lf => "\n",
    }
}

/// `detectLineEnding` (core/tools/edit-diff.ts): detect the dominant line
/// ending, returning pi's `"\r\n" | "\n"` union.
#[napi(js_name = "detectLineEnding")]
pub fn detect_line_ending(content: String) -> String {
    line_ending_to_str(atilla_coding::core::tools::edit_diff::detect_line_ending(
        &content,
    ))
    .to_string()
}

/// `normalizeToLF` (core/tools/edit-diff.ts): normalize all line endings to
/// `\n`.
#[napi(js_name = "normalizeToLf")]
pub fn normalize_to_lf(text: String) -> String {
    atilla_coding::core::tools::edit_diff::normalize_to_lf(&text)
}

/// `restoreLineEndings` (core/tools/edit-diff.ts): restore `\n` to `ending`. The
/// shim passes pi's `"\r\n" | "\n"` union as `ending`.
#[napi(js_name = "restoreLineEndings")]
pub fn restore_line_endings(text: String, ending: String) -> String {
    use atilla_coding::core::tools::edit_diff::LineEnding;
    let le = if ending == "\r\n" {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    };
    atilla_coding::core::tools::edit_diff::restore_line_endings(&text, le)
}

/// `normalizeForFuzzyMatch` (core/tools/edit-diff.ts): NFKC + trailing-ws strip
/// + smart quote/dash/space folding.
#[napi(js_name = "normalizeForFuzzyMatch")]
pub fn normalize_for_fuzzy_match(text: String) -> String {
    atilla_coding::core::tools::edit_diff::normalize_for_fuzzy_match(&text)
}

/// `stripBom` (core/tools/edit-diff.ts): strip a leading UTF-8 BOM, returning
/// pi's `{ bom, text }` shape as JSON.
#[napi(js_name = "stripBom")]
pub fn strip_bom(content: String) -> String {
    let r = atilla_coding::core::tools::edit_diff::strip_bom(&content);
    serde_json::json!({ "bom": r.bom, "text": r.text }).to_string()
}

/// `fuzzyFindText` (core/tools/edit-diff.ts): exact-then-fuzzy search, returning
/// pi's `FuzzyMatchResult` shape as JSON. Offsets are byte-based (as in the Rust
/// port); pi's tests do not deep-index the returned offset.
#[napi(js_name = "fuzzyFindText")]
pub fn fuzzy_find_text(content: String, old_text: String) -> String {
    let r = atilla_coding::core::tools::edit_diff::fuzzy_find_text(&content, &old_text);
    let index: i64 = if r.found { r.index as i64 } else { -1 };
    serde_json::json!({
        "found": r.found,
        "index": index,
        "matchLength": r.match_length,
        "usedFuzzyMatch": r.used_fuzzy_match,
        "contentForReplacement": r.content_for_replacement,
    })
    .to_string()
}

/// `applyReplacementsPreservingUnchangedLines` (core/tools/edit-diff.ts): apply
/// replacements matched against a normalized base view to the original content,
/// keeping unchanged line blocks. `replacements_json` is pi's
/// `{ matchIndex, matchLength, newText }[]`; the array index supplies the
/// (algorithmically irrelevant) `edit_index`. Errors cross as thrown JS errors.
#[napi(js_name = "applyReplacementsPreservingUnchangedLines")]
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: String,
    base_content: String,
    replacements_json: String,
) -> napi::Result<String> {
    use atilla_coding::core::tools::edit_diff::PreservingReplacement;
    let raw: Vec<serde_json::Value> = serde_json::from_str(&replacements_json)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let reps: Vec<PreservingReplacement> = raw
        .iter()
        .enumerate()
        .map(|(i, v)| PreservingReplacement {
            edit_index: i,
            match_index: v.get("matchIndex").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            match_length: v.get("matchLength").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            new_text: v
                .get("newText")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect();
    atilla_coding::core::tools::edit_diff::apply_replacements_preserving_unchanged_lines(
        &original_content,
        &base_content,
        &reps,
    )
    .map_err(napi::Error::from_reason)
}

/// `applyEditsToNormalizedContent` (core/tools/edit-diff.ts): apply one or more
/// exact-text replacements to LF-normalized content, returning pi's
/// `{ baseContent, newContent }` shape as JSON. `edits_json` is pi's
/// `{ oldText, newText }[]`. Match/duplicate/overlap errors cross as thrown JS
/// errors (pi throws too).
#[napi(js_name = "applyEditsToNormalizedContent")]
pub fn apply_edits_to_normalized_content(
    normalized_content: String,
    edits_json: String,
    path: String,
) -> napi::Result<String> {
    use atilla_coding::core::tools::edit_diff::Edit;
    let raw: Vec<serde_json::Value> =
        serde_json::from_str(&edits_json).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let edits: Vec<Edit> = raw
        .iter()
        .map(|v| Edit {
            old_text: v
                .get("oldText")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            new_text: v
                .get("newText")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect();
    let r = atilla_coding::core::tools::edit_diff::apply_edits_to_normalized_content(
        &normalized_content,
        &edits,
        &path,
    )
    .map_err(napi::Error::from_reason)?;
    Ok(serde_json::json!({
        "baseContent": r.base_content,
        "newContent": r.new_content,
    })
    .to_string())
}

/// `generateUnifiedPatch` (core/tools/edit-diff.ts): jsdiff-compatible unified
/// patch. The shim supplies pi's `contextLines = 4` default.
#[napi(js_name = "generateUnifiedPatch")]
pub fn generate_unified_patch(
    path: String,
    old_content: String,
    new_content: String,
    context_lines: i64,
) -> String {
    atilla_coding::core::tools::edit_diff::generate_unified_patch(
        &path,
        &old_content,
        &new_content,
        context_lines.max(0) as usize,
    )
}

/// `generateDiffString` (core/tools/edit-diff.ts): display-oriented diff with
/// line numbers, returning pi's `{ diff, firstChangedLine }` shape as JSON
/// (`firstChangedLine` is `null` when there is no change; the shim maps it to
/// `undefined`). The shim supplies pi's `contextLines = 4` default.
#[napi(js_name = "generateDiffString")]
pub fn generate_diff_string(
    old_content: String,
    new_content: String,
    context_lines: i64,
) -> String {
    let r = atilla_coding::core::tools::edit_diff::generate_diff_string(
        &old_content,
        &new_content,
        context_lines.max(0) as usize,
    );
    let first_changed = match r.first_changed_line {
        Some(n) => serde_json::json!(n),
        None => serde_json::Value::Null,
    };
    serde_json::json!({ "diff": r.diff, "firstChangedLine": first_changed }).to_string()
}

// --- coding-agent tools: path-utils ----------------------------------------
//
// Thin wrappers over `atilla_coding::core::tools::path_utils`, backing the
// native `core/tools/path-utils.ts` shim. `expandPath`/`resolveToCwd` return a
// Rust `Result`; the shim maps a thrown error back to pi's throw-on-bad-input.
// The private macOS filename transforms are exposed so the shim can rebuild
// pi's `resolveReadPath` fs-probe fallback with a real `accessSync` closure.
// `pathExists`/`resolveReadPathAsync` are not ported and stay in pi's original.

/// `expandPath` (core/tools/path-utils.ts): fold unicode spaces, strip a leading
/// `@`, expand `~`, convert `file://`. Errors cross as thrown JS errors.
#[napi(js_name = "expandPath")]
pub fn expand_path(file_path: String) -> napi::Result<String> {
    atilla_coding::core::tools::path_utils::expand_path(&file_path)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// `resolveToCwd` (core/tools/path-utils.ts): resolve `file_path` against `cwd`.
/// Errors cross as thrown JS errors (pi's `resolvePath` throws on bad input).
#[napi(js_name = "resolveToCwd")]
pub fn resolve_to_cwd(file_path: String, cwd: String) -> napi::Result<String> {
    atilla_coding::core::tools::path_utils::resolve_to_cwd(&file_path, &cwd)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Private pi transform `tryMacOSScreenshotPath`, exposed so the shim's
/// `resolveReadPath` can rebuild pi's fallback ordering natively.
#[napi(js_name = "pathTryMacosScreenshotPath")]
pub fn path_try_macos_screenshot_path(file_path: String) -> String {
    atilla_coding::core::tools::path_utils::try_macos_screenshot_path(&file_path)
}

/// Private pi transform `tryNFDVariant`, exposed for the shim's `resolveReadPath`.
#[napi(js_name = "pathTryNfdVariant")]
pub fn path_try_nfd_variant(file_path: String) -> String {
    atilla_coding::core::tools::path_utils::try_nfd_variant(&file_path)
}

/// Private pi transform `tryCurlyQuoteVariant`, exposed for the shim's
/// `resolveReadPath`.
#[napi(js_name = "pathTryCurlyQuoteVariant")]
pub fn path_try_curly_quote_variant(file_path: String) -> String {
    atilla_coding::core::tools::path_utils::try_curly_quote_variant(&file_path)
}

// --- coding-agent core: resolve-config-value --------------------------------
//
// Thin wrappers over `atilla_coding::core::resolve_config_value`, backing the
// native `core/resolve-config-value.ts` shim. pi's `env?` credential-scoped
// override crosses as an optional JSON object string; the process environment
// is read by the Rust port directly (`std::env::var`), matching pi's
// `env?.[name] || process.env[name]`. Rust `None` maps back to pi's
// `undefined` in the shim; `resolveConfigValue`'s `!command` subprocess path and
// the process-lifetime command cache live in Rust.

/// Parse pi's optional `env` override (a JSON `Record<string,string>`) into a
/// map. An absent/empty/`null` argument means "no override".
fn parse_config_env(json: Option<String>) -> napi::Result<Option<HashMap<String, String>>> {
    match json {
        None => Ok(None),
        Some(s) if s.trim().is_empty() || s == "null" => Ok(None),
        Some(s) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| napi::Error::from_reason(format!("invalid env override: {e}"))),
    }
}

/// `resolveConfigValue` (resolve-config-value.ts): resolve a literal / env
/// template / cached `!command`. `None` -> pi's `undefined`.
#[napi(js_name = "resolveConfigValue")]
pub fn resolve_config_value(config: String, env: Option<String>) -> napi::Result<Option<String>> {
    let env = parse_config_env(env)?;
    Ok(atilla_coding::core::resolve_config_value::resolve_config_value(&config, env.as_ref()))
}

/// `resolveConfigValueUncached` (resolve-config-value.ts): like
/// [`resolve_config_value`] but re-executes `!command`s every call.
#[napi(js_name = "resolveConfigValueUncached")]
pub fn resolve_config_value_uncached(
    config: String,
    env: Option<String>,
) -> napi::Result<Option<String>> {
    let env = parse_config_env(env)?;
    Ok(
        atilla_coding::core::resolve_config_value::resolve_config_value_uncached(
            &config,
            env.as_ref(),
        ),
    )
}

/// `resolveConfigValueOrThrow` (resolve-config-value.ts): resolve or throw pi's
/// descriptive error. Rust `Err` crosses as a thrown JS `Error` with pi's message.
#[napi(js_name = "resolveConfigValueOrThrow")]
pub fn resolve_config_value_or_throw(
    config: String,
    description: String,
    env: Option<String>,
) -> napi::Result<String> {
    let env = parse_config_env(env)?;
    atilla_coding::core::resolve_config_value::resolve_config_value_or_throw(
        &config,
        &description,
        env.as_ref(),
    )
    .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// `getConfigValueEnvVarName` (resolve-config-value.ts): the single env var a
/// value references, or `null` (pi's `undefined`).
#[napi(js_name = "getConfigValueEnvVarName")]
pub fn get_config_value_env_var_name(config: String) -> Option<String> {
    atilla_coding::core::resolve_config_value::get_config_value_env_var_name(&config)
}

/// `getConfigValueEnvVarNames` (resolve-config-value.ts): all distinct env var
/// names a value references, in first-seen order.
#[napi(js_name = "getConfigValueEnvVarNames")]
pub fn get_config_value_env_var_names(config: String) -> Vec<String> {
    atilla_coding::core::resolve_config_value::get_config_value_env_var_names(&config)
}

/// `getMissingConfigValueEnvVarNames` (resolve-config-value.ts): referenced env
/// var names that do not currently resolve.
#[napi(js_name = "getMissingConfigValueEnvVarNames")]
pub fn get_missing_config_value_env_var_names(
    config: String,
    env: Option<String>,
) -> napi::Result<Vec<String>> {
    let env = parse_config_env(env)?;
    Ok(
        atilla_coding::core::resolve_config_value::get_missing_config_value_env_var_names(
            &config,
            env.as_ref(),
        ),
    )
}

/// `isCommandConfigValue` (resolve-config-value.ts): whether a value is a
/// `!`-prefixed shell command.
#[napi(js_name = "isCommandConfigValue")]
pub fn is_command_config_value(config: String) -> bool {
    atilla_coding::core::resolve_config_value::is_command_config_value(&config)
}

/// `isConfigValueConfigured` (resolve-config-value.ts): whether every env var a
/// value references is set.
#[napi(js_name = "isConfigValueConfigured")]
pub fn is_config_value_configured(config: String, env: Option<String>) -> napi::Result<bool> {
    let env = parse_config_env(env)?;
    Ok(
        atilla_coding::core::resolve_config_value::is_config_value_configured(
            &config,
            env.as_ref(),
        ),
    )
}

/// Serialize a resolved header map to pi's `Record<string,string>` JSON, mapping
/// `None` to a JS `null`. Shared tail of the `resolveHeaders` wrappers.
fn headers_json(resolved: Option<HashMap<String, String>>) -> napi::Result<Option<String>> {
    match resolved {
        None => Ok(None),
        Some(map) => serde_json::to_string(&map)
            .map(Some)
            .map_err(|e| napi::Error::from_reason(e.to_string())),
    }
}

/// `resolveHeaders` (resolve-config-value.ts): resolve each header value,
/// dropping empties. Returns pi's `Record<string,string>` as JSON, or `null`.
#[napi(js_name = "resolveHeaders")]
pub fn resolve_headers(
    headers: Option<String>,
    env: Option<String>,
) -> napi::Result<Option<String>> {
    let headers = parse_config_env(headers)?;
    let env = parse_config_env(env)?;
    headers_json(atilla_coding::core::resolve_config_value::resolve_headers(
        headers.as_ref(),
        env.as_ref(),
    ))
}

/// `resolveHeadersOrThrow` (resolve-config-value.ts): resolve each header value
/// or throw pi's descriptive error. Returns the map JSON, or `null`.
#[napi(js_name = "resolveHeadersOrThrow")]
pub fn resolve_headers_or_throw(
    headers: Option<String>,
    description: String,
    env: Option<String>,
) -> napi::Result<Option<String>> {
    let headers = parse_config_env(headers)?;
    let env = parse_config_env(env)?;
    let resolved = atilla_coding::core::resolve_config_value::resolve_headers_or_throw(
        headers.as_ref(),
        &description,
        env.as_ref(),
    )
    .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    headers_json(resolved)
}

/// `clearConfigValueCache` (resolve-config-value.ts): clear the process-lifetime
/// `!command` result cache.
#[napi(js_name = "clearConfigValueCache")]
pub fn clear_config_value_cache() {
    atilla_coding::core::resolve_config_value::clear_config_value_cache();
}

// --- coding-agent core: trust-manager ---------------------------------------
//
// Thin wrappers over `atilla_coding::core::trust_manager`, backing the native
// `core/trust-manager.ts` shim. Structured values cross as JSON using pi's exact
// field names (`{ path, decision }`, `{ label, trusted, updates, savedPath? }`);
// the shim `JSON.parse`s them. `ProjectTrustStore` stays a JS class holding the
// agent dir, delegating each method to the stateless functions below (each
// reconstructs the Rust store, whose only state is the on-disk `trust.json`).

/// Serialize a Rust `ProjectTrustUpdate` into pi's `{ path, decision }` shape,
/// mapping `Option<bool>` to pi's `boolean | null`.
fn trust_update_to_json(update: &atilla_coding::core::trust_manager::ProjectTrustUpdate) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("path".to_string(), Value::String(update.path.clone()));
    obj.insert(
        "decision".to_string(),
        match update.decision {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
    );
    Value::Object(obj)
}

/// Serialize a Rust `ProjectTrustOption` into pi's option shape; `savedPath` is
/// omitted (pi's `undefined`) for session-only options.
fn trust_option_to_json(option: &atilla_coding::core::trust_manager::ProjectTrustOption) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("label".to_string(), Value::String(option.label.clone()));
    obj.insert("trusted".to_string(), Value::Bool(option.trusted));
    obj.insert(
        "updates".to_string(),
        Value::Array(option.updates.iter().map(trust_update_to_json).collect()),
    );
    if let Some(saved_path) = &option.saved_path {
        obj.insert("savedPath".to_string(), Value::String(saved_path.clone()));
    }
    Value::Object(obj)
}

/// `getProjectTrustParentPath` (trust-manager.ts): the nearest ancestor path, or
/// `null` at a filesystem root (pi's `undefined`).
#[napi(js_name = "getProjectTrustParentPath")]
pub fn get_project_trust_parent_path(cwd: String) -> Option<String> {
    atilla_coding::core::trust_manager::get_project_trust_parent_path(&cwd)
}

/// `getProjectTrustOptions` (trust-manager.ts): the ordered trust options for
/// `cwd`, as a JSON array. The shim supplies pi's `{ includeSessionOnly }` default.
#[napi(js_name = "getProjectTrustOptions")]
pub fn get_project_trust_options(cwd: String, include_session_only: bool) -> napi::Result<String> {
    let options =
        atilla_coding::core::trust_manager::get_project_trust_options(&cwd, include_session_only);
    let array: Vec<Value> = options.iter().map(trust_option_to_json).collect();
    serde_json::to_string(&Value::Array(array)).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// `hasTrustRequiringProjectResources` (trust-manager.ts): whether `cwd` carries
/// project-local resources that must be gated by trust. The shim passes pi's
/// `process.env.HOME || homedir()` as `home_dir`.
#[napi(js_name = "hasTrustRequiringProjectResources")]
pub fn has_trust_requiring_project_resources(cwd: String, home_dir: String) -> bool {
    atilla_coding::core::trust_manager::has_trust_requiring_project_resources_with_home(
        &cwd, &home_dir,
    )
}

/// `ProjectTrustStore.getEntry` (trust-manager.ts): the nearest recorded trust
/// entry for `cwd`, as JSON `{ path, decision }`, or `null`. Errors cross as
/// thrown JS errors (pi throws on an unreadable/invalid store).
#[napi(js_name = "trustStoreGetEntry")]
pub fn trust_store_get_entry(agent_dir: String, cwd: String) -> napi::Result<Option<String>> {
    let store = atilla_coding::core::trust_manager::ProjectTrustStore::new(&agent_dir);
    match store
        .get_entry(&cwd)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?
    {
        None => Ok(None),
        Some(entry) => {
            let mut obj = serde_json::Map::new();
            obj.insert("path".to_string(), Value::String(entry.path));
            obj.insert("decision".to_string(), Value::Bool(entry.decision));
            Ok(Some(Value::Object(obj).to_string()))
        }
    }
}

/// `ProjectTrustStore.setMany` (trust-manager.ts): apply a batch of trust updates
/// (a JSON array of `{ path, decision }`, `decision: boolean | null`). Errors
/// cross as thrown JS errors.
#[napi(js_name = "trustStoreSetMany")]
pub fn trust_store_set_many(agent_dir: String, updates_json: String) -> napi::Result<()> {
    #[derive(serde::Deserialize)]
    struct TrustUpdateJson {
        path: String,
        decision: Option<bool>,
    }
    let updates: Vec<TrustUpdateJson> = serde_json::from_str(&updates_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid trust updates: {e}")))?;
    let mapped: Vec<atilla_coding::core::trust_manager::ProjectTrustUpdate> = updates
        .into_iter()
        .map(|u| atilla_coding::core::trust_manager::ProjectTrustUpdate {
            path: u.path,
            decision: u.decision,
        })
        .collect();
    let store = atilla_coding::core::trust_manager::ProjectTrustStore::new(&agent_dir);
    store
        .set_many(&mapped)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

// --- coding-agent core: keybindings -----------------------------------------
//
// Thin wrappers over `atilla_coding::core::keybindings`, backing the native
// `core/keybindings.ts` shim. The default table and legacy-name migration cross
// as JSON in pi's exact camelCase shape (`{ defaultKeys, description }`,
// `{ config, migrated }`); `IndexMap` preserves pi's source order, which the
// migration file-rewrite depends on. pi-tui's `KeybindingsManager` base
// (resolution + `matches()`) is a separate, still-original module, so the shim
// keeps extending it and only swaps in this native default table and migration.

/// `keybindingsFor` (keybindings.ts): the ordered default keybinding table for a
/// `process.platform` string, as pi's `KeybindingDefinitions` JSON. Order is
/// preserved so `orderKeybindingsConfig`/`getResolvedBindings` stay faithful.
#[napi(js_name = "keybindingsFor")]
pub fn keybindings_for(platform: String) -> napi::Result<String> {
    use atilla_coding::core::keybindings::Platform;
    let target = match platform.as_str() {
        "win32" => Platform::Windows,
        "darwin" => Platform::Macos,
        _ => Platform::Other,
    };
    let definitions = atilla_coding::core::keybindings::keybindings_for(target);
    let mut out: IndexMap<String, Value> = IndexMap::new();
    for (id, definition) in definitions {
        let keys = serde_json::to_value(&definition.default_keys)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let mut obj = serde_json::Map::new();
        obj.insert("defaultKeys".to_string(), keys);
        obj.insert(
            "description".to_string(),
            Value::String(definition.description.to_string()),
        );
        out.insert(id, Value::Object(obj));
    }
    serde_json::to_string(&out).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// `migrateKeybindingsConfig` (keybindings.ts): rewrite legacy flat key names to
/// namespaced ids. Takes pi's raw config as a JSON object, returns
/// `{ config, migrated }` JSON with key order preserved (`IndexMap`).
#[napi(js_name = "migrateKeybindingsConfig")]
pub fn migrate_keybindings_config(raw_json: String) -> napi::Result<String> {
    let raw: IndexMap<String, Value> = serde_json::from_str(&raw_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid keybindings config: {e}")))?;
    let (config, migrated) = atilla_coding::core::keybindings::migrate_keybindings_config(&raw);
    let config_str =
        serde_json::to_string(&config).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(format!(
        "{{\"config\":{config_str},\"migrated\":{migrated}}}"
    ))
}

// --- tui fuzzy layer (packages/tui/src/fuzzy.ts) ---------------------------
//
// Thin wrappers over `atilla_tui::fuzzy`, backing the hand-written native
// `fuzzy.ts` shim. `fuzzyMatch` crosses as a plain `{ matches, score }`
// object; the shim re-implements `fuzzyFilter` in JS on top of this (pi's
// `fuzzyFilter` takes a `getText` callback, which cannot cross the boundary).

/// Result of [`fuzzy_match`]; serialized to pi's `{ matches, score }`.
#[napi(object)]
pub struct FuzzyMatchResult {
    pub matches: bool,
    pub score: f64,
}

/// `fuzzyMatch` (fuzzy.ts): fuzzy-match `query` against `text`, returning pi's
/// `{ matches, score }` (lower score = better).
#[napi(js_name = "fuzzyMatch")]
pub fn fuzzy_match(query: String, text: String) -> FuzzyMatchResult {
    let m = atilla_tui::fuzzy_match(&query, &text);
    FuzzyMatchResult {
        matches: m.matches,
        score: m.score,
    }
}

// --- tui word-navigation layer (packages/tui/src/word-navigation.ts) --------
//
// Thin wrappers over `atilla_tui::word_navigation`, backing the native
// `word-navigation.ts` shim. Cursors are UTF-16 string indices (as in pi). The
// napi surface covers only the default-segmenter path; the shim delegates to
// pi's original when `options.segment`/`options.isAtomicSegment` are supplied
// (JS callbacks that cannot cross the boundary).

/// `findWordBackward` (word-navigation.ts), default segmentation: cursor after
/// moving one word backward from `cursor` (UTF-16 index).
#[napi(js_name = "findWordBackward")]
pub fn find_word_backward(text: String, cursor: u32) -> u32 {
    atilla_tui::find_word_backward(
        &text,
        cursor as usize,
        &atilla_tui::WordNavOptions::default(),
    ) as u32
}

/// `findWordForward` (word-navigation.ts), default segmentation: cursor after
/// moving one word forward from `cursor` (UTF-16 index).
#[napi(js_name = "findWordForward")]
pub fn find_word_forward(text: String, cursor: u32) -> u32 {
    atilla_tui::find_word_forward(
        &text,
        cursor as usize,
        &atilla_tui::WordNavOptions::default(),
    ) as u32
}

// --- tui truncated-text layer (packages/tui/src/components/truncated-text.ts)
//
// Thin wrapper over `atilla_tui::truncated_text_render`, backing the native
// `truncated-text.ts` shim. The shim re-implements pi's `TruncatedText` class
// (constructor + `invalidate`) and delegates `render(width)` here.

/// `TruncatedText.render` (truncated-text.ts): render `text` truncated to
/// `width` columns with horizontal/vertical padding, ANSI-aware.
#[napi(js_name = "truncatedTextRender")]
pub fn truncated_text_render(
    text: String,
    padding_x: u32,
    padding_y: u32,
    width: u32,
) -> Vec<String> {
    atilla_tui::truncated_text_render(
        &text,
        padding_x as usize,
        padding_y as usize,
        width as usize,
    )
}

// --- tui markdown layer (packages/tui/src/components/markdown.ts) -----------
//
// Thin wrapper over `atilla_tui::markdown_render`, backing the native
// `markdown.ts` shim. `markdown_render` bakes in pi's default markdown theme at
// chalk level 3 with zero padding and no options, so the shim delegates
// `render(width)` here only when the constructed `Markdown` matches that shape
// (default theme, no padding, no default text style, no options) and otherwise
// falls back to pi's original class.

/// `Markdown.render` (markdown.ts) on the default-theme path: render `source`
/// wrapped to `width` columns with pi's `defaultMarkdownTheme` (chalk level 3).
#[napi(js_name = "markdownRender")]
pub fn markdown_render(source: String, width: u32) -> Vec<String> {
    atilla_tui::markdown_render(&source, width as usize)
}

// --- tui keybindings layer (packages/tui/src/keybindings.ts) ----------------
//
// A stateful `#[napi]` class wrapping `atilla_tui::KeybindingsManager`. The
// hand-written `keybindings.ts` shim re-implements pi's `KeybindingsManager`
// class (keeping `definitions`/`userBindings`/`getDefinition`/`getUserBindings`
// as JS, identical to pi) and routes the resolution logic — `matches`,
// `getKeys`, `getConflicts`, `getResolvedBindings` — through this core. The core
// is immutable per construction; the shim's `setUserBindings` builds a fresh
// core. Definitions and user bindings cross as JSON arrays (not objects) so
// JS insertion order is preserved without relying on serde_json's
// `preserve_order` feature.

#[derive(serde::Deserialize)]
struct KeybindingDefinitionIn {
    id: String,
    #[serde(rename = "defaultKeys")]
    default_keys: Vec<String>,
    description: Option<String>,
}

#[derive(serde::Deserialize)]
struct UserBindingIn {
    id: String,
    // `null` = pi's explicit `undefined` (falls back to the default keys).
    keys: Option<Vec<String>>,
}

/// The Rust-backed keybindings core, exposed to JavaScript as
/// `KeybindingsManagerCore`.
#[napi(js_name = "KeybindingsManagerCore")]
pub struct KeybindingsManagerCore {
    inner: atilla_tui::KeybindingsManager,
}

#[napi]
impl KeybindingsManagerCore {
    /// Build a core from pi's `definitions`/`userBindings`, each JSON-encoded as
    /// an ordered array (`[{ id, defaultKeys, description? }]` and
    /// `[{ id, keys }]`, `keys: null` for an explicit `undefined`).
    #[napi(constructor)]
    pub fn new(definitions_json: String, user_bindings_json: String) -> napi::Result<Self> {
        let defs_in: Vec<KeybindingDefinitionIn> = serde_json::from_str(&definitions_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid definitions: {e}")))?;
        let user_in: Vec<UserBindingIn> = serde_json::from_str(&user_bindings_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid userBindings: {e}")))?;

        let defs_owned: Vec<(String, atilla_tui::KeybindingDefinition)> = defs_in
            .into_iter()
            .map(|d| {
                (
                    d.id,
                    atilla_tui::KeybindingDefinition {
                        default_keys: d.default_keys,
                        description: d.description,
                    },
                )
            })
            .collect();
        let definitions: Vec<(&str, atilla_tui::KeybindingDefinition)> = defs_owned
            .iter()
            .map(|(id, def)| (id.as_str(), def.clone()))
            .collect();
        let user_bindings: Vec<(&str, Option<Vec<String>>)> = user_in
            .iter()
            .map(|u| (u.id.as_str(), u.keys.clone()))
            .collect();

        Ok(Self {
            inner: atilla_tui::KeybindingsManager::new(definitions, user_bindings),
        })
    }

    /// pi's `matches(data, keybinding)`: does `data` match any bound key?
    #[napi(js_name = "matches")]
    pub fn matches(&self, data: String, keybinding: String) -> bool {
        self.inner.matches(&data, &keybinding)
    }

    /// pi's `getKeys(keybinding)`: the keys bound to `keybinding` (empty if
    /// unknown).
    #[napi(js_name = "getKeys")]
    pub fn get_keys(&self, keybinding: String) -> Vec<String> {
        self.inner.get_keys(&keybinding)
    }

    /// pi's `getConflicts()` as JSON: an ordered array of
    /// `{ key, keybindings }`.
    #[napi(js_name = "getConflictsJson")]
    pub fn get_conflicts_json(&self) -> napi::Result<String> {
        let conflicts: Vec<serde_json::Value> = self
            .inner
            .get_conflicts()
            .into_iter()
            .map(|c| serde_json::json!({ "key": c.key, "keybindings": c.keybindings }))
            .collect();
        serde_json::to_string(&conflicts).map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// pi's `getResolvedBindings()` as JSON: an ordered array of `[id, keys]`
    /// pairs, in definition order. The shim rebuilds pi's `key | key[]` shape.
    #[napi(js_name = "getResolvedBindingsJson")]
    pub fn get_resolved_bindings_json(&self) -> napi::Result<String> {
        let resolved: Vec<(String, Vec<String>)> = self.inner.get_resolved_bindings();
        serde_json::to_string(&resolved).map_err(|e| napi::Error::from_reason(e.to_string()))
    }
}
