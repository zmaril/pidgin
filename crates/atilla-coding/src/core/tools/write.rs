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
//! Deferred seam: pi's `write.ts` also carries a large syntax-highlight
//! render/caching layer (`WriteCallRenderComponent`, incremental highlight
//! cache) plus the `renderCall`/`renderResult` TUI hooks. Those are TUI-only
//! and depend on the theme layer, so — as with the other ported tools — they
//! are not reproduced here; only the execute path is ported.

use std::path::{Path, PathBuf};

use tokio::sync::watch;

use super::file_mutation_queue::with_file_mutation_queue;
use super::path_utils::resolve_to_cwd;

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
/// example SSH). Kept a trait so extensions can supply their own backend.
#[allow(async_fn_in_trait)] // Local seam; callers use concrete impls, not `dyn`.
pub trait WriteOperations {
    /// Write `content` to the file at `absolute_path`.
    async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), String>;
    /// Create `dir` (and any missing parents) recursively.
    async fn mkdir(&self, dir: &str) -> Result<(), String>;
}

/// The default local-filesystem [`WriteOperations`], backed by `tokio::fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalWriteOperations;

impl WriteOperations for LocalWriteOperations {
    async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), String> {
        tokio::fs::write(absolute_path, content)
            .await
            .map_err(|e| e.to_string())
    }

    async fn mkdir(&self, dir: &str) -> Result<(), String> {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| e.to_string())
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
pub async fn run_write<O: WriteOperations>(
    cwd: &str,
    params: &WriteParams,
    ops: &O,
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
        async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), String> {
            self.writes
                .lock()
                .unwrap()
                .push((absolute_path.to_string(), content.to_string()));
            Ok(())
        }

        async fn mkdir(&self, dir: &str) -> Result<(), String> {
            self.mkdirs.lock().unwrap().push(dir.to_string());
            Ok(())
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
}
