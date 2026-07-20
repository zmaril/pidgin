//! Port of pi's `core/tools/ls.ts`.
//!
//! The ls tool lists a directory's entries, sorted case-insensitively, with a
//! `/` suffix on subdirectories, then applies an entry-count cap and the shared
//! byte-cap truncation, emitting the same actionable notices as pi. Directory
//! access goes through a pluggable [`LsOperations`] trait (pi's `LsOperations`
//! interface) so extensions can swap the backend (for example an SSH-backed
//! remote filesystem); [`create_local_ls_operations`] returns the default
//! std-backed implementation.
//!
//! The TUI render hooks ([`ls_render_call`]/[`ls_render_result`]) are ported
//! here as **stateless** functions (pi reuses a `Text` via
//! `context.lastComponent`, but the output is a pure function of its inputs). ls
//! uses the DEFAULT render shell, so the returned `Text` is composed into the
//! shell's call/result box.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::watch;

use pidgin_agent::types::AgentToolResult;
use pidgin_tui::renderer::Component;
use pidgin_tui::Text;

use crate::core::extensions::types::{ToolRenderContext, ToolRenderResultOptions};
use crate::modes::interactive::theme::runtime::Theme;

use super::path_utils::resolve_to_cwd;
use super::render_utils::{
    detail_usize, get_text_output_from_blocks, json_number_display, render_tool_path, str_json,
    tools_expand_hint,
};
use super::truncate::{
    format_size, truncate_head, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
};

/// Default entry cap when the caller does not supply a `limit` (pi's
/// `DEFAULT_LIMIT`).
pub const DEFAULT_LIMIT: usize = 500;

/// Input parameters for the ls tool (pi's `{ path?, limit? }` schema).
#[derive(Debug, Clone, Default)]
pub struct LsParams {
    /// Directory to list (default: current directory).
    pub path: Option<String>,
    /// Maximum number of entries to return (default: 500).
    pub limit: Option<usize>,
}

/// Minimal stat metadata, mirroring pi's `{ isDirectory: () => boolean }`.
#[derive(Debug, Clone, Copy)]
pub struct Metadata {
    is_dir: bool,
}

impl Metadata {
    /// Construct metadata from a directory flag.
    pub fn new(is_dir: bool) -> Self {
        Self { is_dir }
    }

    /// Whether the entry is a directory.
    pub fn is_directory(&self) -> bool {
        self.is_dir
    }
}

/// The result of an ls run: the formatted listing plus truncation accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsResult {
    /// The formatted output text (listing + notices, or `(empty directory)`).
    pub text: String,
    /// The entry limit that was reached, if any (pi's `entryLimitReached`).
    pub entry_limit_reached: Option<usize>,
    /// Byte-cap truncation accounting, if truncation occurred.
    pub truncation: Option<TruncationResult>,
}

/// Pluggable operations for the ls tool, mirroring pi's `LsOperations`
/// interface. Override these to delegate directory listing to remote systems
/// (for example SSH). Kept a `dyn`-object-safe trait (methods return boxed
/// futures) so a custom backend can be injected as `Arc<dyn LsOperations>` — for
/// example across the napi boundary.
///
/// The `Send + Sync` supertrait makes the *backend value* shareable across
/// threads (`Arc<dyn LsOperations>: Send + Sync`); the returned futures are not
/// `+ Send`, matching the other tool seams (driven via `block_on`).
pub trait LsOperations: Send + Sync {
    /// Check whether `absolute_path` exists.
    fn exists<'a>(&'a self, absolute_path: &'a str) -> Pin<Box<dyn Future<Output = bool> + 'a>>;
    /// Get metadata for `absolute_path`. Returns `Err` if it cannot be stat'd.
    fn stat<'a>(
        &'a self,
        absolute_path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Metadata, String>> + 'a>>;
    /// Read the entry names of the directory at `absolute_path`.
    fn readdir<'a>(
        &'a self,
        absolute_path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>>;
}

/// The default local-filesystem [`LsOperations`], backed by `std::fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalLsOperations;

impl LsOperations for LocalLsOperations {
    fn exists<'a>(&'a self, absolute_path: &'a str) -> Pin<Box<dyn Future<Output = bool> + 'a>> {
        Box::pin(async move { Path::new(absolute_path).exists() })
    }

    fn stat<'a>(
        &'a self,
        absolute_path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Metadata, String>> + 'a>> {
        Box::pin(async move {
            let meta = std::fs::metadata(absolute_path).map_err(|e| e.to_string())?;
            Ok(Metadata::new(meta.is_dir()))
        })
    }

    fn readdir<'a>(
        &'a self,
        absolute_path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
        Box::pin(async move {
            let mut names = Vec::new();
            let read = std::fs::read_dir(absolute_path).map_err(|e| e.to_string())?;
            for entry in read {
                let entry = entry.map_err(|e| e.to_string())?;
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            Ok(names)
        })
    }
}

/// Construct the default local-filesystem ls operations (pi's
/// `defaultLsOperations` / `createLocalLsOperations` analog).
pub fn create_local_ls_operations() -> LocalLsOperations {
    LocalLsOperations
}

/// Execute the ls tool: resolve the directory, verify it exists and is a
/// directory, list and sort its entries case-insensitively, mark directories
/// with a trailing `/`, cap at `limit`, byte-truncate, and append the notices.
///
/// `signal` is an optional cancellation channel modeled after the foundation
/// modules (a `watch::Receiver<bool>` that flips to `true` on abort); it is
/// checked up front, mirroring pi's `if (signal?.aborted)` fast path, returning
/// `Err("Operation aborted")` to match pi's thrown error.
pub async fn run_ls(
    cwd: &str,
    params: &LsParams,
    ops: &dyn LsOperations,
    signal: Option<&watch::Receiver<bool>>,
) -> Result<LsResult, String> {
    if signal.map(|s| *s.borrow()).unwrap_or(false) {
        return Err("Operation aborted".to_string());
    }

    // pi uses `path || "."`, so an absent OR empty path falls back to ".".
    let requested = match params.path.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => ".",
    };
    let dir_path = resolve_to_cwd(requested, cwd).map_err(|e| e.to_string())?;
    let effective_limit = params.limit.unwrap_or(DEFAULT_LIMIT);

    // Check if path exists.
    if !ops.exists(&dir_path).await {
        return Err(format!("Path not found: {dir_path}"));
    }

    // Check if path is a directory.
    let stat = ops.stat(&dir_path).await?;
    if !stat.is_directory() {
        return Err(format!("Not a directory: {dir_path}"));
    }

    // Read directory entries.
    let mut entries = ops
        .readdir(&dir_path)
        .await
        .map_err(|e| format!("Cannot read directory: {e}"))?;

    // Sort alphabetically, case-insensitive. pi uses
    // `a.toLowerCase().localeCompare(b.toLowerCase())`; Rust has no built-in
    // Unicode collator, so we lowercase and compare by scalar order. This
    // matches `localeCompare`'s ordering for ASCII names; locale-specific
    // collation of non-ASCII names (accent/punctuation weighting) may differ.
    entries.sort_by_cached_key(|a| a.to_lowercase());

    // Format entries with directory indicators.
    let mut results: Vec<String> = Vec::new();
    let mut entry_limit_reached = false;
    for entry in &entries {
        if results.len() >= effective_limit {
            entry_limit_reached = true;
            break;
        }

        let full_path = Path::new(&dir_path).join(entry);
        let full_path = full_path.to_string_lossy();
        let suffix = match ops.stat(&full_path).await {
            Ok(entry_stat) if entry_stat.is_directory() => "/",
            Ok(_) => "",
            // Skip entries we cannot stat.
            Err(_) => continue,
        };
        results.push(format!("{entry}{suffix}"));
    }

    if results.is_empty() {
        return Ok(LsResult {
            text: "(empty directory)".to_string(),
            entry_limit_reached: None,
            truncation: None,
        });
    }

    let raw_output = results.join("\n");
    // Apply byte truncation. There is no separate line limit because entry
    // count is already capped.
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: usize::MAX,
            max_bytes: DEFAULT_MAX_BYTES,
        },
    );
    let mut output = truncation.content.clone();

    // Build actionable notices for truncation and entry limits.
    let mut notices: Vec<String> = Vec::new();
    let mut result_entry_limit: Option<usize> = None;
    let mut result_truncation: Option<TruncationResult> = None;
    if entry_limit_reached {
        notices.push(format!(
            "{effective_limit} entries limit reached. Use limit={} for more",
            effective_limit * 2
        ));
        result_entry_limit = Some(effective_limit);
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        result_truncation = Some(truncation);
    }
    if !notices.is_empty() {
        output += &format!("\n\n[{}]", notices.join(". "));
    }

    Ok(LsResult {
        text: output,
        entry_limit_reached: result_entry_limit,
        truncation: result_truncation,
    })
}

// ---------------------------------------------------------------------------
// TUI render hooks (pi's `renderCall` / `renderResult`, `ls.ts:210` / `:215`)
// ---------------------------------------------------------------------------

/// Local `theme.fg` wrapper falling back to unstyled text on an unknown color
/// key (pi's `theme.fg` is infallible; the ported [`Theme::fg`] returns a
/// `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// Format the ls call header (pi's `formatLsCall`): `ls <path>` with an empty
/// path defaulting to `.`, plus an optional ` (limit N)` suffix.
fn format_ls_call(args: &Value, theme: &Theme, cwd: &str) -> String {
    let path_display =
        render_tool_path(str_json(args.get("path")).as_deref(), theme, cwd, Some("."));
    let mut text = format!(
        "{} {}",
        fg(theme, "toolTitle", &theme.bold("ls")),
        path_display
    );
    if let Some(limit) = args.get("limit") {
        text += &fg(
            theme,
            "toolOutput",
            &format!(" (limit {})", json_number_display(limit)),
        );
    }
    text
}

/// Format the ls result body (pi's `formatLsResult`): the listing (up to 20
/// lines unless expanded) plus a `[Truncated: …]` footer for entry/byte caps.
fn format_ls_result(
    result: &AgentToolResult,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
) -> String {
    let output = get_text_output_from_blocks(&result.content, show_images);
    let output = output.trim();
    let mut text = String::new();
    if !output.is_empty() {
        let lines: Vec<&str> = output.split('\n').collect();
        let max_lines = if options.expanded { lines.len() } else { 20 };
        let display_lines = &lines[..max_lines.min(lines.len())];
        let remaining = lines.len() as isize - max_lines as isize;
        text += &format!(
            "\n{}",
            display_lines
                .iter()
                .map(|line| fg(theme, "toolOutput", line))
                .collect::<Vec<_>>()
                .join("\n")
        );
        if remaining > 0 {
            text += &fg(theme, "muted", &format!("\n... ({remaining} more lines,"));
            text += " ";
            text += &tools_expand_hint(theme, "to expand");
            text += &fg(theme, "muted", ")");
        }
    }

    let entry_limit = result
        .details
        .get("entryLimitReached")
        .and_then(Value::as_u64)
        .filter(|&e| e != 0);
    let truncation = result.details.get("truncation");
    let truncated = truncation
        .and_then(|t| t.get("truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if entry_limit.is_some() || truncated {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(e) = entry_limit {
            warnings.push(format!("{e} entries limit"));
        }
        if truncated {
            let max_bytes = truncation
                .and_then(|t| detail_usize(t, "maxBytes"))
                .unwrap_or(DEFAULT_MAX_BYTES);
            warnings.push(format!("{} limit", format_size(max_bytes)));
        }
        text += &format!(
            "\n{}",
            fg(
                theme,
                "warning",
                &format!("[Truncated: {}]", warnings.join(", "))
            )
        );
    }
    text
}

/// Custom rendering for the ls tool call (pi's `renderCall`, `ls.ts:210`).
pub fn ls_render_call(
    args: &Value,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    Box::new(Text::new(
        &format_ls_call(args, theme, context.cwd),
        0,
        0,
        None,
    ))
}

/// Custom rendering for the ls tool result (pi's `renderResult`, `ls.ts:215`).
pub fn ls_render_result(
    result: &AgentToolResult,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    Box::new(Text::new(
        &format_ls_result(result, options, theme, context.show_images),
        0,
        0,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::test_support::TempDir;
    use std::collections::HashSet;

    #[tokio::test]
    async fn lists_entries_sorted_case_insensitively_with_dir_slashes() {
        let dir = TempDir::new("ls-basic");
        dir.write("Banana.txt", "");
        dir.write("apple.txt", "");
        dir.write("Cherry.txt", "");
        dir.mkdir("zsub");
        let ops = create_local_ls_operations();
        let out = run_ls(dir.cwd(), &LsParams::default(), &ops, None)
            .await
            .unwrap();
        // Case-insensitive: apple, Banana, Cherry, zsub (dir -> trailing "/").
        assert_eq!(out.text, "apple.txt\nBanana.txt\nCherry.txt\nzsub/");
        assert!(out.entry_limit_reached.is_none());
        assert!(out.truncation.is_none());
    }

    #[tokio::test]
    async fn reports_empty_directory() {
        let dir = TempDir::new("ls-empty");
        let ops = create_local_ls_operations();
        let out = run_ls(dir.cwd(), &LsParams::default(), &ops, None)
            .await
            .unwrap();
        assert_eq!(out.text, "(empty directory)");
    }

    #[tokio::test]
    async fn caps_entries_at_limit_and_emits_notice() {
        let dir = TempDir::new("ls-limit");
        dir.write("a.txt", "");
        dir.write("b.txt", "");
        dir.write("c.txt", "");
        let ops = create_local_ls_operations();
        let out = run_ls(
            dir.cwd(),
            &LsParams {
                path: None,
                limit: Some(2),
            },
            &ops,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            out.text,
            "a.txt\nb.txt\n\n[2 entries limit reached. Use limit=4 for more]"
        );
        assert_eq!(out.entry_limit_reached, Some(2));
    }

    #[tokio::test]
    async fn errors_when_path_missing() {
        let dir = TempDir::new("ls-missing");
        let missing = dir.path.join("nope");
        let ops = create_local_ls_operations();
        let err = run_ls(
            dir.cwd(),
            &LsParams {
                path: Some(missing.to_string_lossy().into_owned()),
                limit: None,
            },
            &ops,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            format!("Path not found: {}", missing.to_string_lossy())
        );
    }

    #[tokio::test]
    async fn errors_when_path_not_a_directory() {
        let dir = TempDir::new("ls-notdir");
        let file = dir.write("file.txt", "x");
        let ops = create_local_ls_operations();
        let err = run_ls(
            dir.cwd(),
            &LsParams {
                path: Some(file.to_string_lossy().into_owned()),
                limit: None,
            },
            &ops,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err, format!("Not a directory: {}", file.to_string_lossy()));
    }

    #[tokio::test]
    async fn returns_operation_aborted_when_signal_set() {
        let dir = TempDir::new("ls-abort");
        let ops = create_local_ls_operations();
        let (tx, rx) = watch::channel(true);
        let err = run_ls(dir.cwd(), &LsParams::default(), &ops, Some(&rx))
            .await
            .unwrap_err();
        assert_eq!(err, "Operation aborted");
        drop(tx);
    }

    /// A fake [`LsOperations`] returning canned entries and directory flags —
    /// proving the operations seam is pluggable without touching the filesystem.
    struct FakeLsOperations {
        entries: Vec<String>,
        dirs: HashSet<String>,
    }

    impl LsOperations for FakeLsOperations {
        fn exists<'a>(
            &'a self,
            _absolute_path: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + 'a>> {
            Box::pin(async { true })
        }

        fn stat<'a>(
            &'a self,
            absolute_path: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Metadata, String>> + 'a>> {
            Box::pin(async move {
                // The root listing target is a directory; entries are dirs when
                // their basename is in `dirs`.
                let name = Path::new(absolute_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(Metadata::new(
                    self.dirs.contains(&name) || !self.entries.iter().any(|e| e == &name),
                ))
            })
        }

        fn readdir<'a>(
            &'a self,
            _absolute_path: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
            Box::pin(async move { Ok(self.entries.clone()) })
        }
    }

    #[tokio::test]
    async fn uses_injected_operations_backend() {
        let ops = FakeLsOperations {
            entries: vec!["Zeta".to_string(), "alpha".to_string(), "docs".to_string()],
            dirs: ["docs".to_string()].into_iter().collect(),
        };
        let out = run_ls("/virtual", &LsParams::default(), &ops, None)
            .await
            .unwrap();
        // Case-insensitive sort: alpha, docs (dir), Zeta.
        assert_eq!(out.text, "alpha\ndocs/\nZeta");
    }

    #[tokio::test]
    async fn emits_byte_cap_notice_via_injected_backend() {
        // Build enough long entries to exceed the 50KB byte cap so the
        // size-limit notice is emitted. Each name is a file (not a dir).
        let entries: Vec<String> = (0..400)
            .map(|i| format!("{i:05}-{}", "x".repeat(200)))
            .collect();
        let ops = FakeLsOperations {
            entries,
            dirs: HashSet::new(),
        };
        let out = run_ls(
            "/virtual",
            &LsParams {
                path: None,
                limit: Some(usize::MAX),
            },
            &ops,
            None,
        )
        .await
        .unwrap();
        assert!(
            out.text.ends_with(&format!(
                "[{} limit reached]",
                format_size(DEFAULT_MAX_BYTES)
            )),
            "expected byte-cap notice, got tail: {:?}",
            &out.text[out.text.len().saturating_sub(60)..]
        );
        assert!(out.truncation.is_some());
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

    fn text_result(text: &str, details: Value) -> AgentToolResult {
        AgentToolResult {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                text_signature: None,
            }],
            details,
            added_tool_names: None,
            terminate: None,
        }
    }

    fn opts(expanded: bool) -> ToolRenderResultOptions {
        ToolRenderResultOptions {
            expanded,
            is_partial: false,
        }
    }

    #[test]
    fn call_renders_ls_header() {
        let theme = dark_theme();
        let args = json!({ "path": "src" });
        let text = format_ls_call(&args, &theme, "/tmp/tool-cwd");
        assert!(text.contains("ls"));
        assert!(text.contains("src"));
        assert!(!text.contains("limit"));
    }

    #[test]
    fn call_with_limit_shows_limit_suffix() {
        let theme = dark_theme();
        let args = json!({ "path": "src", "limit": 100 });
        let text = format_ls_call(&args, &theme, "/tmp/tool-cwd");
        assert!(text.contains("(limit 100)"), "got: {text:?}");
    }

    #[test]
    fn empty_path_defaults_to_dot() {
        let theme = dark_theme();
        let args = json!({});
        let text = format_ls_call(&args, &theme, "/tmp/tool-cwd");
        assert!(text.contains('.'), "got: {text:?}");
    }

    #[test]
    fn result_lists_entries_with_tool_output() {
        let theme = dark_theme();
        let result = text_result("a.txt\nb.txt\nsub/", Value::Null);
        let body = format_ls_result(&result, &opts(false), &theme, false);
        assert!(body.starts_with('\n'));
        assert!(body.contains("a.txt"));
        assert!(body.contains("sub/"));
    }

    #[test]
    fn result_shows_truncation_footer_for_entry_and_byte_caps() {
        let theme = dark_theme();
        let details = json!({ "entryLimitReached": 500, "truncation": { "truncated": true, "maxBytes": 51200 } });
        let result = text_result("a.txt", details);
        let body = format_ls_result(&result, &opts(false), &theme, false);
        assert!(body.contains("[Truncated:"), "got: {body:?}");
        assert!(body.contains("500 entries limit"), "got: {body:?}");
        assert!(body.contains("50.0KB limit"), "got: {body:?}");
    }
}
