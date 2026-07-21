//! Node-API bridge for pidgin, built with napi-rs as a `cdylib` addon.
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
// Generated from schema/api.json via fluessig — see src/generated.rs and the
// `FauxCoreImpl` engine seam in src/core_impl.rs.

// TUI renderer surface (`TuiCore`): drives pi's differential render path
// (`TUI::doRender`) natively. The JS shim feeds pre-rendered lines in and drains
// the write stream out; overlays/focus/input stay in pi's TS. Additive.
mod tui;

// TUI stdin-buffer surface (`StdinBufferCore`): drives pi's `StdinBuffer`
// escape-sequence splitter / bracketed-paste / Kitty-dedup state machine
// natively. The JS shim keeps only pi's EventEmitter plumbing, the completion
// timer, and Buffer adaptation; every splitting decision runs in Rust. Now
// generated from the fluessig api schema through `crate::generated` +
// `crate::core_impl` (the `StdinBufferCoreImpl` engine seam) — see
// src/generated.rs and schema/api.json. Additive.

// TUI terminal-colors surface (`isOsc11BackgroundColorResponse`,
// `parseOsc11BackgroundColor`, `parseTerminalColorSchemeReport`): pi's pure
// terminal color parsers (`terminal-colors.ts`) run natively. `pub` so the
// module's free `#[napi]` functions register as crate-reachable. Additive.
pub mod terminal_colors;

// Coding-agent compaction-utils surface (`serializeConversation`): drives pi's
// message-to-text serializer (`compaction/utils.ts`) natively via the Rust port
// (`pidgin_coding::core::compaction::serialize_conversation`). The JS shim
// re-exports the module's un-flipped file-op helpers from pi's original and
// delegates only `serializeConversation` to Rust. Additive.
pub mod compaction_utils;

// TUI terminal-image surface (`isImageLine`, `encodeKitty`, `renderImage`, the
// image-header parsers, capability/cell-dimension state, and `hyperlink`):
// drives pi's `terminal-image.ts` graphics helpers natively. Every export the
// suite touches runs in Rust; only `detectCapabilities` (which takes a JS
// closure) stays in pi's TS. Additive.
pub mod terminal_image;

// The OAuth flow surface (`OAuthFlowCore`, `DeviceCodePollCore`), driving the
// Rust OAuth login/refresh and device-code poll state machines from JS. Additive.
mod oauth;

// Exec-tool bindings (`lsToolExecute`, `writeToolExecute`, `bashToolExecute`)
// backing the native ls/write/bash conformance shims' default (local) path.
// Additive; the async run layer is driven via `block_on`. The `#[napi]` export
// wrappers live here (crate root) — the thin impls are in `tools`.
mod tools;

// The package-manager command flow (`CommandCore`) now generates from the
// fluessig api schema through `crate::generated` + `crate::core_impl` (the
// `CommandCoreImpl` behind an interior-mutability `Mutex`, driving the Rust
// command-flow state machines in `pidgin_coding::core::package_manager` behind a
// JSON in/out driver loop). The hand-written `#[napi]` class was retired.

// The coding-agent session-cwd surface (`getMissingSessionCwdIssue`,
// `formatMissingSessionCwdError`, `formatMissingSessionCwdPrompt`) plus its
// `SessionCwdIssueJs` DTO is now fluessig-generated (`src/generated.rs`); the
// engine seam and DTO conversions live in `core_impl`.

// The tui autocomplete provider (`AutocompleteCore`): wraps pi's
// `CombinedAutocompleteProvider` over a native `FileProvider` (std::fs + real
// `fd`), backing the native `autocomplete.ts` shim. Additive.
mod autocomplete;

// The provider error-body normalizer surface (`normalizeProviderError`,
// `formatProviderError`, `truncateErrorText`): drives pi's provider HTTP
// error-body field-probe / truncation / compose logic natively. The JS shim only
// splits `Error` vs non-`Error` and plucks the SDK carrier fields; every decision
// runs in Rust. pi's `safeJsonStringify` (JS-runtime JSON.stringify semantics)
// stays in the shim's TS. Additive. Public like `agent` so the free `#[napi]`
// export functions are reachable from the crate root (not dead under `--test`).
pub mod error_body;

/// `createLsTool(...).execute` default path (`ls.ts`): list a directory through
/// the native `run_ls` port, returning pi's `AgentToolResult` JSON. See
/// [`tools::ls_execute`].
///
/// Exported to JavaScript as `lsToolExecute`.
#[napi(js_name = "lsToolExecute")]
pub fn ls_tool_execute(cwd: String, input_json: String) -> napi::Result<String> {
    tools::ls_execute(cwd, input_json)
}

/// `createWriteTool(...).execute` default path (`write.ts`): write a file through
/// the native `run_write` port and mutation queue. See [`tools::write_execute`].
///
/// Exported to JavaScript as `writeToolExecute`.
#[napi(js_name = "writeToolExecute")]
pub fn write_tool_execute(cwd: String, input_json: String) -> napi::Result<String> {
    tools::write_execute(cwd, input_json)
}

/// `createBashTool(...).execute` default path (`bash.ts`): run a command through
/// the native `BashTool` local-shell path. See [`tools::bash_execute`].
///
/// Exported to JavaScript as `bashToolExecute`.
#[napi(js_name = "bashToolExecute")]
pub fn bash_tool_execute(cwd: String, input_json: String) -> napi::Result<String> {
    tools::bash_execute(cwd, input_json)
}

// Bridge slice 1: the first Rust→JS blocking callback bridge (`AgentBridge`),
// driving the Rust agent loop while live JS closures fire mid-run. Additive.
mod agent_bridge;

// Agent-tier exports (`crates/pidgin-agent`), namespaced in their own module so
// the agent flips stay merge-clean beside the coding-agent/ai exports here.
// `pub` so the module's free `#[napi]` functions register as crate-reachable
// (matching the crate-root exports); otherwise `--all-targets` clippy reads them
// as dead code in the lib-test target.
pub mod agent;

// The fluessig-generated napi surface (`crate::generated`) + its hand-written
// engine seam (`crate::core_impl`). The `version/core` export
// (`pidginNativeVersion`) is generated from `schema/api.json` and routes through
// the `PidginCore` trait — do not add hand-written `#[napi]` exports here that a
// schema op can describe; edit the schema and rerun `regen.sh` instead.
//
// `pub mod` so the generated free `#[napi]` functions register as crate-reachable
// (matching the other flipped modules). The generated file's own banner carries
// `#![allow(unused_imports)]`, and fluessig's napi-2 prelude now emits only the
// imports the surface actually uses, so no module-level allow is needed here.
mod core_impl;
pub mod generated;

/// Parse an Anthropic Messages SSE body into the uniform assistant-message event
/// stream and final message, backed by
/// [`pidgin_ai::api::anthropic::parse_sse_stream_to_json`].
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
    pidgin_ai::api::anthropic::parse_sse_stream_to_json(&body, &model, is_oauth, timestamp as i64)
        .map_err(napi::Error::from_reason)
}

// --- tui width layer (packages/tui/src/utils.ts) ---------------------------
//
// MODULE 4 (utils): the six width ops (`visibleWidth`, `normalizeTerminalOutput`,
// `truncateToWidth`, `wrapTextWithAnsi`, `sliceWithWidth`, and `extractSegments`)
// plus their two result DTOs (`SliceWithWidth`, `ExtractSegmentsResult`) now
// generate from the fluessig api schema through `crate::generated` +
// `crate::core_impl`, routing into the `pidgin_tui` width layer. Numeric
// params/returns are authored as `int32` (JS `number`) and widened to the
// engine's `i64`/`usize` at the core seam. The hand-written `#[napi]` exports
// that lived here were deleted; edit `schema/api.json` and rerun `regen.sh`
// instead of re-adding them.

// --- tui key layer (packages/tui/src/keys.ts) ------------------------------
//
// MODULE 3 (keys): the five key ops (`parseKey`, `matchesKey`,
// `decodeKittyPrintable`, `decodePrintableKey`, and `setKittyProtocolActive`)
// now generate from the fluessig api schema through `crate::generated` +
// `crate::core_impl`, routing into `pidgin_tui::keys`. The hand-written `#[napi]`
// exports that lived here were deleted; edit `schema/api.json` and rerun
// `regen.sh` instead of re-adding them.

// --- coding-agent utils layer -----------------------------------------------
//
// The `detectSupportedImageMimeType` byte-sniffer (utils/mime.ts) now generates
// from the fluessig api schema through `crate::generated` + `crate::core_impl`,
// routing into `pidgin_coding::utils::mime`. Its image buffer arg is authored as
// the fluessig `bytes` scalar (spelled `Uint8Array` in the node `.d.ts`). The
// hand-written `#[napi]` export that lived here was deleted; edit `schema/api.json`
// and rerun `regen.sh` instead of re-adding it.

// --- coding-agent http-dispatcher layer -------------------------------------
//
// Thin wrappers over `pidgin_coding::core::http_dispatcher`, backing the native
// `core/http-dispatcher.ts` shim's idle-timeout parse/format helpers. pi's
// `parseHttpIdleTimeoutMs(value: unknown)` accepts a number OR a string; the two
// typeof branches are exposed here as separate exports and recombined in the
// shim. `Option<u64>` (pi's `number | undefined`) crosses as `f64 | null`; the
// shim converts `null` → `undefined`.

/// `parseHttpIdleTimeoutMs` (core/http-dispatcher.ts) numeric branch: normalize
/// a numeric idle-timeout. Non-finite or negative → `null`, else floored.
#[napi(js_name = "parseHttpIdleTimeoutMsFromNumber")]
pub fn parse_http_idle_timeout_ms_from_number(value: f64) -> Option<f64> {
    pidgin_coding::core::http_dispatcher::parse_http_idle_timeout_num(value).map(|ms| ms as f64)
}

/// `parseHttpIdleTimeoutMs` (core/http-dispatcher.ts) string branch:
/// `"disabled"` (case-insensitive) → `0`, empty/whitespace or non-numeric →
/// `null`, else the numeric value floored.
#[napi(js_name = "parseHttpIdleTimeoutMsFromString")]
pub fn parse_http_idle_timeout_ms_from_string(value: String) -> Option<f64> {
    pidgin_coding::core::http_dispatcher::parse_http_idle_timeout_ms(&value).map(|ms| ms as f64)
}

/// `formatHttpIdleTimeoutMs` (core/http-dispatcher.ts): a preset label when one
/// matches, else `"<seconds> sec"`.
#[napi(js_name = "formatHttpIdleTimeoutMs")]
pub fn format_http_idle_timeout_ms(timeout_ms: f64) -> String {
    pidgin_coding::core::http_dispatcher::format_http_idle_timeout_ms(timeout_ms as u64)
}

// --- coding-agent export-html layer -----------------------------------------
//
// Thin wrappers over `pidgin_coding::core::export_html::ansi_to_html`, backing
// the native `core/export-html/ansi-to-html.ts` shim.

/// `ansiToHtml` (core/export-html/ansi-to-html.ts): convert ANSI-escaped text to
/// HTML with inline styles.
#[napi(js_name = "ansiToHtml")]
pub fn ansi_to_html(text: String) -> String {
    pidgin_coding::core::export_html::ansi_to_html::ansi_to_html(&text)
}

/// `ansiLinesToHtml` (core/export-html/ansi-to-html.ts): convert an array of
/// ANSI-escaped lines to HTML, wrapping each in an `ansi-line` div.
#[napi(js_name = "ansiLinesToHtml")]
pub fn ansi_lines_to_html(lines: Vec<String>) -> String {
    pidgin_coding::core::export_html::ansi_to_html::ansi_lines_to_html(&lines)
}

// --- coding-agent tools: truncate ------------------------------------------
//
// Thin wrappers over `pidgin_coding::core::tools::truncate`, backing the native
// `core/tools/truncate.ts` shim. Structured results cross as JSON strings using
// pi's exact `TruncationResult` field names; the shim `JSON.parse`s them and
// re-adds pi's JS default arguments (which the Rust port dropped).

/// Serialize a Rust `TruncationResult` into pi's `TruncationResult` JSON shape,
/// mapping the `TruncatedBy` enum + `Option` to pi's `"lines" | "bytes" | null`.
fn truncation_result_to_json(r: &pidgin_coding::core::tools::truncate::TruncationResult) -> String {
    use pidgin_coding::core::tools::truncate::TruncatedBy;
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
    pidgin_coding::core::tools::truncate::format_size(bytes.max(0) as usize)
}

/// `truncateHead` (core/tools/truncate.ts): keep the first N lines/bytes. The
/// shim supplies `maxLines`/`maxBytes` (its defaulted `TruncationOptions`).
#[napi(js_name = "truncateHead")]
pub fn truncate_head(content: String, max_lines: i64, max_bytes: i64) -> String {
    use pidgin_coding::core::tools::truncate::{truncate_head, TruncationOptions};
    let opts = TruncationOptions {
        max_lines: max_lines.max(0) as usize,
        max_bytes: max_bytes.max(0) as usize,
    };
    truncation_result_to_json(&truncate_head(&content, opts))
}

/// `truncateTail` (core/tools/truncate.ts): keep the last N lines/bytes.
#[napi(js_name = "truncateTail")]
pub fn truncate_tail(content: String, max_lines: i64, max_bytes: i64) -> String {
    use pidgin_coding::core::tools::truncate::{truncate_tail, TruncationOptions};
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
    let r = pidgin_coding::core::tools::truncate::truncate_line(&line, max_chars.max(0) as usize);
    serde_json::json!({ "text": r.text, "wasTruncated": r.was_truncated }).to_string()
}

// --- coding-agent tools: edit-diff -----------------------------------------
//
// Thin wrappers over `pidgin_coding::core::tools::edit_diff`, backing the native
// `core/tools/edit-diff.ts` shim. The `LineEnding` enum crosses as pi's
// `"\r\n" | "\n"` union; structured results cross as JSON strings with pi's
// exact field names. The async `computeEditsDiff`/`computeEditDiff` are not
// ported and stay in pi's original.

fn line_ending_to_str(ending: pidgin_coding::core::tools::edit_diff::LineEnding) -> &'static str {
    match ending {
        pidgin_coding::core::tools::edit_diff::LineEnding::Crlf => "\r\n",
        pidgin_coding::core::tools::edit_diff::LineEnding::Lf => "\n",
    }
}

/// `detectLineEnding` (core/tools/edit-diff.ts): detect the dominant line
/// ending, returning pi's `"\r\n" | "\n"` union.
#[napi(js_name = "detectLineEnding")]
pub fn detect_line_ending(content: String) -> String {
    line_ending_to_str(pidgin_coding::core::tools::edit_diff::detect_line_ending(
        &content,
    ))
    .to_string()
}

/// `normalizeToLF` (core/tools/edit-diff.ts): normalize all line endings to
/// `\n`.
#[napi(js_name = "normalizeToLf")]
pub fn normalize_to_lf(text: String) -> String {
    pidgin_coding::core::tools::edit_diff::normalize_to_lf(&text)
}

/// `restoreLineEndings` (core/tools/edit-diff.ts): restore `\n` to `ending`. The
/// shim passes pi's `"\r\n" | "\n"` union as `ending`.
#[napi(js_name = "restoreLineEndings")]
pub fn restore_line_endings(text: String, ending: String) -> String {
    use pidgin_coding::core::tools::edit_diff::LineEnding;
    let le = if ending == "\r\n" {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    };
    pidgin_coding::core::tools::edit_diff::restore_line_endings(&text, le)
}

/// `normalizeForFuzzyMatch` (core/tools/edit-diff.ts): NFKC + trailing-ws strip
/// + smart quote/dash/space folding.
#[napi(js_name = "normalizeForFuzzyMatch")]
pub fn normalize_for_fuzzy_match(text: String) -> String {
    pidgin_coding::core::tools::edit_diff::normalize_for_fuzzy_match(&text)
}

/// `stripBom` (core/tools/edit-diff.ts): strip a leading UTF-8 BOM, returning
/// pi's `{ bom, text }` shape as JSON.
#[napi(js_name = "stripBom")]
pub fn strip_bom(content: String) -> String {
    let r = pidgin_coding::core::tools::edit_diff::strip_bom(&content);
    serde_json::json!({ "bom": r.bom, "text": r.text }).to_string()
}

/// `fuzzyFindText` (core/tools/edit-diff.ts): exact-then-fuzzy search, returning
/// pi's `FuzzyMatchResult` shape as JSON. Offsets are byte-based (as in the Rust
/// port); pi's tests do not deep-index the returned offset.
#[napi(js_name = "fuzzyFindText")]
pub fn fuzzy_find_text(content: String, old_text: String) -> String {
    let r = pidgin_coding::core::tools::edit_diff::fuzzy_find_text(&content, &old_text);
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
    use pidgin_coding::core::tools::edit_diff::PreservingReplacement;
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
    pidgin_coding::core::tools::edit_diff::apply_replacements_preserving_unchanged_lines(
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
    use pidgin_coding::core::tools::edit_diff::Edit;
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
    let r = pidgin_coding::core::tools::edit_diff::apply_edits_to_normalized_content(
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
    pidgin_coding::core::tools::edit_diff::generate_unified_patch(
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
    let r = pidgin_coding::core::tools::edit_diff::generate_diff_string(
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

// --- coding-agent tools: path-utils -----------------------------------------
//
// MODULE 2 (path-utils): the five path ops (`expandPath`, `resolveToCwd`, and
// the three private macOS filename transforms) now generate from the fluessig
// api schema through `crate::generated` + `crate::core_impl`. The hand-written
// `#[napi]` exports that lived here were deleted; edit `schema/api.json` and
// rerun `regen.sh` instead of re-adding them.

// --- coding-agent core: resolve-config-value --------------------------------
//
// Thin wrappers over `pidgin_coding::core::resolve_config_value`, backing the
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
    Ok(pidgin_coding::core::resolve_config_value::resolve_config_value(&config, env.as_ref()))
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
        pidgin_coding::core::resolve_config_value::resolve_config_value_uncached(
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
    pidgin_coding::core::resolve_config_value::resolve_config_value_or_throw(
        &config,
        &description,
        env.as_ref(),
    )
    .map_err(|e| napi::Error::from_reason(e.to_string()))
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
        pidgin_coding::core::resolve_config_value::get_missing_config_value_env_var_names(
            &config,
            env.as_ref(),
        ),
    )
}

/// `isConfigValueConfigured` (resolve-config-value.ts): whether every env var a
/// value references is set.
#[napi(js_name = "isConfigValueConfigured")]
pub fn is_config_value_configured(config: String, env: Option<String>) -> napi::Result<bool> {
    let env = parse_config_env(env)?;
    Ok(
        pidgin_coding::core::resolve_config_value::is_config_value_configured(
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
    headers_json(pidgin_coding::core::resolve_config_value::resolve_headers(
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
    let resolved = pidgin_coding::core::resolve_config_value::resolve_headers_or_throw(
        headers.as_ref(),
        &description,
        env.as_ref(),
    )
    .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    headers_json(resolved)
}

// --- coding-agent core: trust-manager ---------------------------------------
//
// Thin wrappers over `pidgin_coding::core::trust_manager`, backing the native
// `core/trust-manager.ts` shim. Structured values cross as JSON using pi's exact
// field names (`{ path, decision }`, `{ label, trusted, updates, savedPath? }`);
// the shim `JSON.parse`s them. `ProjectTrustStore` stays a JS class holding the
// agent dir, delegating each method to the stateless functions below (each
// reconstructs the Rust store, whose only state is the on-disk `trust.json`).

/// Serialize a Rust `ProjectTrustUpdate` into pi's `{ path, decision }` shape,
/// mapping `Option<bool>` to pi's `boolean | null`.
fn trust_update_to_json(update: &pidgin_coding::core::trust_manager::ProjectTrustUpdate) -> Value {
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
fn trust_option_to_json(option: &pidgin_coding::core::trust_manager::ProjectTrustOption) -> Value {
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

/// `getProjectTrustOptions` (trust-manager.ts): the ordered trust options for
/// `cwd`, as a JSON array. The shim supplies pi's `{ includeSessionOnly }` default.
#[napi(js_name = "getProjectTrustOptions")]
pub fn get_project_trust_options(cwd: String, include_session_only: bool) -> napi::Result<String> {
    let options =
        pidgin_coding::core::trust_manager::get_project_trust_options(&cwd, include_session_only);
    let array: Vec<Value> = options.iter().map(trust_option_to_json).collect();
    serde_json::to_string(&Value::Array(array)).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// `ProjectTrustStore.getEntry` (trust-manager.ts): the nearest recorded trust
/// entry for `cwd`, as JSON `{ path, decision }`, or `null`. Errors cross as
/// thrown JS errors (pi throws on an unreadable/invalid store).
#[napi(js_name = "trustStoreGetEntry")]
pub fn trust_store_get_entry(agent_dir: String, cwd: String) -> napi::Result<Option<String>> {
    let store = pidgin_coding::core::trust_manager::ProjectTrustStore::new(&agent_dir);
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
    let mapped: Vec<pidgin_coding::core::trust_manager::ProjectTrustUpdate> = updates
        .into_iter()
        .map(|u| pidgin_coding::core::trust_manager::ProjectTrustUpdate {
            path: u.path,
            decision: u.decision,
        })
        .collect();
    let store = pidgin_coding::core::trust_manager::ProjectTrustStore::new(&agent_dir);
    store
        .set_many(&mapped)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

// --- coding-agent core: keybindings -----------------------------------------
//
// Thin wrappers over `pidgin_coding::core::keybindings`, backing the native
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
    use pidgin_coding::core::keybindings::Platform;
    let target = match platform.as_str() {
        "win32" => Platform::Windows,
        "darwin" => Platform::Macos,
        _ => Platform::Other,
    };
    let definitions = pidgin_coding::core::keybindings::keybindings_for(target);
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
    let (config, migrated) = pidgin_coding::core::keybindings::migrate_keybindings_config(&raw);
    let config_str =
        serde_json::to_string(&config).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(format!(
        "{{\"config\":{config_str},\"migrated\":{migrated}}}"
    ))
}

// --- tui fuzzy layer (packages/tui/src/fuzzy.ts) ---------------------------
//
// The two fuzzy ops (`fuzzyMatch`, `fuzzyFilter`) now generate from the fluessig
// api schema through `crate::generated` + `crate::core_impl`, routing into
// `pidgin_tui`'s fuzzy layer. `fuzzyMatch` returns `FuzzyMatchResult`, whose
// `score` crosses as `float64` (JS `number`); `fuzzyFilter` returns the ranked
// surviving indices as `uint32` (JS `number`), widened from the engine's `usize`
// at the core seam. `fuzzyFilter`'s shim materializes each item's text via its
// `getText` callback in JS and maps the returned indices back to items, so pi's
// `getText` stays JS-side while the whole tokenize/AND-gate/score-sum/sort
// orchestration runs in Rust. The hand-written `#[napi]` exports that lived here
// were deleted; edit `schema/api.json` and rerun `regen.sh` instead of re-adding
// them.

// --- tui word-navigation layer (packages/tui/src/word-navigation.ts) --------
//
// MODULE 5 (word-navigation): the two word-navigation ops (`findWordBackward`,
// `findWordForward`) now generate from the fluessig api schema through
// `crate::generated` + `crate::core_impl`, routing into
// `pidgin_tui::word_navigation`'s default-segmenter path. Cursors are UTF-16
// string indices authored as `int32` (JS `number`) and widened to the engine's
// `usize` at the core seam. The shim still delegates to pi's original when
// `options.segment`/`options.isAtomicSegment` are supplied (JS callbacks that
// cannot cross the boundary). The hand-written `#[napi]` exports that lived here
// were deleted; edit `schema/api.json` and rerun `regen.sh` instead of re-adding
// them.

// --- tui truncated-text layer (packages/tui/src/components/truncated-text.ts)
//
// Thin wrapper over `pidgin_tui::truncated_text_render`, backing the native
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
    pidgin_tui::truncated_text_render(
        &text,
        padding_x as usize,
        padding_y as usize,
        width as usize,
    )
}

// --- tui markdown layer (packages/tui/src/components/markdown.ts) -----------
//
// Thin wrapper over `pidgin_tui::markdown_render`, backing the native
// `markdown.ts` shim. `markdown_render` bakes in pi's default markdown theme at
// chalk level 3 with zero padding and no options, so the shim delegates
// `render(width)` here only when the constructed `Markdown` matches that shape
// (default theme, no padding, no default text style, no options) and otherwise
// falls back to pi's original class.

/// `Markdown.render` (markdown.ts) on the default-theme path: render `source`
/// wrapped to `width` columns with pi's `defaultMarkdownTheme` (chalk level 3).
#[napi(js_name = "markdownRender")]
pub fn markdown_render(source: String, width: u32) -> Vec<String> {
    pidgin_tui::markdown_render(&source, width as usize)
}

// --- tui input layer (packages/tui/src/components/input.ts) -----------------
//
// The single-line `Input` component (`InputCore`) now GENERATES from the
// fluessig api schema through `crate::generated` + `crate::core_impl` (the
// `InputCoreImpl` engine seam). It is authored `#[fluessig(single_threaded)]`,
// so the generated handle holds the `!Send` core (pi's `Rc<RefCell<…>>` event
// cell plus non-`Send` `onSubmit`/`onEscape` closures) THREAD-CONFINED in a
// `RefCell<Impl>` — no `Arc`, no `Send`/`Sync` bound — rather than the default
// `Arc<Impl>` handle a `Send` core would use. The JS `input.ts` shim keeps
// `onSubmit`/`onEscape` as JS callbacks and the `focused` accessor as JS, and
// replays the [`InputEvent`] the core returns from `handleInput` onto them —
// unchanged by the swap. See src/generated.rs and schema/api.json. Additive.

// --- tui select-list layer (packages/tui/src/components/select-list.ts) -----
//
// A stateful `#[napi]` class wrapping `pidgin_tui::SelectList`. pi's `render`
// composes JS theme callbacks (`selectedText`, `description`, `scrollInfo`,
// `noMatch`, `selectedPrefix`) and an optional `truncatePrimary` override — JS
// closures that cannot cross the addon boundary. The hand-written
// `select-list.ts` shim therefore routes `render` through this core ONLY when
// the theme functions are all identity and no `truncatePrimary` override is
// supplied (the core bakes in an identity theme and no override); every other
// construction delegates to pi's original class. Item text and layout bounds
// cross as JSON / numbers; selection and filter state live in the core so the
// shim can keep it in sync for `render`.

#[derive(serde::Deserialize)]
struct SelectItemIn {
    value: String,
    label: String,
    description: Option<String>,
}

fn identity_select_theme() -> pidgin_tui::SelectListTheme {
    pidgin_tui::SelectListTheme {
        selected_prefix: Box::new(|s| s.to_string()),
        selected_text: Box::new(|s| s.to_string()),
        description: Box::new(|s| s.to_string()),
        scroll_info: Box::new(|s| s.to_string()),
        no_match: Box::new(|s| s.to_string()),
    }
}

/// The Rust-backed select-list core, exposed to JavaScript as `SelectListCore`.
/// Constructed with an identity theme and no `truncatePrimary` override; the
/// shim only builds one when pi's theme is identity and no override is set.
#[napi(js_name = "SelectListCore")]
pub struct SelectListCore {
    inner: pidgin_tui::SelectList,
}

#[napi]
impl SelectListCore {
    /// Build a core from pi's `items` (JSON array of `{ value, label,
    /// description? }`), `maxVisible`, and the optional
    /// `minPrimaryColumnWidth`/`maxPrimaryColumnWidth` layout bounds.
    #[napi(constructor)]
    pub fn new(
        items_json: String,
        max_visible: i64,
        min_primary_column_width: Option<i64>,
        max_primary_column_width: Option<i64>,
    ) -> napi::Result<Self> {
        let items_in: Vec<SelectItemIn> = serde_json::from_str(&items_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid items: {e}")))?;
        let items: Vec<pidgin_tui::SelectItem> = items_in
            .into_iter()
            .map(|i| pidgin_tui::SelectItem {
                value: i.value,
                label: i.label,
                description: i.description,
            })
            .collect();
        let layout = pidgin_tui::SelectListLayoutOptions {
            min_primary_column_width,
            max_primary_column_width,
            truncate_primary: None,
        };
        Ok(Self {
            inner: pidgin_tui::SelectList::new(items, max_visible, identity_select_theme(), layout),
        })
    }

    /// pi's `setFilter(filter)`: case-insensitive `value` prefix filter.
    #[napi(js_name = "setFilter")]
    pub fn set_filter(&mut self, filter: String) {
        self.inner.set_filter(&filter);
    }

    /// pi's `setSelectedIndex(index)`: clamp the selection into range.
    #[napi(js_name = "setSelectedIndex")]
    pub fn set_selected_index(&mut self, index: i64) {
        self.inner.set_selected_index(index);
    }

    /// pi's `handleInput(keyData)`: move/confirm/cancel. Callbacks are handled by
    /// the shim's original instance; the core only advances selection state.
    #[napi(js_name = "handleInput")]
    pub fn handle_input(&mut self, key_data: String) {
        self.inner.handle_input_str(&key_data);
    }

    /// pi's `getSelectedItem()` as JSON (`{ value, label, description? }`), or
    /// `null` when the filtered list is empty.
    #[napi(js_name = "getSelectedItemJson")]
    pub fn get_selected_item_json(&self) -> napi::Result<Option<String>> {
        match self.inner.get_selected_item() {
            Some(item) => serde_json::to_string(&serde_json::json!({
                "value": item.value,
                "label": item.label,
                "description": item.description,
            }))
            .map(Some)
            .map_err(|e| napi::Error::from_reason(e.to_string())),
            None => Ok(None),
        }
    }

    /// pi's `render(width)`: render the list to lines (identity theme baked in).
    #[napi(js_name = "render")]
    pub fn render(&self, width: u32) -> Vec<String> {
        self.inner.render_lines(width as usize)
    }
}
