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
