//! Port of pi's `core/tools/write.ts`.
//!
//! The write tool creates parent directories and writes file contents to disk,
//! serializing concurrent writes to the same file through the file-mutation
//! queue. The filesystem side effects go through a pluggable
//! [`WriteOperations`] trait (pi's `WriteOperations` interface) so extensions
//! can swap the backend (for example an SSH-backed remote filesystem);
//! [`create_local_write_operations`] returns the default `tokio::fs`-backed
//! implementation.
//!
//! The TUI render hooks ([`write_render_call`]/[`write_render_result`]) are
//! ported here as **stateless** functions. pi threads a mutable
//! `WriteCallRenderComponent` with an incremental syntax-highlight cache through
//! `context.lastComponent`, but that cache converges to a full rebuild on the
//! `argsComplete` frame (`rebuildWriteHighlightCacheFull` == the non-cache
//! `highlightCode(replaceTabs(normalizeDisplayText(content)), lang)` path), so
//! the settled output is a pure function of `{args, options, context}`. write
//! uses the DEFAULT render shell, so the returned components are composed into
//! the shell's call/result box. Valid-language highlighting is the deno-plane
//! seam documented on [`highlight_code`](super::render_utils::highlight_code).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::watch;

use pidgin_agent::types::AgentToolResult;
use pidgin_ai::ContentBlock;
use pidgin_tui::renderer::{Component, Container};
use pidgin_tui::Text;

use crate::core::extensions::types::{ToolRenderContext, ToolRenderResultOptions};
use crate::modes::interactive::theme::runtime::Theme;

use super::file_mutation_queue::with_file_mutation_queue;
use super::path_utils::resolve_to_cwd;
use super::render_utils::{
    get_language_from_path, highlight_code, normalize_display_text, render_tool_path, replace_tabs,
    str_json, tools_expand_hint, trim_trailing_empty_lines,
};

/// Input parameters for the write tool (pi's `{ path, content }` schema).
#[derive(Debug, Clone)]
pub struct WriteParams {
    /// Path to the file to write (relative or absolute).
    pub path: String,
    /// Content to write to the file.
    pub content: String,
}

/// The result of a write: the success message returned to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    /// The `Successfully wrote N bytes to <path>` message.
    pub text: String,
}

/// Pluggable operations for the write tool, mirroring pi's `WriteOperations`
/// interface. Override these to delegate file writing to remote systems (for
/// example SSH). Kept a `dyn`-object-safe trait (methods return boxed futures)
/// so a custom backend can be injected as `Arc<dyn WriteOperations>` — for
/// example across the napi boundary — the same way the package-manager flip
/// injects `Box<dyn CommandFlowMachine>`.
///
/// The `Send + Sync` supertrait makes the *backend value* shareable across
/// threads (`Arc<dyn WriteOperations>: Send + Sync`). The returned futures are
/// deliberately **not** `+ Send`, matching the other tool seams; they are driven
/// on the tools bridge runtime via `block_on`, which accepts `!Send` futures.
pub trait WriteOperations: Send + Sync {
    /// Write `content` to the file at `absolute_path`.
    fn write_file<'a>(
        &'a self,
        absolute_path: &'a str,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;
    /// Create `dir` (and any missing parents) recursively.
    fn mkdir<'a>(&'a self, dir: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;
}

/// The default local-filesystem [`WriteOperations`], backed by `tokio::fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalWriteOperations;

impl WriteOperations for LocalWriteOperations {
    fn write_file<'a>(
        &'a self,
        absolute_path: &'a str,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
        Box::pin(async move {
            tokio::fs::write(absolute_path, content)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn mkdir<'a>(&'a self, dir: &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

/// Construct the default local-filesystem write operations (pi's
/// `defaultWriteOperations` / `createLocalWriteOperations` analog).
pub fn create_local_write_operations() -> LocalWriteOperations {
    LocalWriteOperations
}

/// Return `true` when `signal` is present and currently aborted, mirroring pi's
/// `signal?.aborted` check.
fn is_aborted(signal: Option<&watch::Receiver<bool>>) -> bool {
    signal.map(|s| *s.borrow()).unwrap_or(false)
}

/// Execute the write tool: resolve `params.path` against `cwd`, create the
/// parent directory, and write the content, all under the per-file mutation
/// queue so concurrent writes to the same file serialize.
///
/// `signal` is an optional cancellation channel modeled after the foundation
/// modules (a `watch::Receiver<bool>` that flips to `true` on abort). It is
/// checked after each await point — exactly like pi's `throwIfAborted` — so an
/// abort is observed without releasing the mutation queue mid-operation; on
/// abort this returns `Err("Operation aborted")` to match pi's thrown error.
pub async fn run_write(
    cwd: &str,
    params: &WriteParams,
    ops: &dyn WriteOperations,
    signal: Option<&watch::Receiver<bool>>,
) -> Result<WriteResult, String> {
    let absolute_path = resolve_to_cwd(&params.path, cwd).map_err(|e| e.to_string())?;
    let dir = Path::new(&absolute_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let abs_pathbuf = PathBuf::from(&absolute_path);

    with_file_mutation_queue(&abs_pathbuf, async {
        // Check `signal.aborted` after each await rather than rejecting from an
        // abort listener: this observes the same aborts while keeping the queue
        // locked until the in-flight filesystem operation has settled.
        if is_aborted(signal) {
            return Err("Operation aborted".to_string());
        }

        // Create parent directories if needed.
        ops.mkdir(&dir).await?;
        if is_aborted(signal) {
            return Err("Operation aborted".to_string());
        }

        // Write the file contents.
        ops.write_file(&absolute_path, &params.content).await?;
        if is_aborted(signal) {
            return Err("Operation aborted".to_string());
        }

        // pi reports `content.length`, which in JS is the number of UTF-16 code
        // units (NOT UTF-8 bytes and NOT Unicode scalar count). Reproduce that
        // exactly with `encode_utf16().count()` so the byte figure and the
        // echoed original `path` match pi byte-for-byte.
        let byte_count = params.content.encode_utf16().count();
        Ok(WriteResult {
            text: format!("Successfully wrote {byte_count} bytes to {}", params.path),
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// TUI render hooks (pi's `renderCall` / `renderResult`, `write.ts:227` / `:251`)
// ---------------------------------------------------------------------------

/// Local `theme.fg` wrapper falling back to unstyled text on an unknown color
/// key (pi's `theme.fg` is infallible; the ported [`Theme::fg`] returns a
/// `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// The path argument for display: `file_path` unless nullish, else `path`,
/// coerced through pi's `str` (mirrors `str(args?.file_path ?? args?.path)`).
fn write_path_arg(args: &Value) -> Option<String> {
    let raw = match args.get("file_path") {
        Some(v) if !v.is_null() => Some(v),
        _ => args.get("path"),
    };
    str_json(raw)
}

/// Format the write call header + content preview (pi's `formatWriteCall`).
///
/// The syntax-highlight cache is stateless here: on the settled frame pi's cache
/// equals `highlightCode(replaceTabs(normalizeDisplayText(content)), lang)`, so
/// this recomputes it directly (see the module doc).
fn format_write_call(
    args: &Value,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    cwd: &str,
) -> String {
    let raw_path = write_path_arg(args);
    let file_content = str_json(args.get("content"));
    let path_display = render_tool_path(raw_path.as_deref(), theme, cwd, None);
    let mut text = format!(
        "{} {}",
        fg(theme, "toolTitle", &theme.bold("write")),
        path_display
    );

    match file_content {
        None => {
            text += &format!(
                "\n\n{}",
                fg(theme, "error", "[invalid content arg - expected string]")
            );
        }
        Some(content) if !content.is_empty() => {
            let lang = raw_path
                .as_deref()
                .filter(|p| !p.is_empty())
                .and_then(get_language_from_path);
            let rendered_lines: Vec<String> = match lang {
                Some(l) => highlight_code(
                    &replace_tabs(&normalize_display_text(&content)),
                    Some(l),
                    theme,
                ),
                None => normalize_display_text(&content)
                    .split('\n')
                    .map(str::to_string)
                    .collect(),
            };
            let lines = trim_trailing_empty_lines(&rendered_lines);
            let total_lines = lines.len();
            let max_lines = if options.expanded { lines.len() } else { 10 };
            let display_lines = &lines[..max_lines.min(lines.len())];
            let remaining = lines.len() as isize - max_lines as isize;

            let body = display_lines
                .iter()
                .map(|line| {
                    if lang.is_some() {
                        line.clone()
                    } else {
                        fg(theme, "toolOutput", &replace_tabs(line))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            text += &format!("\n\n{body}");

            if remaining > 0 {
                text += &fg(
                    theme,
                    "muted",
                    &format!("\n... ({remaining} more lines, {total_lines} total,"),
                );
                text += " ";
                text += &tools_expand_hint(theme, "to expand");
                text += &fg(theme, "muted", ")");
            }
        }
        Some(_) => {}
    }

    text
}

/// Format the write result body (pi's `formatWriteResult`): only rendered on
/// error, as the raw error text (no ANSI stripping/sanitizing, matching pi).
fn format_write_result(result: &AgentToolResult, theme: &Theme, is_error: bool) -> Option<String> {
    if !is_error {
        return None;
    }
    let output = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if output.is_empty() {
        return None;
    }
    Some(format!("\n{}", fg(theme, "error", &output)))
}

/// Custom rendering for the write tool call (pi's `renderCall`, `write.ts:227`).
pub fn write_render_call(
    args: &Value,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    let options = ToolRenderResultOptions {
        expanded: context.expanded,
        is_partial: context.is_partial,
    };
    Box::new(Text::new(
        &format_write_call(args, &options, theme, context.cwd),
        0,
        0,
        None,
    ))
}

/// Custom rendering for the write tool result (pi's `renderResult`,
/// `write.ts:251`). pi returns a cleared `Container` (renders nothing) unless
/// the result is an error, in which case it returns the error text `Text`.
pub fn write_render_result(
    result: &AgentToolResult,
    _options: &ToolRenderResultOptions,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    match format_write_result(result, theme, context.is_error) {
        None => Box::new(Container::new()),
        Some(output) => Box::new(Text::new(&output, 0, 0, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::test_support::TempDir;
    use std::sync::Mutex;

    fn params(path: &str, content: &str) -> WriteParams {
        WriteParams {
            path: path.to_string(),
            content: content.to_string(),
        }
    }

    #[tokio::test]
    async fn writes_content_and_reports_byte_count() {
        let dir = TempDir::new("write-basic");
        let ops = create_local_write_operations();
        let out = run_write(dir.cwd(), &params("out.txt", "hello"), &ops, None)
            .await
            .unwrap();
        // ASCII: 5 UTF-16 code units == 5 bytes; echoed path is the input path.
        assert_eq!(out.text, "Successfully wrote 5 bytes to out.txt");
        let written = std::fs::read_to_string(dir.path.join("out.txt")).unwrap();
        assert_eq!(written, "hello");
    }

    #[tokio::test]
    async fn counts_utf16_code_units_for_astral_characters() {
        let dir = TempDir::new("write-utf16");
        let ops = create_local_write_operations();
        // "a" (1 UTF-16 unit) + U+1F600 (astral -> 2 UTF-16 surrogate units).
        // pi's `content.length` == 3 here, whereas UTF-8 bytes == 5 and Unicode
        // scalar count == 2. We must reproduce pi's 3.
        let content = "a\u{1F600}";
        assert_eq!(content.len(), 5); // UTF-8 bytes
        assert_eq!(content.chars().count(), 2); // scalar count
        let out = run_write(dir.cwd(), &params("emoji.txt", content), &ops, None)
            .await
            .unwrap();
        assert_eq!(out.text, "Successfully wrote 3 bytes to emoji.txt");
    }

    #[tokio::test]
    async fn creates_parent_directories() {
        let dir = TempDir::new("write-mkdir");
        let ops = create_local_write_operations();
        let out = run_write(
            dir.cwd(),
            &params("nested/deep/file.txt", "data"),
            &ops,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            out.text,
            "Successfully wrote 4 bytes to nested/deep/file.txt"
        );
        let written = std::fs::read_to_string(dir.path.join("nested/deep/file.txt")).unwrap();
        assert_eq!(written, "data");
    }

    #[tokio::test]
    async fn returns_operation_aborted_when_signal_set() {
        let dir = TempDir::new("write-abort");
        let ops = create_local_write_operations();
        let (tx, rx) = watch::channel(true); // already aborted
        let err = run_write(dir.cwd(), &params("x.txt", "y"), &ops, Some(&rx))
            .await
            .unwrap_err();
        assert_eq!(err, "Operation aborted");
        drop(tx);
        // Nothing should have been written.
        assert!(!dir.path.join("x.txt").exists());
    }

    /// A fake [`WriteOperations`] that records its calls instead of touching the
    /// filesystem — proving the operations seam is pluggable.
    #[derive(Default)]
    struct RecordingWriteOperations {
        mkdirs: Mutex<Vec<String>>,
        writes: Mutex<Vec<(String, String)>>,
    }

    impl WriteOperations for RecordingWriteOperations {
        fn write_file<'a>(
            &'a self,
            absolute_path: &'a str,
            content: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
            Box::pin(async move {
                self.writes
                    .lock()
                    .unwrap()
                    .push((absolute_path.to_string(), content.to_string()));
                Ok(())
            })
        }

        fn mkdir<'a>(
            &'a self,
            dir: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
            Box::pin(async move {
                self.mkdirs.lock().unwrap().push(dir.to_string());
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn uses_injected_operations_backend() {
        let ops = RecordingWriteOperations::default();
        let out = run_write("/base/cwd", &params("sub/file.txt", "abc"), &ops, None)
            .await
            .unwrap();
        assert_eq!(out.text, "Successfully wrote 3 bytes to sub/file.txt");
        assert_eq!(
            *ops.mkdirs.lock().unwrap(),
            vec!["/base/cwd/sub".to_string()]
        );
        assert_eq!(
            *ops.writes.lock().unwrap(),
            vec![("/base/cwd/sub/file.txt".to_string(), "abc".to_string())]
        );
    }

    // ---- File-mutation-queue serialization / abort-in-flight ----
    //
    // These port pi's `test/file-mutation-queue.test.ts` cases that inject a
    // delayed `writeFile` backend through the write tool's operations seam to
    // prove same-file serialization and, crucially, that the queue stays locked
    // while an aborted write is still in flight (pi L176).

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    /// A one-shot gate — the Rust analogue of pi's `createDeferred()` promise.
    ///
    /// `resolve()` opens the gate (idempotent); `wait()` returns immediately once
    /// resolved, otherwise parks until it is. Built on [`Notify`] plus a flag so
    /// it is race-free regardless of whether `resolve` or `wait` happens first,
    /// and reusable across the queue tests.
    #[derive(Default)]
    struct Deferred {
        resolved: AtomicBool,
        notify: Notify,
    }

    impl Deferred {
        fn new() -> Self {
            Self::default()
        }

        fn resolve(&self) {
            self.resolved.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }

        async fn wait(&self) {
            loop {
                if self.resolved.load(Ordering::SeqCst) {
                    return;
                }
                // Register interest before re-checking so a `resolve()` racing in
                // between cannot be missed (no lost wakeup).
                let notified = self.notify.notified();
                if self.resolved.load(Ordering::SeqCst) {
                    return;
                }
                notified.await;
            }
        }
    }

    /// The boxed-future return type the fake [`WriteOperations`] impls share.
    type BoxWriteFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;

    /// The terminal filesystem write both queue backends perform once their
    /// scripted preamble completes — pi's real `writeFile`, boxed. Sharing this
    /// keeps the `write_file` impls free of duplicated write-and-map-err tails.
    fn write_through<'a>(absolute_path: &'a str, content: &'a str) -> BoxWriteFuture<'a> {
        Box::pin(async move {
            tokio::fs::write(absolute_path, content)
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// The no-op `mkdir` shared by fake backends that never create directories.
    fn noop_mkdir<'a>() -> BoxWriteFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    /// A [`WriteOperations`] whose `write_file` sleeps and records occupancy, so
    /// two writes to the same path can be proven to serialize (pi's "serializes
    /// operations for the same file" via an injected delayed backend).
    struct DelayedWriteOperations {
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl WriteOperations for DelayedWriteOperations {
        fn write_file<'a>(
            &'a self,
            absolute_path: &'a str,
            content: &'a str,
        ) -> BoxWriteFuture<'a> {
            Box::pin(async move {
                let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                write_through(absolute_path, content).await
            })
        }

        fn mkdir<'a>(&'a self, _dir: &'a str) -> BoxWriteFuture<'a> {
            noop_mkdir()
        }
    }

    #[tokio::test]
    async fn serializes_same_path_writes_via_delayed_backend() {
        let dir = TempDir::new("write-serialize");
        let cwd = dir.cwd().to_string();
        let ops: Arc<DelayedWriteOperations> = Arc::new(DelayedWriteOperations {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });

        // `run_write`'s futures are `!Send` (the seam's boxed futures carry no
        // `+ Send` bound), so drive the two concurrent writes on a `LocalSet`.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let a_ops = ops.clone();
                let a_cwd = cwd.clone();
                let a = tokio::task::spawn_local(async move {
                    run_write(&a_cwd, &params("same.txt", "a\n"), &*a_ops, None)
                        .await
                        .unwrap();
                });
                let b_ops = ops.clone();
                let b_cwd = cwd.clone();
                let b = tokio::task::spawn_local(async move {
                    run_write(&b_cwd, &params("same.txt", "b\n"), &*b_ops, None)
                        .await
                        .unwrap();
                });
                a.await.unwrap();
                b.await.unwrap();
            })
            .await;

        assert_eq!(
            ops.max_active.load(Ordering::SeqCst),
            1,
            "same-path writes must not overlap"
        );
    }

    /// The L176 backend: the first write (content `"first\n"`) resolves
    /// `first_started`, blocks on `finish_first`, writes, then flags `settled`.
    /// The second write (content `"second\n"`) asserts `settled` before it may
    /// begin and resolves `second_started`.
    struct AbortInFlightWriteOperations {
        first_started: Arc<Deferred>,
        finish_first: Arc<Deferred>,
        second_started: Arc<Deferred>,
        settled: Arc<AtomicBool>,
    }

    impl WriteOperations for AbortInFlightWriteOperations {
        fn write_file<'a>(
            &'a self,
            absolute_path: &'a str,
            content: &'a str,
        ) -> BoxWriteFuture<'a> {
            Box::pin(async move {
                if content == "first\n" {
                    self.first_started.resolve();
                    self.finish_first.wait().await;
                    write_through(absolute_path, content).await?;
                    self.settled.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                if content == "second\n" {
                    assert!(
                        self.settled.load(Ordering::SeqCst),
                        "second write must not begin until the first has settled"
                    );
                    self.second_started.resolve();
                }
                write_through(absolute_path, content).await
            })
        }

        fn mkdir<'a>(&'a self, _dir: &'a str) -> BoxWriteFuture<'a> {
            noop_mkdir()
        }
    }

    #[tokio::test]
    async fn keeps_write_queue_locked_while_aborted_write_in_flight() {
        let dir = TempDir::new("write-abort-inflight");
        let cwd = dir.cwd().to_string();
        let file_path = dir.path.join("abort-write.txt");

        let first_started = Arc::new(Deferred::new());
        let finish_first = Arc::new(Deferred::new());
        let second_started = Arc::new(Deferred::new());
        let settled = Arc::new(AtomicBool::new(false));

        let ops: Arc<AbortInFlightWriteOperations> = Arc::new(AbortInFlightWriteOperations {
            first_started: first_started.clone(),
            finish_first: finish_first.clone(),
            second_started: second_started.clone(),
            settled: settled.clone(),
        });

        let (tx, rx) = watch::channel(false);

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Start the first (aborted) write; it parks inside `write_file`.
                let first_ops = ops.clone();
                let first_cwd = cwd.clone();
                let first_rx = rx.clone();
                let first = tokio::task::spawn_local(async move {
                    run_write(
                        &first_cwd,
                        &params("abort-write.txt", "first\n"),
                        &*first_ops,
                        Some(&first_rx),
                    )
                    .await
                });

                // Wait until the first write is in flight, then abort it.
                first_started.wait().await;
                let _ = tx.send(true);

                // Start the second write (no signal). It must not begin while the
                // first — aborted but still in flight — holds the queue lock.
                let second_ops = ops.clone();
                let second_cwd = cwd.clone();
                let second = tokio::task::spawn_local(async move {
                    run_write(
                        &second_cwd,
                        &params("abort-write.txt", "second\n"),
                        &*second_ops,
                        None,
                    )
                    .await
                });

                let began =
                    tokio::time::timeout(Duration::from_millis(20), second_started.wait()).await;
                assert!(
                    began.is_err(),
                    "second write must not start while the aborted first is in flight"
                );

                // Release the first write; it finishes its filesystem write, then
                // `run_write` observes the abort and rejects with pi's error.
                finish_first.resolve();
                let first_result = first.await.unwrap();
                assert_eq!(first_result.unwrap_err(), "Operation aborted");

                // The second write now runs to completion.
                let second_result = second.await.unwrap();
                assert_eq!(
                    second_result.unwrap().text,
                    "Successfully wrote 7 bytes to abort-write.txt"
                );
            })
            .await;

        drop(tx);
        // Final on-disk content is the second write's, exactly like pi.
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "second\n");
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode};
    use pidgin_ai::ContentBlock;
    use serde_json::json;
    use std::path::PathBuf;

    fn dark_theme() -> Theme {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/modes/interactive/theme/dark.json");
        let content = std::fs::read_to_string(&path).expect("read dark.json");
        let json = parse_theme_json(&content).expect("parse dark.json");
        create_theme(&json, Some(ColorMode::Color256), None).expect("create dark theme")
    }

    fn ctx<'a>(args: &'a Value, is_error: bool) -> ToolRenderContext<'a> {
        ToolRenderContext {
            args,
            cwd: "/tmp/tool-cwd",
            execution_started: true,
            args_complete: true,
            is_partial: false,
            expanded: false,
            show_images: false,
            is_error,
        }
    }

    fn text_result(text: &str) -> AgentToolResult {
        AgentToolResult {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                text_signature: None,
            }],
            details: Value::Null,
            added_tool_names: None,
            terminate: None,
        }
    }

    fn opts() -> ToolRenderResultOptions {
        ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        }
    }

    #[test]
    fn call_renders_write_header_and_content_preview() {
        let theme = dark_theme();
        let args = json!({ "path": "out.txt", "content": "hello\nworld" });
        let text = format_write_call(&args, &opts(), &theme, "/tmp/tool-cwd");
        assert!(text.contains("write"), "got: {text:?}");
        assert!(text.contains("out.txt"), "got: {text:?}");
        assert!(text.contains("hello"));
        assert!(text.contains("world"));
    }

    #[test]
    fn call_with_empty_content_is_header_only() {
        let theme = dark_theme();
        let args = json!({ "path": "out.txt", "content": "" });
        let text = format_write_call(&args, &opts(), &theme, "/tmp/tool-cwd");
        assert!(text.contains("out.txt"));
        // No content preview (no double-newline body).
        assert!(!text.contains("\n\n"), "got: {text:?}");
    }

    #[test]
    fn call_with_non_string_content_shows_invalid_marker() {
        let theme = dark_theme();
        let args = json!({ "path": "out.txt", "content": 42 });
        let text = format_write_call(&args, &opts(), &theme, "/tmp/tool-cwd");
        assert!(
            text.contains("[invalid content arg - expected string]"),
            "got: {text:?}"
        );
    }

    #[test]
    fn result_is_empty_container_on_success() {
        let theme = dark_theme();
        let args = json!({ "path": "out.txt", "content": "hi" });
        let result = text_result("Successfully wrote 2 bytes to out.txt");
        let out = write_render_result(&result, &opts(), &theme, &ctx(&args, false)).render(80);
        assert!(
            out.is_empty(),
            "success result must render nothing: {out:?}"
        );
    }

    #[test]
    fn result_shows_error_text_on_error() {
        let theme = dark_theme();
        let args = json!({ "path": "out.txt", "content": "hi" });
        let result = text_result("EACCES: permission denied");
        let out = write_render_result(&result, &opts(), &theme, &ctx(&args, true)).render(80);
        let joined = out.join("\n");
        assert!(
            joined.contains("EACCES: permission denied"),
            "got: {joined:?}"
        );
    }
}
