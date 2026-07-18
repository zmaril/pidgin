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
//! Deferred seam: pi's `ls.ts` also carries the `renderCall`/`renderResult` TUI
//! hooks (theme-styled listing with a `[Truncated: ...]` footer). Those are
//! TUI-only and depend on the theme layer, so — as with the other ported tools
//! — they are not reproduced here; only the execute path is ported.

use std::path::Path;

use tokio::sync::watch;

use super::path_utils::resolve_to_cwd;
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
/// (for example SSH). Kept a trait so extensions can supply their own backend.
#[allow(async_fn_in_trait)] // Local seam; callers use concrete impls, not `dyn`.
pub trait LsOperations {
    /// Check whether `absolute_path` exists.
    async fn exists(&self, absolute_path: &str) -> bool;
    /// Get metadata for `absolute_path`. Returns `Err` if it cannot be stat'd.
    async fn stat(&self, absolute_path: &str) -> Result<Metadata, String>;
    /// Read the entry names of the directory at `absolute_path`.
    async fn readdir(&self, absolute_path: &str) -> Result<Vec<String>, String>;
}

/// The default local-filesystem [`LsOperations`], backed by `std::fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalLsOperations;

impl LsOperations for LocalLsOperations {
    async fn exists(&self, absolute_path: &str) -> bool {
        Path::new(absolute_path).exists()
    }

    async fn stat(&self, absolute_path: &str) -> Result<Metadata, String> {
        let meta = std::fs::metadata(absolute_path).map_err(|e| e.to_string())?;
        Ok(Metadata::new(meta.is_dir()))
    }

    async fn readdir(&self, absolute_path: &str) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        let read = std::fs::read_dir(absolute_path).map_err(|e| e.to_string())?;
        for entry in read {
            let entry = entry.map_err(|e| e.to_string())?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
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
pub async fn run_ls<O: LsOperations>(
    cwd: &str,
    params: &LsParams,
    ops: &O,
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
        async fn exists(&self, _absolute_path: &str) -> bool {
            true
        }

        async fn stat(&self, absolute_path: &str) -> Result<Metadata, String> {
            // The root listing target is a directory; entries are dirs when
            // their basename is in `dirs`.
            let name = Path::new(absolute_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Metadata::new(
                self.dirs.contains(&name) || !self.entries.iter().any(|e| e == &name),
            ))
        }

        async fn readdir(&self, _absolute_path: &str) -> Result<Vec<String>, String> {
            Ok(self.entries.clone())
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
