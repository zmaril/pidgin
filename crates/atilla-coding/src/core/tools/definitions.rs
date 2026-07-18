//! Per-tool `create_<tool>_tool_definition` factories (pi's per-file
//! `create<Tool>ToolDefinition`, e.g.
//! `vendor/pi/packages/coding-agent/src/core/tools/read.ts:203`).
//!
//! # Layout divergence from pi
//!
//! pi declares each `create<Tool>ToolDefinition` in that tool's own `.ts` file.
//! atilla's tool files hold only the **pure** ported layers (`format_text_read`,
//! `run_grep`, `compute_edit_result`, `BashTool::execute`, …) and `bash.rs` is
//! already at the file-size straitjacket limit, so the seven factories are
//! centralized here instead. Each factory still carries pi's **byte-exact**
//! `name` / `label` / `description` / `parameters` (verified against
//! `typebox@1.1.38` schema serialization) and adapts the corresponding ported
//! tool in its `execute` closure. `index.rs` re-exports them.
//!
//! # Error contract
//!
//! pi tools `throw` on failure and the agent loop maps the throw to an
//! `is_error` tool result. atilla's [`atilla_agent::types::AgentToolExecute`]
//! returns [`AgentToolResult`] directly and cannot throw (the loop treats every
//! result as `is_error: false`), so a failing factory encodes the error text as
//! the result `content` with `details: {}`, matching the shape the loop's own
//! `create_error_tool_result` produces. The model-facing `content` is identical
//! to pi; only the internal `is_error` flag (a render/event concern) differs.
//!
//! # Async bridging
//!
//! `ls` / `write` / `edit` / `bash` have async ported runs, but
//! `execute` is synchronous. They are driven on a shared multi-thread
//! [`RUNTIME`] via [`block_on`]. The sync agent loop guarantees no ambient tokio
//! runtime on the calling thread; if this were ever called from within a runtime,
//! the `block_on` would need to move to `spawn_blocking`.
//!
//! # Deferred option surface
//!
//! pi's per-tool option bags carry custom `operations` backends (SSH/remote),
//! image auto-resize (`read`), and bash `spawnHook`. atilla's ported tools do not
//! expose those seams yet, so the option structs here are minimal placeholders;
//! `bash` threads `command_prefix` / `shell_path` through (both are `Send + Sync`)
//! but not `spawn_hook` (its `Box<dyn Fn>` is not `Send + Sync`).

// straitjacket-allow-file:duplication — the seven factories intentionally share
// the same parallel prepare/execute/map shape, mirroring pi's per-file factories.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::watch;

use atilla_agent::types::{AgentToolResult, AgentToolUpdateCallback};
use atilla_ai::seams::AbortSignal;
use atilla_ai::ContentBlock;

use crate::core::extensions::types::{RenderShell, ToolDefinition, ToolDefinitionExecute};

use super::bash::{create_bash_tool, BashToolOptions, BashUpdate, OnUpdate};
use super::edit::{compute_edit_result, prepare_edit_arguments, validate_edit_input};
use super::file_mutation_queue::with_file_mutation_queue;
use super::find::run_find;
use super::grep::{run_grep, GrepParams};
use super::ls::{create_local_ls_operations, run_ls, LsParams};
use super::path_utils::{resolve_read_path, resolve_to_cwd};
use super::read::format_text_read;
use super::write::{create_local_write_operations, run_write, WriteParams};

// ---------------------------------------------------------------------------
// Runtime + async bridges
// ---------------------------------------------------------------------------

/// Shared multi-thread runtime used to `block_on` async tool runs from the sync
/// `execute` contract. See the module docs for the ambient-runtime caveat.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the tools bridge runtime")
});

/// Drive `fut` to completion on the shared [`RUNTIME`].
pub(crate) fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    RUNTIME.block_on(fut)
}

/// Bridge an optional cooperative [`AbortSignal`] into the
/// `watch::Receiver<bool>` the async tool runs consume, run `make_fut`, and stop
/// the bridge poller when the run settles.
///
/// A background task polls [`AbortSignal::is_aborted`] and flips the watch
/// sender `true` on abort (or exits once the receiver is dropped). When no signal
/// is supplied the run gets `None` and no poller is spawned.
fn run_with_abort<F, T>(
    signal: Option<&AbortSignal>,
    make_fut: impl FnOnce(Option<watch::Receiver<bool>>) -> F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    match signal {
        None => block_on(make_fut(None)),
        Some(sig) => {
            let (tx, rx) = watch::channel(false);
            if sig.is_aborted() {
                let _ = tx.send(true);
            }
            let sig = sig.clone();
            let fut = make_fut(Some(rx));
            block_on(async move {
                let poller = tokio::spawn(async move {
                    loop {
                        if sig.is_aborted() {
                            let _ = tx.send(true);
                            break;
                        }
                        if tx.is_closed() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                });
                let out = fut.await;
                poller.abort();
                out
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Result helpers
// ---------------------------------------------------------------------------

fn text_block(text: String) -> ContentBlock {
    ContentBlock::Text {
        text,
        text_signature: None,
    }
}

/// A successful single-text-block result (pi's `details` is `undefined` for the
/// text tools; the port keeps `details: null` since the render layer that would
/// consume the truncation detail is deferred).
fn ok_result(text: String) -> AgentToolResult {
    AgentToolResult {
        content: vec![text_block(text)],
        details: Value::Null,
        added_tool_names: None,
        terminate: None,
    }
}

/// An error result mirroring the agent loop's `create_error_tool_result` shape.
fn error_result(message: String) -> AgentToolResult {
    AgentToolResult {
        content: vec![text_block(message)],
        details: json!({}),
        added_tool_names: None,
        terminate: None,
    }
}

fn arg_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn arg_bool(params: &Value, key: &str) -> Option<bool> {
    params.get(key).and_then(Value::as_bool)
}

fn arg_usize(params: &Value, key: &str) -> Option<usize> {
    params.get(key).and_then(|v| {
        v.as_u64()
            .map(|n| n as usize)
            .or_else(|| v.as_f64().map(|f| f as usize))
    })
}

fn is_aborted(signal: Option<&AbortSignal>) -> bool {
    signal.map(AbortSignal::is_aborted).unwrap_or(false)
}

/// Map a streamed [`BashUpdate`] into the [`AgentToolResult`] snapshot the agent
/// loop's update callback expects (pi's `onUpdate({ content, details })`).
fn bash_update_to_result(update: BashUpdate) -> AgentToolResult {
    let content = match update.content {
        // pi's initial update sends `content: []`.
        None => Vec::new(),
        Some(text) => vec![text_block(text)],
    };
    AgentToolResult {
        content,
        details: Value::Null,
        added_tool_names: None,
        terminate: None,
    }
}

// ---------------------------------------------------------------------------
// Option structs (minimal placeholders; see module docs)
// ---------------------------------------------------------------------------

/// Options for [`create_read_tool_definition`]. pi's `ReadToolOptions`
/// (`autoResizeImages`, custom `operations`) is deferred.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadToolOptions {}

/// Options for [`create_edit_tool_definition`]. pi's `EditToolOptions`
/// (custom `operations`) is deferred.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditToolOptions {}

/// Options for [`create_write_tool_definition`]. pi's `WriteToolOptions`
/// (custom `operations`) is deferred.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteToolOptions {}

/// Options for [`create_grep_tool_definition`]. pi's `GrepToolOptions`
/// (custom `operations`) is deferred.
#[derive(Debug, Clone, Copy, Default)]
pub struct GrepToolOptions {}

/// Options for [`create_find_tool_definition`]. pi's `FindToolOptions`
/// (custom `operations`) is deferred.
#[derive(Debug, Clone, Copy, Default)]
pub struct FindToolOptions {}

/// Options for [`create_ls_tool_definition`]. pi's `LsToolOptions`
/// (custom `operations`) is deferred.
#[derive(Debug, Clone, Copy, Default)]
pub struct LsToolOptions {}

// ---------------------------------------------------------------------------
// read (sync)
// ---------------------------------------------------------------------------

/// Build the `read` [`ToolDefinition`] (pi's `createReadToolDefinition`).
pub fn create_read_tool_definition(
    cwd: impl Into<String>,
    _options: Option<ReadToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    let execute: ToolDefinitionExecute = Arc::new(move |_id, params, signal, _on_update, _ctx| {
        if is_aborted(signal) {
            return error_result("Operation aborted".to_string());
        }
        let path = match arg_str(params, "path") {
            Some(p) => p,
            None => {
                return error_result("read tool input is invalid. path is required.".to_string())
            }
        };
        let offset = arg_usize(params, "offset");
        let limit = arg_usize(params, "limit");
        let resolved = match resolve_read_path(path, &cwd, &|p| Path::new(p).exists()) {
            Ok(p) => p,
            Err(e) => return error_result(e.to_string()),
        };
        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(e) => return error_result(e.to_string()),
        };
        match format_text_read(&content, path, offset, limit) {
            Ok(out) => ok_result(out.text),
            Err(e) => error_result(e),
        }
    });
    ToolDefinition {
        name: "read".to_string(),
        label: "read".to_string(),
        description: "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
                "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
                "limit": { "type": "number", "description": "Maximum number of lines to read" }
            }
        }),
        execution_mode: None,
        execute,
        prepare_arguments: None,
        prompt_snippet: Some("Read file contents".to_string()),
        prompt_guidelines: Some(vec!["Use read to examine files instead of cat or sed.".to_string()]),
        render_shell: None,
    }
}

// ---------------------------------------------------------------------------
// grep (sync)
// ---------------------------------------------------------------------------

/// Build the `grep` [`ToolDefinition`] (pi's `createGrepToolDefinition`).
pub fn create_grep_tool_definition(
    cwd: impl Into<String>,
    _options: Option<GrepToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    let execute: ToolDefinitionExecute = Arc::new(move |_id, params, signal, _on_update, _ctx| {
        if is_aborted(signal) {
            return error_result("Operation aborted".to_string());
        }
        let pattern = arg_str(params, "pattern").unwrap_or("");
        let grep_params = GrepParams {
            pattern,
            path: arg_str(params, "path"),
            glob: arg_str(params, "glob"),
            ignore_case: arg_bool(params, "ignoreCase").unwrap_or(false),
            literal: arg_bool(params, "literal").unwrap_or(false),
            context: arg_usize(params, "context").unwrap_or(0),
            limit: arg_usize(params, "limit"),
        };
        match run_grep(&cwd, &grep_params) {
            Ok(result) => ok_result(result.text),
            Err(e) => error_result(e),
        }
    });
    ToolDefinition {
        name: "grep".to_string(),
        label: "grep".to_string(),
        description: "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": { "type": "string", "description": "Search pattern (regex or literal string)" },
                "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
                "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'" },
                "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
                "literal": { "type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)" },
                "context": { "type": "number", "description": "Number of lines to show before and after each match (default: 0)" },
                "limit": { "type": "number", "description": "Maximum number of matches to return (default: 100)" }
            }
        }),
        execution_mode: None,
        execute,
        prepare_arguments: None,
        prompt_snippet: Some("Search file contents for patterns (respects .gitignore)".to_string()),
        prompt_guidelines: None,
        render_shell: None,
    }
}

// ---------------------------------------------------------------------------
// find (sync)
// ---------------------------------------------------------------------------

/// Build the `find` [`ToolDefinition`] (pi's `createFindToolDefinition`).
pub fn create_find_tool_definition(
    cwd: impl Into<String>,
    _options: Option<FindToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    let execute: ToolDefinitionExecute = Arc::new(move |_id, params, signal, _on_update, _ctx| {
        if is_aborted(signal) {
            return error_result("Operation aborted".to_string());
        }
        let pattern = arg_str(params, "pattern").unwrap_or("");
        let path = arg_str(params, "path");
        let limit = arg_usize(params, "limit");
        match run_find(&cwd, pattern, path, limit) {
            Ok(result) => ok_result(result.text),
            Err(e) => error_result(e),
        }
    });
    ToolDefinition {
        name: "find".to_string(),
        label: "find".to_string(),
        description: "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'" },
                "path": { "type": "string", "description": "Directory to search in (default: current directory)" },
                "limit": { "type": "number", "description": "Maximum number of results (default: 1000)" }
            }
        }),
        execution_mode: None,
        execute,
        prepare_arguments: None,
        prompt_snippet: Some("Find files by glob pattern (respects .gitignore)".to_string()),
        prompt_guidelines: None,
        render_shell: None,
    }
}

// ---------------------------------------------------------------------------
// ls (async run)
// ---------------------------------------------------------------------------

/// Build the `ls` [`ToolDefinition`] (pi's `createLsToolDefinition`).
pub fn create_ls_tool_definition(
    cwd: impl Into<String>,
    _options: Option<LsToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    let execute: ToolDefinitionExecute = Arc::new(move |_id, params, signal, _on_update, _ctx| {
        let ls_params = LsParams {
            path: arg_str(params, "path").map(str::to_string),
            limit: arg_usize(params, "limit"),
        };
        let cwd = cwd.clone();
        let ops = create_local_ls_operations();
        let result = run_with_abort(signal, move |rx| async move {
            run_ls(&cwd, &ls_params, &ops, rx.as_ref()).await
        });
        match result {
            Ok(result) => ok_result(result.text),
            Err(e) => error_result(e),
        }
    });
    ToolDefinition {
        name: "ls".to_string(),
        label: "ls".to_string(),
        description: "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list (default: current directory)" },
                "limit": { "type": "number", "description": "Maximum number of entries to return (default: 500)" }
            }
        }),
        execution_mode: None,
        execute,
        prepare_arguments: None,
        prompt_snippet: Some("List directory contents".to_string()),
        prompt_guidelines: None,
        render_shell: None,
    }
}

// ---------------------------------------------------------------------------
// write (async run)
// ---------------------------------------------------------------------------

/// Build the `write` [`ToolDefinition`] (pi's `createWriteToolDefinition`).
pub fn create_write_tool_definition(
    cwd: impl Into<String>,
    _options: Option<WriteToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    let execute: ToolDefinitionExecute = Arc::new(move |_id, params, signal, _on_update, _ctx| {
        let write_params = WriteParams {
            path: arg_str(params, "path").unwrap_or("").to_string(),
            content: arg_str(params, "content").unwrap_or("").to_string(),
        };
        let cwd = cwd.clone();
        let ops = create_local_write_operations();
        let result = run_with_abort(signal, move |rx| async move {
            run_write(&cwd, &write_params, &ops, rx.as_ref()).await
        });
        match result {
            Ok(result) => ok_result(result.text),
            Err(e) => error_result(e),
        }
    });
    ToolDefinition {
        name: "write".to_string(),
        label: "write".to_string(),
        description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            }
        }),
        execution_mode: None,
        execute,
        prepare_arguments: None,
        prompt_snippet: Some("Create or overwrite files".to_string()),
        prompt_guidelines: Some(vec!["Use write only for new files or complete rewrites.".to_string()]),
        render_shell: None,
    }
}

// ---------------------------------------------------------------------------
// edit (async, file-mutation-queue-backed)
// ---------------------------------------------------------------------------

/// Build the `edit` [`ToolDefinition`] (pi's `createEditToolDefinition`).
pub fn create_edit_tool_definition(
    cwd: impl Into<String>,
    _options: Option<EditToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    let execute: ToolDefinitionExecute = Arc::new(move |_id, params, signal, _on_update, _ctx| {
        let validated = match validate_edit_input(params) {
            Ok(v) => v,
            Err(e) => return error_result(e),
        };
        let abs = match resolve_to_cwd(&validated.path, &cwd) {
            Ok(p) => p,
            Err(e) => return error_result(e.to_string()),
        };
        let sig = signal.cloned();
        let path_display = validated.path.clone();
        let edits = validated.edits.clone();
        let queue_key = PathBuf::from(&abs);
        let result: Result<String, String> = block_on(async move {
            with_file_mutation_queue(&queue_key, async move {
                let aborted = || sig.as_ref().map(AbortSignal::is_aborted).unwrap_or(false);
                if aborted() {
                    return Err("Operation aborted".to_string());
                }
                let raw = match std::fs::read_to_string(&abs) {
                    Ok(c) => c,
                    Err(e) => {
                        return Err(format!("Could not edit file: {path_display}. {e}."));
                    }
                };
                if aborted() {
                    return Err("Operation aborted".to_string());
                }
                let computed = compute_edit_result(&raw, &edits, &path_display)?;
                if let Err(e) = std::fs::write(&abs, &computed.final_content) {
                    return Err(format!("Could not edit file: {path_display}. {e}."));
                }
                if aborted() {
                    return Err("Operation aborted".to_string());
                }
                Ok(computed.message)
            })
            .await
        });
        match result {
            Ok(message) => ok_result(message),
            Err(e) => error_result(e),
        }
    });
    ToolDefinition {
        name: "edit".to_string(),
        label: "edit".to_string(),
        description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["path", "edits"],
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["oldText", "newText"],
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                        }
                    },
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead."
                }
            }
        }),
        execution_mode: None,
        execute,
        prepare_arguments: Some(Arc::new(prepare_edit_arguments)),
        prompt_snippet: Some("Make precise file edits with exact text replacement, including multiple disjoint edits in one call".to_string()),
        prompt_guidelines: Some(vec![
            "Use edit for precise changes (edits[].oldText must match exactly)".to_string(),
            "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls".to_string(),
            "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.".to_string(),
            "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.".to_string(),
        ]),
        render_shell: Some(RenderShell::SelfRender),
    }
}

// ---------------------------------------------------------------------------
// bash (async, streaming + abort)
// ---------------------------------------------------------------------------

/// Build the `bash` [`ToolDefinition`] (pi's `createBashToolDefinition`).
pub fn create_bash_tool_definition(
    cwd: impl Into<String>,
    options: Option<BashToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    // `spawn_hook` is intentionally dropped: its `Box<dyn Fn>` is not
    // `Send + Sync`, which the `AgentToolExecute` closure requires. The
    // `Send + Sync` `command_prefix` / `shell_path` are threaded through.
    let (command_prefix, shell_path) = match options {
        Some(o) => (o.command_prefix, o.shell_path),
        None => (None, None),
    };
    let execute: ToolDefinitionExecute = Arc::new(move |_id, params, signal, on_update, _ctx| {
        let command = arg_str(params, "command").unwrap_or("").to_string();
        let timeout = params.get("timeout").and_then(Value::as_f64);
        let tool = create_bash_tool(
            cwd.clone(),
            Some(BashToolOptions {
                command_prefix: command_prefix.clone(),
                shell_path: shell_path.clone(),
                spawn_hook: None,
            }),
        );
        let on_update_cb: Option<OnUpdate> = on_update.map(|cb| {
            let cb: AgentToolUpdateCallback = cb.clone();
            Box::new(move |update: BashUpdate| cb(&bash_update_to_result(update))) as OnUpdate
        });
        let result = run_with_abort(signal, move |rx| async move {
            tool.execute(&command, timeout, rx, on_update_cb).await
        });
        match result {
            Ok(result) => ok_result(result.content),
            Err(e) => error_result(e),
        }
    });
    ToolDefinition {
        name: "bash".to_string(),
        label: "bash".to_string(),
        description: "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.".to_string(),
        parameters: json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": { "type": "string", "description": "Bash command to execute" },
                "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" }
            }
        }),
        execution_mode: None,
        execute,
        prepare_arguments: None,
        prompt_snippet: Some("Execute bash commands (ls, grep, find, etc.)".to_string()),
        prompt_guidelines: None,
        render_shell: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::test_support::TempDir;
    use crate::core::tools::tool_definition_wrapper::wrap_tool_definition;

    fn result_text(result: &AgentToolResult) -> String {
        result
            .content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text, .. } => text.clone(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn read_metadata_matches_pi() {
        let def = create_read_tool_definition(".", None);
        assert_eq!(def.name, "read");
        assert_eq!(def.label, "read");
        assert!(def.description.starts_with("Read the contents of a file."));
        assert_eq!(def.parameters["required"], json!(["path"]));
        assert_eq!(
            def.parameters["properties"]["path"]["description"],
            json!("Path to the file to read (relative or absolute)")
        );
    }

    #[test]
    fn read_executes_via_wrapped_agent_tool() {
        let dir = TempDir::new("def-read");
        dir.write("hello.txt", "line1\nline2\n");
        let tool = wrap_tool_definition(create_read_tool_definition(dir.cwd(), None), None);
        let result = (tool.execute)("c", &json!({ "path": "hello.txt" }), None, None);
        let text = result_text(&result);
        assert!(text.contains("line1"), "got: {text}");
        assert!(text.contains("line2"), "got: {text}");
    }

    #[test]
    fn grep_metadata_and_execution() {
        let dir = TempDir::new("def-grep");
        dir.write("a.txt", "needle here\nother\n");
        let def = create_grep_tool_definition(dir.cwd(), None);
        assert_eq!(def.name, "grep");
        assert_eq!(def.parameters["required"], json!(["pattern"]));
        let tool = wrap_tool_definition(def, None);
        let result = (tool.execute)("c", &json!({ "pattern": "needle" }), None, None);
        assert!(result_text(&result).contains("needle here"));
    }

    #[test]
    fn find_metadata_and_execution() {
        let dir = TempDir::new("def-find");
        dir.write("src/main.rs", "fn main() {}");
        let def = create_find_tool_definition(dir.cwd(), None);
        assert_eq!(def.name, "find");
        let tool = wrap_tool_definition(def, None);
        let result = (tool.execute)("c", &json!({ "pattern": "*.rs" }), None, None);
        assert!(result_text(&result).contains("main.rs"));
    }

    #[test]
    fn ls_metadata_and_execution() {
        let dir = TempDir::new("def-ls");
        dir.write("one.txt", "x");
        dir.mkdir("sub");
        let def = create_ls_tool_definition(dir.cwd(), None);
        assert_eq!(def.name, "ls");
        assert!(def.parameters.get("required").is_none());
        let tool = wrap_tool_definition(def, None);
        let result = (tool.execute)("c", &json!({}), None, None);
        let text = result_text(&result);
        assert!(text.contains("one.txt"), "got: {text}");
        assert!(text.contains("sub/"), "got: {text}");
    }

    #[test]
    fn write_metadata_and_execution_block_on() {
        let dir = TempDir::new("def-write");
        let def = create_write_tool_definition(dir.cwd(), None);
        assert_eq!(def.name, "write");
        assert_eq!(def.parameters["required"], json!(["path", "content"]));
        let tool = wrap_tool_definition(def, None);
        let result = (tool.execute)(
            "c",
            &json!({ "path": "out.txt", "content": "hello" }),
            None,
            None,
        );
        assert_eq!(
            result_text(&result),
            "Successfully wrote 5 bytes to out.txt"
        );
        let written = std::fs::read_to_string(dir.path.join("out.txt")).unwrap();
        assert_eq!(written, "hello");
    }

    #[test]
    fn edit_metadata_and_execution() {
        let dir = TempDir::new("def-edit");
        dir.write("f.txt", "alpha beta gamma");
        let def = create_edit_tool_definition(dir.cwd(), None);
        assert_eq!(def.name, "edit");
        assert_eq!(def.render_shell, Some(RenderShell::SelfRender));
        assert!(def.prepare_arguments.is_some());
        assert_eq!(def.prompt_guidelines.as_ref().unwrap().len(), 4);
        let tool = wrap_tool_definition(def, None);
        let result = (tool.execute)(
            "c",
            &json!({ "path": "f.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
            None,
            None,
        );
        assert_eq!(
            result_text(&result),
            "Successfully replaced 1 block(s) in f.txt."
        );
        assert_eq!(
            std::fs::read_to_string(dir.path.join("f.txt")).unwrap(),
            "alpha BETA gamma"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bash_metadata_and_execution_block_on() {
        let dir = TempDir::new("def-bash");
        let def = create_bash_tool_definition(dir.cwd(), None);
        assert_eq!(def.name, "bash");
        assert_eq!(def.parameters["required"], json!(["command"]));
        let tool = wrap_tool_definition(def, None);
        let result = (tool.execute)("c", &json!({ "command": "echo hi" }), None, None);
        assert_eq!(result_text(&result).trim(), "hi");
    }

    #[cfg(unix)]
    #[test]
    fn bash_abort_signal_aborts_run() {
        let dir = TempDir::new("def-bash-abort");
        let tool = wrap_tool_definition(create_bash_tool_definition(dir.cwd(), None), None);
        let signal = AbortSignal::aborted();
        let result = (tool.execute)(
            "c",
            &json!({ "command": "sleep 5; echo done" }),
            Some(&signal),
            None,
        );
        let text = result_text(&result);
        assert!(text.contains("aborted"), "expected abort, got: {text}");
    }
}
