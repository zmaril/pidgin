//! Node-API surface for the exec tools (`ls`, `write`, `bash`).
//!
//! These back the native `ls.ts` / `write.ts` / `bash.ts` conformance shims.
//! Each binding drives the async Rust run layer
//! ([`pidgin_coding::core::tools`]) to completion and returns the pi-shaped
//! `AgentToolResult` as JSON:
//!
//! ```json
//! { "content": [{ "type": "text", "text": "..." }], "details": <value|absent> }
//! ```
//!
//! The `details` key is **omitted** (not `null`) when there are no details so
//! the JS side sees `result.details === undefined`, matching pi's tools that
//! return `details: undefined`.
//!
//! # Why no threadsafe callback is needed here
//!
//! Like `faux.rs`/`command_core.rs`, this boundary is plain synchronous JSON.
//! The shims route only the **default** (local-filesystem / local-shell) tool
//! path through these bindings; when a pi test injects a custom `operations`
//! backend (bash streaming-chunk cases, the write mutation-queue barrier/abort
//! cases) the shim delegates to pi's original TypeScript instead, so no
//! JS→Rust callback ever has to cross this seam. The injected-ops cases stay
//! TS-backed (a hybrid flip); driving them natively would require the codebase's
//! first `ThreadsafeFunction`, which the house rule (faux.rs) deliberately avoids.
//!
//! # The async→sync bridge
//!
//! The run layer is async (tokio). napi calls arrive on node's thread with no
//! ambient tokio runtime, so a direct `block_on` on the dedicated [`RUNTIME`]
//! does not hit the "Cannot start a runtime from within a runtime" panic. The
//! ops futures are only `block_on`'d, never spawned.

// straitjacket-allow-file:duplication — the three tool bindings share one
// faithful parse-input / run / serialize-AgentToolResult shape at the Node
// boundary; the near-identical bodies mirror pi's per-tool `execute` surface
// and are kept distinct so each tool's JSON contract stays explicit.

use std::sync::LazyLock;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use pidgin_coding::core::tools::bash::{create_local_bash_operations, BashTool, BashToolDetails};
use pidgin_coding::core::tools::ls::{create_local_ls_operations, run_ls, LsParams};
use pidgin_coding::core::tools::truncate::{TruncatedBy, TruncationResult};
use pidgin_coding::core::tools::write::{create_local_write_operations, run_write, WriteParams};

/// Shared multi-thread runtime used to `block_on` the async tool runs from the
/// sync napi contract. See the module docs for the ambient-runtime caveat.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the pidgin-napi tools runtime")
});

/// Drive `fut` to completion on the shared [`RUNTIME`].
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    RUNTIME.block_on(fut)
}

/// A single `{ type: "text", text }` content block.
fn text_content(text: String) -> Value {
    json!([{ "type": "text", "text": text }])
}

/// Serialize a Rust [`TruncationResult`] to pi's camelCase `TruncationResult`.
fn truncation_json(t: &TruncationResult) -> Value {
    let truncated_by = match t.truncated_by {
        Some(TruncatedBy::Lines) => Value::from("lines"),
        Some(TruncatedBy::Bytes) => Value::from("bytes"),
        None => Value::Null,
    };
    json!({
        "content": t.content,
        "truncated": t.truncated,
        "truncatedBy": truncated_by,
        "totalLines": t.total_lines,
        "totalBytes": t.total_bytes,
        "outputLines": t.output_lines,
        "outputBytes": t.output_bytes,
        "lastLinePartial": t.last_line_partial,
        "firstLineExceedsLimit": t.first_line_exceeds_limit,
        "maxLines": t.max_lines,
        "maxBytes": t.max_bytes,
    })
}

/// Build the final `AgentToolResult` JSON, omitting `details` when `None` so the
/// JS side observes `result.details === undefined`.
fn result_json(content: Value, details: Option<Value>) -> napi::Result<String> {
    let mut obj = Map::new();
    obj.insert("content".to_string(), content);
    if let Some(d) = details {
        obj.insert("details".to_string(), d);
    }
    serde_json::to_string(&Value::Object(obj)).map_err(|e| napi::Error::from_reason(e.to_string()))
}

// ---------------------------------------------------------------------------
// ls
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LsInput {
    path: Option<String>,
    limit: Option<f64>,
}

/// `createLsTool(...).execute` default path (`ls.ts`): list `cwd`-relative
/// `input.path` with the local filesystem operations, returning the pi-shaped
/// `AgentToolResult` JSON. Errors (`Path not found`, `Not a directory`, …) cross
/// as a thrown JS `Error` with pi's exact message, matching pi's `reject(...)`.
///
/// Wrapped by the `#[napi]` `lsToolExecute` export in `lib.rs`.
pub(crate) fn ls_execute(cwd: String, input_json: String) -> napi::Result<String> {
    let input: LsInput = serde_json::from_str(&input_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid ls input: {e}")))?;
    let params = LsParams {
        path: input.path,
        limit: input.limit.map(|l| l as usize),
    };
    let ops = create_local_ls_operations();
    let result = block_on(run_ls(&cwd, &params, &ops, None)).map_err(napi::Error::from_reason)?;

    // Assemble pi's `LsToolDetails` (`{ entryLimitReached?, truncation? }`),
    // omitting it entirely when empty (pi returns `details: undefined`).
    let mut details = Map::new();
    if let Some(n) = result.entry_limit_reached {
        details.insert("entryLimitReached".to_string(), json!(n));
    }
    if let Some(t) = &result.truncation {
        details.insert("truncation".to_string(), truncation_json(t));
    }
    let details = if details.is_empty() {
        None
    } else {
        Some(Value::Object(details))
    };

    result_json(text_content(result.text), details)
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteInput {
    path: String,
    content: String,
}

/// `createWriteTool(...).execute` default path (`write.ts`): create parent dirs
/// and write `content` to `path` through the local filesystem operations and the
/// native file-mutation queue, returning pi's `Successfully wrote N bytes to
/// <path>` result with `details: undefined`. Errors cross as a thrown JS `Error`.
///
/// Wrapped by the `#[napi]` `writeToolExecute` export in `lib.rs`.
pub(crate) fn write_execute(cwd: String, input_json: String) -> napi::Result<String> {
    let input: WriteInput = serde_json::from_str(&input_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid write input: {e}")))?;
    let params = WriteParams {
        path: input.path,
        content: input.content,
    };
    let ops = create_local_write_operations();
    let result =
        block_on(run_write(&cwd, &params, &ops, None)).map_err(napi::Error::from_reason)?;
    // pi's write returns `details: undefined`.
    result_json(text_content(result.text), None)
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct BashInput {
    command: String,
    timeout: Option<f64>,
}

/// `createBashTool(...).execute` default path (`bash.ts`): run `command` through
/// the local-shell operations, streaming into the truncation/temp-file layer, and
/// return pi's `{ content, details: { truncation?, fullOutputPath? } }`. Non-zero
/// exit / timeout / abort cross as a thrown JS `Error` with pi's exact tail
/// message.
///
/// Wrapped by the `#[napi]` `bashToolExecute` export in `lib.rs`. This binding is
/// prepared for the default-path bash flip; the streaming-chunk and
/// injected-`operations` bash tests stay TS-backed (the shim delegates), so no
/// `onData`/`onUpdate` callback crosses this seam.
pub(crate) fn bash_execute(cwd: String, input_json: String) -> napi::Result<String> {
    let input: BashInput = serde_json::from_str(&input_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid bash input: {e}")))?;
    let tool = BashTool::new(cwd, create_local_bash_operations(None));
    let result = block_on(tool.execute(&input.command, input.timeout, None, None))
        .map_err(napi::Error::from_reason)?;
    let details = result.details.as_ref().map(bash_details_json);
    result_json(text_content(result.content), details)
}

/// Serialize pi's `BashToolDetails` (`{ truncation?, fullOutputPath? }`),
/// omitting each key when absent.
fn bash_details_json(d: &BashToolDetails) -> Value {
    let mut obj = Map::new();
    if let Some(t) = &d.truncation {
        obj.insert("truncation".to_string(), truncation_json(t));
    }
    if let Some(p) = &d.full_output_path {
        obj.insert("fullOutputPath".to_string(), json!(p));
    }
    Value::Object(obj)
}
