// straitjacket-allow-file:duplication — the per-code `as_str` match arms and the
// parallel `FileError`/`ExecutionError` error-type shapes mirror pi's stable code
// unions by design; the clone detector reads these mirrored arms as duplicates.
//! The harness execution-environment contract, mirroring the `FileSystem`,
//! `Shell`, and `ExecutionEnv` interfaces plus the `Result` monad and error
//! types of `packages/agent/src/harness/types.ts`.
//!
//! # Faithful divergences from pi
//!
//! - **Synchronous.** pi's `FileSystem`/`Shell` methods are `async` and return
//!   `Promise<Result<T, E>>`; per the contract they *never* throw — every
//!   failure is encoded in the returned `Result`. This port drops `async`
//!   entirely (no tokio): methods return `Result<T, E>` directly and eagerly.
//! - **Native `Result`.** pi models `Result<TValue, TError>` as a tagged union
//!   `{ ok: true; value } | { ok: false; error }`. Rust's own
//!   [`core::result::Result`] *is* that monad, so it is reused directly; the
//!   [`ok`]/[`err`]/[`get_or_throw`]/[`get_or_undefined`]/[`to_error`] helpers
//!   mirror pi's free functions.
//! - **Cooperative `AbortSignal`.** pi threads an optional `AbortSignal`
//!   through every `FileSystem`/`Shell` method for cooperative cancellation.
//!   This port carries the same seam as a `signal: Option<&AbortSignal>`
//!   parameter (and, for [`Shell::exec`], the `abort_signal` field of
//!   [`ShellExecOptions`], mirroring pi's `ShellExecOptions.abortSignal`). At
//!   the exact points pi checks `signal.aborted` it returns the stable
//!   [`FileErrorCode::Aborted`]/[`ExecutionErrorCode::Aborted`] codes. The
//!   signal is cooperative (an `Arc<AtomicBool>`): the eager, synchronous port
//!   has no async `await` to interrupt, so pi's mid-`await` re-checks that guard
//!   an async gap absent here collapse to the entry check, and the wiring of the
//!   signal to real syscall/process cancellation is deliberately not reproduced.
//!
//! This is the harness's *own* rich contract. It is distinct from
//! `pidgin_ai::seams::storage::ExecutionEnv`, the reduced 5-method seam the
//! session/storage layer injects; the two are deliberately not shared.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};

use pidgin_ai::seams::AbortSignal;

mod nodejs;

pub use nodejs::NodeExecutionEnv;

/// pi's `abortResult`: an `aborted`-coded [`FileError`] for `path` when `signal`
/// is tripped, else `None`. The cooperative analog of pi's `signal?.aborted`
/// guard (`nodejs-env.ts`'s `abortResult`), shared by every `FileSystem` method
/// that checks the signal at pi's check points.
fn aborted_file_error(signal: Option<&AbortSignal>, path: &str) -> Option<FileError> {
    signal
        .is_some_and(AbortSignal::is_aborted)
        .then(|| FileError::with_path(FileErrorCode::Aborted, "aborted", path))
}

/// The cooperative analog of pi's inline `err(new ExecutionError("aborted",
/// "aborted"))` guard in `exec`: an `aborted`-coded [`ExecutionError`] when
/// `signal` is tripped, else `None`.
fn aborted_execution_error(signal: Option<&AbortSignal>) -> Option<ExecutionError> {
    signal
        .is_some_and(AbortSignal::is_aborted)
        .then(|| ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"))
}

// ---------------------------------------------------------------------------
// Result monad helpers (`types.ts:5-38`)
// ---------------------------------------------------------------------------

/// Create a successful [`Result`]. Mirrors pi's `ok`.
///
/// `Ok(value)` is the native constructor; this wrapper exists for parity with
/// pi's helper set.
#[allow(clippy::unnecessary_wraps)]
pub fn ok<T, E>(value: T) -> Result<T, E> {
    Ok(value)
}

/// Create a failed [`Result`]. Mirrors pi's `err`.
#[allow(clippy::unnecessary_wraps)]
pub fn err<T, E>(error: E) -> Result<T, E> {
    Err(error)
}

/// Return the success value or panic with the failure error. Intended for tests
/// and explicit adapter boundaries, mirroring pi's `getOrThrow` (which throws
/// the error).
pub fn get_or_throw<T, E: fmt::Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("get_or_throw on Err: {error:?}"),
    }
}

/// Return the success value or `None`. Mirrors pi's `getOrUndefined`.
///
/// pi restricts the value to `object` to dodge JS truthiness bugs with
/// primitives; Rust's `Option` has no such hazard, so any value is allowed.
pub fn get_or_undefined<T, E>(result: Result<T, E>) -> Option<T> {
    result.ok()
}

/// Normalize any displayable error into its message string. Mirrors pi's
/// `toError`, which coerces unknown thrown values into `Error` instances; in
/// Rust the errors that flow through this contract are already typed, so the
/// faithful essence is collapsing a value to the message an `Error` would carry.
pub fn to_error<E: fmt::Display>(error: E) -> String {
    error.to_string()
}

// ---------------------------------------------------------------------------
// File errors (`types.ts:107-134`)
// ---------------------------------------------------------------------------

/// Kind of filesystem object as addressed by a [`FileSystem`]. Symlinks are not
/// followed automatically. Mirrors pi's `FileKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

impl FileKind {
    /// The wire string pi uses (`"file" | "directory" | "symlink"`).
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::File => "file",
            FileKind::Directory => "directory",
            FileKind::Symlink => "symlink",
        }
    }
}

/// Stable, backend-independent file error codes returned by [`FileSystem`]
/// operations. Mirrors pi's `FileErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileErrorCode {
    Aborted,
    NotFound,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    Invalid,
    NotSupported,
    Unknown,
}

impl FileErrorCode {
    /// The wire string pi uses for this code (`FileError.code`).
    pub fn as_str(self) -> &'static str {
        match self {
            FileErrorCode::Aborted => "aborted",
            FileErrorCode::NotFound => "not_found",
            FileErrorCode::PermissionDenied => "permission_denied",
            FileErrorCode::NotDirectory => "not_directory",
            FileErrorCode::IsDirectory => "is_directory",
            FileErrorCode::Invalid => "invalid",
            FileErrorCode::NotSupported => "not_supported",
            FileErrorCode::Unknown => "unknown",
        }
    }
}

/// Error returned by [`FileSystem`] operations. Mirrors pi's `FileError` (the
/// `cause` chain is dropped, matching this crate's `SessionError` port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    /// Backend-independent error code.
    pub code: FileErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Absolute addressed path associated with the failure, when available.
    pub path: Option<String>,
}

impl FileError {
    /// A `FileError` with no associated path.
    pub fn new(code: FileErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
        }
    }

    /// A `FileError` carrying the addressed path.
    pub fn with_path(
        code: FileErrorCode,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            path: Some(path.into()),
        }
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for FileError {}

// ---------------------------------------------------------------------------
// Execution errors (`types.ts:136-155`)
// ---------------------------------------------------------------------------

/// Stable, backend-independent execution error codes returned by
/// [`Shell::exec`]. Mirrors pi's `ExecutionErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    Unknown,
}

impl ExecutionErrorCode {
    /// The wire string pi uses for this code (`ExecutionError.code`).
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionErrorCode::Aborted => "aborted",
            ExecutionErrorCode::Timeout => "timeout",
            ExecutionErrorCode::ShellUnavailable => "shell_unavailable",
            ExecutionErrorCode::SpawnError => "spawn_error",
            ExecutionErrorCode::CallbackError => "callback_error",
            ExecutionErrorCode::Unknown => "unknown",
        }
    }
}

/// Error returned by [`Shell::exec`]. Mirrors pi's `ExecutionError` (the `cause`
/// chain is dropped, matching this crate's `SessionError` port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionError {
    /// Backend-independent error code.
    pub code: ExecutionErrorCode,
    /// Human-readable message.
    pub message: String,
}

impl ExecutionError {
    /// An `ExecutionError` with the given code and message.
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Error for ExecutionError {}

// ---------------------------------------------------------------------------
// FileInfo & file content (`types.ts:229-241`, writeFile/appendFile union)
// ---------------------------------------------------------------------------

/// Metadata for one filesystem object in a [`FileSystem`]. Mirrors pi's
/// `FileInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Basename of [`FileInfo::path`].
    pub name: String,
    /// Absolute, syntactically normalized addressed path. Symlinks are not
    /// followed.
    pub path: String,
    /// Object kind. Symlink targets are not followed.
    pub kind: FileKind,
    /// Size in bytes for the addressed filesystem object.
    pub size: u64,
    /// Modification time as milliseconds since the Unix epoch.
    pub mtime_ms: i64,
}

/// Content passed to [`FileSystem::write_file`]/[`FileSystem::append_file`].
/// Mirrors pi's `string | Uint8Array` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileContent<'a> {
    /// UTF-8 text.
    Text(&'a str),
    /// Raw bytes.
    Bytes(&'a [u8]),
}

impl FileContent<'_> {
    /// The content as a byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            FileContent::Text(text) => text.as_bytes(),
            FileContent::Bytes(bytes) => bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// Shell exec options & output (`types.ts:304-329`)
// ---------------------------------------------------------------------------

/// A streamed-output callback invoked with chunks as they are produced.
pub type OutputCallback<'a> = Box<dyn FnMut(&str) + 'a>;

/// Options for [`Shell::exec`]. Mirrors pi's `ShellExecOptions`. The
/// `on_stdout`/`on_stderr` callbacks receive output chunks as they are produced.
#[derive(Default)]
pub struct ShellExecOptions<'a> {
    /// Working directory for the command. Relative paths resolve against the
    /// environment's cwd.
    pub cwd: Option<String>,
    /// Additional environment variables. Values override the defaults.
    pub env: Option<BTreeMap<String, String>>,
    /// Timeout in seconds. Implementations should return a timeout error when
    /// the command exceeds this duration.
    pub timeout: Option<f64>,
    /// Cooperative abort signal used to terminate the command. Mirrors pi's
    /// `ShellExecOptions.abortSignal`. Defaults to no signal.
    pub abort_signal: Option<&'a AbortSignal>,
    /// Called with stdout chunks as they are produced.
    pub on_stdout: Option<OutputCallback<'a>>,
    /// Called with stderr chunks as they are produced.
    pub on_stderr: Option<OutputCallback<'a>>,
}

/// The captured result of a completed shell command. Mirrors pi's
/// `{ stdout; stderr; exitCode }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

// ---------------------------------------------------------------------------
// FileSystem / Shell / ExecutionEnv traits (`types.ts:243-332`)
// ---------------------------------------------------------------------------

/// Filesystem capability used by the harness. Mirrors pi's `FileSystem`.
///
/// Paths passed to methods may be absolute or relative to [`FileSystem::cwd`].
/// Operations must never panic: every failure, including unexpected backend
/// failures, is encoded in the returned `Result`.
pub trait FileSystem {
    /// Current working directory for relative paths.
    fn cwd(&self) -> String;
    /// Return an absolute addressed path without requiring it to exist and
    /// without resolving symlinks.
    fn absolute_path(&self, path: &str, signal: Option<&AbortSignal>) -> Result<String, FileError>;
    /// Join path segments in the filesystem namespace.
    fn join_path(&self, parts: &[&str], signal: Option<&AbortSignal>) -> Result<String, FileError>;
    /// Read a UTF-8 text file.
    fn read_text_file(&self, path: &str, signal: Option<&AbortSignal>)
        -> Result<String, FileError>;
    /// Read UTF-8 text lines, stopping once `max_lines` lines have been read.
    fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<String>, FileError>;
    /// Read a binary file.
    fn read_binary_file(
        &self,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<u8>, FileError>;
    /// Create or overwrite a file, creating parent directories when supported.
    fn write_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        signal: Option<&AbortSignal>,
    ) -> Result<(), FileError>;
    /// Create or append to a file, creating parent directories when supported.
    fn append_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        signal: Option<&AbortSignal>,
    ) -> Result<(), FileError>;
    /// Return metadata for the addressed path without following symlinks.
    fn file_info(&self, path: &str, signal: Option<&AbortSignal>) -> Result<FileInfo, FileError>;
    /// List direct children of a directory without following symlinks.
    fn list_dir(
        &self,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<FileInfo>, FileError>;
    /// Return the canonical path for an existing path, resolving symlinks where
    /// supported.
    fn canonical_path(&self, path: &str, signal: Option<&AbortSignal>)
        -> Result<String, FileError>;
    /// Return false for missing paths. Other errors return a [`FileError`].
    fn exists(&self, path: &str, signal: Option<&AbortSignal>) -> Result<bool, FileError>;
    /// Create a directory (pi default: `recursive: true`).
    fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        signal: Option<&AbortSignal>,
    ) -> Result<(), FileError>;
    /// Remove a file or directory (pi defaults: `recursive: false`,
    /// `force: false`).
    fn remove(
        &self,
        path: &str,
        recursive: bool,
        force: bool,
        signal: Option<&AbortSignal>,
    ) -> Result<(), FileError>;
    /// Create a temporary directory and return its absolute path (pi default
    /// prefix: `"tmp-"`).
    fn create_temp_dir(
        &self,
        prefix: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<String, FileError>;
    /// Create a temporary file and return its absolute path (pi defaults:
    /// prefix `""`, suffix `""`).
    fn create_temp_file(
        &self,
        prefix: &str,
        suffix: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<String, FileError>;
    /// Release filesystem resources. Best-effort; must not panic.
    fn cleanup(&self);
}

/// Shell execution capability used by the harness. Mirrors pi's `Shell`.
pub trait Shell {
    /// Execute a shell command in the environment's cwd unless `options.cwd` is
    /// provided.
    fn exec(
        &self,
        command: &str,
        options: ShellExecOptions<'_>,
    ) -> Result<ShellExecOutput, ExecutionError>;
    /// Release shell resources. Best-effort; must not panic.
    fn cleanup(&self);
}

/// Filesystem and process execution environment used by the harness. Mirrors
/// pi's `ExecutionEnv extends FileSystem, Shell`.
pub trait ExecutionEnv: FileSystem + Shell {}

impl<T: FileSystem + Shell> ExecutionEnv for T {}

// ---------------------------------------------------------------------------
// MemoryExecutionEnv — in-memory test double
// ---------------------------------------------------------------------------

/// A scripted shell outcome for [`MemoryExecutionEnv`].
enum ExecOutcome {
    /// Streamed stdout/stderr chunks and a final exit code.
    Output {
        stdout: Vec<String>,
        stderr: Vec<String>,
        exit_code: i32,
    },
    /// A shell failure.
    Failure(ExecutionError),
}

#[derive(Default)]
struct MemState {
    cwd: String,
    files: BTreeMap<String, Vec<u8>>,
    /// Directory paths present in the tree (kind `directory`). Directories are
    /// otherwise implicit for plain files, but skills/prompt-template loading
    /// stats and lists directories, so they are tracked explicitly.
    dirs: BTreeSet<String>,
    /// Symlink path -> target path. `file_info` reports these as `symlink`
    /// (lstat-style, final component not followed); `read_text_file`,
    /// `list_dir`, and `canonical_path` follow them (stat-style).
    symlinks: BTreeMap<String, String>,
    temp_counter: u64,
    exec_queue: VecDeque<ExecOutcome>,
}

/// A deterministic in-memory [`ExecutionEnv`] for exercising the harness leaves
/// without a real filesystem or shell.
///
/// Files live in a map; `exec` replays queued scripted outcomes (via
/// [`MemoryExecutionEnv::push_exec_output`]/[`MemoryExecutionEnv::push_exec_failure`]),
/// streaming each chunk to the `on_stdout`/`on_stderr` callbacks. Clones share
/// state.
#[derive(Clone, Default)]
pub struct MemoryExecutionEnv {
    state: Arc<Mutex<MemState>>,
}

impl MemoryExecutionEnv {
    /// An empty environment with the given working directory.
    pub fn new(cwd: impl Into<String>) -> Self {
        let env = Self::default();
        env.state.lock().unwrap().cwd = cwd.into();
        env
    }

    /// Seed a file's contents.
    pub fn with_file(self, path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.into(), contents.into());
        self
    }

    /// Seed a directory (kind `directory`).
    pub fn with_dir(self, path: impl Into<String>) -> Self {
        self.state.lock().unwrap().dirs.insert(path.into());
        self
    }

    /// Seed a symlink from `path` to `target`. `file_info(path)` reports it as a
    /// symlink; `read_text_file`/`list_dir`/`canonical_path` follow it.
    pub fn with_symlink(self, path: impl Into<String>, target: impl Into<String>) -> Self {
        self.state
            .lock()
            .unwrap()
            .symlinks
            .insert(path.into(), target.into());
        self
    }

    /// Queue a scripted successful `exec`: the given stdout chunks are streamed
    /// in order to `on_stdout`, then the given exit code is returned.
    pub fn push_exec_output(&self, stdout: Vec<String>, stderr: Vec<String>, exit_code: i32) {
        self.state
            .lock()
            .unwrap()
            .exec_queue
            .push_back(ExecOutcome::Output {
                stdout,
                stderr,
                exit_code,
            });
    }

    /// Queue a scripted `exec` failure.
    pub fn push_exec_failure(&self, error: ExecutionError) {
        self.state
            .lock()
            .unwrap()
            .exec_queue
            .push_back(ExecOutcome::Failure(error));
    }

    fn basename(path: &str) -> String {
        path.rsplit('/').next().unwrap_or(path).to_string()
    }

    /// Resolve a path component-by-component, following any symlink encountered
    /// on an intermediate component. The final component's own symlink is
    /// followed only when `follow_last` is set (stat vs lstat semantics).
    ///
    /// Symlink targets are taken as-is when absolute; a relative target is
    /// resolved against the symlink's parent directory. This mirrors how a real
    /// filesystem walks a path through symlinked directories.
    fn resolve(state: &MemState, path: &str, follow_last: bool) -> String {
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        let mut current = String::new();
        for (index, part) in parts.iter().enumerate() {
            current.push('/');
            current.push_str(part);
            let is_last = index + 1 == parts.len();
            if is_last && !follow_last {
                continue;
            }
            // Follow a chain of symlinks at the current component.
            let mut guard = 0;
            while let Some(target) = state.symlinks.get(&current) {
                current = if target.starts_with('/') {
                    target.clone()
                } else {
                    let parent = current.rsplit_once('/').map_or("", |(head, _)| head);
                    format!("{parent}/{target}")
                };
                guard += 1;
                if guard > 64 {
                    break;
                }
            }
        }
        current
    }

    /// Classify a fully-resolved key as a [`FileKind`], if present. Symlinks are
    /// checked first so an unfollowed final symlink reports as `symlink`.
    fn classify(state: &MemState, key: &str) -> Option<FileKind> {
        if state.symlinks.contains_key(key) {
            Some(FileKind::Symlink)
        } else if state.files.contains_key(key) {
            Some(FileKind::File)
        } else if state.dirs.contains(key) {
            Some(FileKind::Directory)
        } else {
            None
        }
    }
}

impl FileSystem for MemoryExecutionEnv {
    fn cwd(&self) -> String {
        self.state.lock().unwrap().cwd.clone()
    }

    fn absolute_path(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> Result<String, FileError> {
        if path.starts_with('/') {
            Ok(path.to_string())
        } else {
            let cwd = self.state.lock().unwrap().cwd.clone();
            Ok(format!("{}/{path}", cwd.trim_end_matches('/')))
        }
    }

    fn join_path(
        &self,
        parts: &[&str],
        _signal: Option<&AbortSignal>,
    ) -> Result<String, FileError> {
        Ok(parts.join("/"))
    }

    fn read_text_file(
        &self,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<String, FileError> {
        if let Some(error) = aborted_file_error(signal, path) {
            return Err(error);
        }
        let state = self.state.lock().unwrap();
        let resolved = Self::resolve(&state, path, true);
        match state.files.get(&resolved) {
            Some(bytes) => String::from_utf8(bytes.clone())
                .map_err(|_| FileError::with_path(FileErrorCode::Invalid, "invalid UTF-8", path)),
            None => Err(FileError::with_path(
                FileErrorCode::NotFound,
                format!("no such file: {path}"),
                path,
            )),
        }
    }

    fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<String>, FileError> {
        if let Some(error) = aborted_file_error(signal, path) {
            return Err(error);
        }
        let text = self.read_text_file(path, signal)?;
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if let Some(max) = max_lines {
            lines.truncate(max);
        }
        Ok(lines)
    }

    fn read_binary_file(
        &self,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<u8>, FileError> {
        if let Some(error) = aborted_file_error(signal, path) {
            return Err(error);
        }
        let state = self.state.lock().unwrap();
        let resolved = Self::resolve(&state, path, true);
        state.files.get(&resolved).cloned().ok_or_else(|| {
            FileError::with_path(
                FileErrorCode::NotFound,
                format!("no such file: {path}"),
                path,
            )
        })
    }

    fn write_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        signal: Option<&AbortSignal>,
    ) -> Result<(), FileError> {
        if let Some(error) = aborted_file_error(signal, path) {
            return Err(error);
        }
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.to_string(), content.as_bytes().to_vec());
        Ok(())
    }

    fn append_file(
        &self,
        path: &str,
        content: FileContent<'_>,
        _signal: Option<&AbortSignal>,
    ) -> Result<(), FileError> {
        self.state
            .lock()
            .unwrap()
            .files
            .entry(path.to_string())
            .or_default()
            .extend_from_slice(content.as_bytes());
        Ok(())
    }

    fn file_info(&self, path: &str, _signal: Option<&AbortSignal>) -> Result<FileInfo, FileError> {
        let state = self.state.lock().unwrap();
        // lstat semantics: intermediate symlinks are followed, the final
        // component is not, so a symlink reports its own `symlink` kind.
        let resolved = Self::resolve(&state, path, false);
        match Self::classify(&state, &resolved) {
            Some(kind) => Ok(FileInfo {
                name: Self::basename(path),
                path: path.to_string(),
                kind,
                size: if kind == FileKind::File {
                    state.files.get(&resolved).map_or(0, Vec::len) as u64
                } else {
                    0
                },
                mtime_ms: 0,
            }),
            None => Err(FileError::with_path(
                FileErrorCode::NotFound,
                format!("no such file: {path}"),
                path,
            )),
        }
    }

    fn list_dir(
        &self,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<FileInfo>, FileError> {
        if let Some(error) = aborted_file_error(signal, path) {
            return Err(error);
        }
        let state = self.state.lock().unwrap();
        // Listing follows the addressed path to its real directory, but reports
        // each child under the *queried* path (as a real readdir does through a
        // symlinked directory).
        let real = Self::resolve(&state, path, true);
        let query = path.trim_end_matches('/');
        let real_prefix = format!("{}/", real.trim_end_matches('/'));
        let mut seen = BTreeSet::new();
        let mut infos = Vec::new();
        // File, directory, and symlink keys share the flat namespace; gather the
        // direct children of `real` from all three maps.
        let keys = state
            .files
            .keys()
            .chain(state.dirs.iter())
            .chain(state.symlinks.keys());
        for key in keys {
            let Some(rest) = key.strip_prefix(&real_prefix) else {
                continue;
            };
            if rest.is_empty() || rest.contains('/') {
                continue;
            }
            if !seen.insert(rest.to_string()) {
                continue;
            }
            let child_real = format!("{real_prefix}{rest}");
            let Some(kind) = Self::classify(&state, &child_real) else {
                continue;
            };
            infos.push(FileInfo {
                name: rest.to_string(),
                path: format!("{query}/{rest}"),
                kind,
                size: if kind == FileKind::File {
                    state.files.get(&child_real).map_or(0, Vec::len) as u64
                } else {
                    0
                },
                mtime_ms: 0,
            });
        }
        Ok(infos)
    }

    fn canonical_path(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> Result<String, FileError> {
        let state = self.state.lock().unwrap();
        Ok(Self::resolve(&state, path, true))
    }

    fn exists(&self, path: &str, _signal: Option<&AbortSignal>) -> Result<bool, FileError> {
        let state = self.state.lock().unwrap();
        let resolved = Self::resolve(&state, path, false);
        Ok(Self::classify(&state, &resolved).is_some())
    }

    fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        _signal: Option<&AbortSignal>,
    ) -> Result<(), FileError> {
        let mut state = self.state.lock().unwrap();
        if recursive {
            // Register every ancestor so the whole chain is stat-able.
            let mut prefix = String::new();
            for part in path.split('/').filter(|part| !part.is_empty()) {
                prefix.push('/');
                prefix.push_str(part);
                state.dirs.insert(prefix.clone());
            }
        } else {
            state.dirs.insert(path.trim_end_matches('/').to_string());
        }
        Ok(())
    }

    fn remove(
        &self,
        path: &str,
        recursive: bool,
        force: bool,
        _signal: Option<&AbortSignal>,
    ) -> Result<(), FileError> {
        let mut state = self.state.lock().unwrap();
        if recursive {
            let prefix = format!("{}/", path.trim_end_matches('/'));
            state
                .files
                .retain(|file_path, _| file_path != path && !file_path.starts_with(&prefix));
            Ok(())
        } else if state.files.remove(path).is_some() || force {
            Ok(())
        } else {
            Err(FileError::with_path(
                FileErrorCode::NotFound,
                format!("no such file: {path}"),
                path,
            ))
        }
    }

    fn create_temp_dir(
        &self,
        prefix: &str,
        _signal: Option<&AbortSignal>,
    ) -> Result<String, FileError> {
        let mut state = self.state.lock().unwrap();
        state.temp_counter += 1;
        Ok(format!("/tmp/{prefix}{}", state.temp_counter))
    }

    fn create_temp_file(
        &self,
        prefix: &str,
        suffix: &str,
        _signal: Option<&AbortSignal>,
    ) -> Result<String, FileError> {
        let mut state = self.state.lock().unwrap();
        state.temp_counter += 1;
        let path = format!("/tmp/{prefix}{}{suffix}", state.temp_counter);
        state.files.insert(path.clone(), Vec::new());
        Ok(path)
    }

    fn cleanup(&self) {}
}

impl Shell for MemoryExecutionEnv {
    fn exec(
        &self,
        _command: &str,
        mut options: ShellExecOptions<'_>,
    ) -> Result<ShellExecOutput, ExecutionError> {
        // pi's `exec` entry guard (`options?.abortSignal?.aborted`).
        if let Some(error) = aborted_execution_error(options.abort_signal) {
            return Err(error);
        }
        // Pop the scripted outcome under the lock, then release it before
        // invoking callbacks (they may re-enter through createTempFile/appendFile).
        let outcome = {
            let mut state = self.state.lock().unwrap();
            state.exec_queue.pop_front()
        };
        match outcome {
            Some(ExecOutcome::Failure(error)) => Err(error),
            Some(ExecOutcome::Output {
                stdout,
                stderr,
                exit_code,
            }) => {
                for chunk in &stdout {
                    if let Some(callback) = options.on_stdout.as_mut() {
                        callback(chunk);
                    }
                }
                for chunk in &stderr {
                    if let Some(callback) = options.on_stderr.as_mut() {
                        callback(chunk);
                    }
                }
                Ok(ShellExecOutput {
                    stdout: stdout.concat(),
                    stderr: stderr.concat(),
                    exit_code,
                })
            }
            None => Ok(ShellExecOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            }),
        }
    }

    fn cleanup(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_helpers_mirror_pi() {
        let good: Result<i32, String> = ok(3);
        let bad: Result<i32, String> = err("boom".to_string());
        assert_eq!(good, Ok(3));
        assert_eq!(bad, Err("boom".to_string()));
        assert_eq!(get_or_throw(ok::<_, String>(7)), 7);
        assert_eq!(get_or_undefined(ok::<_, String>(9)), Some(9));
        assert_eq!(get_or_undefined::<i32, _>(err("x".to_string())), None);
        assert_eq!(
            to_error(FileError::new(FileErrorCode::NotFound, "missing")),
            "missing"
        );
    }

    #[test]
    #[should_panic(expected = "get_or_throw on Err")]
    fn get_or_throw_panics_on_err() {
        get_or_throw::<i32, _>(err("nope".to_string()));
    }

    #[test]
    fn error_codes_serialize_to_pi_wire_strings() {
        assert_eq!(
            FileErrorCode::PermissionDenied.as_str(),
            "permission_denied"
        );
        assert_eq!(FileErrorCode::IsDirectory.as_str(), "is_directory");
        assert_eq!(
            ExecutionErrorCode::ShellUnavailable.as_str(),
            "shell_unavailable"
        );
        assert_eq!(ExecutionErrorCode::SpawnError.as_str(), "spawn_error");
        assert_eq!(FileKind::Directory.as_str(), "directory");
    }

    #[test]
    fn memory_env_round_trips_files() {
        let env = MemoryExecutionEnv::new("/work");
        assert_eq!(env.cwd(), "/work");
        assert_eq!(env.absolute_path("rel", None).unwrap(), "/work/rel");
        assert_eq!(env.absolute_path("/abs", None).unwrap(), "/abs");

        assert!(!env.exists("/work/a.txt", None).unwrap());
        env.write_file("/work/a.txt", FileContent::Text("l1\nl2\n"), None)
            .unwrap();
        assert!(env.exists("/work/a.txt", None).unwrap());
        assert_eq!(env.read_text_file("/work/a.txt", None).unwrap(), "l1\nl2\n");
        assert_eq!(
            env.read_text_lines("/work/a.txt", None, None).unwrap(),
            vec!["l1", "l2"]
        );
        assert_eq!(
            env.read_text_lines("/work/a.txt", Some(1), None).unwrap(),
            vec!["l1"]
        );

        env.append_file("/work/a.txt", FileContent::Text("l3"), None)
            .unwrap();
        assert_eq!(
            env.read_text_file("/work/a.txt", None).unwrap(),
            "l1\nl2\nl3"
        );
    }

    #[test]
    fn memory_env_missing_file_is_not_found() {
        let env = MemoryExecutionEnv::new("/work");
        let error = env.read_text_file("/nope", None).unwrap_err();
        assert_eq!(error.code, FileErrorCode::NotFound);
        assert_eq!(error.path.as_deref(), Some("/nope"));
    }

    #[test]
    fn memory_env_exec_streams_scripted_chunks() {
        let env = MemoryExecutionEnv::new("/work");
        env.push_exec_output(vec!["a".into(), "b".into()], vec!["e".into()], 0);
        let mut out = String::new();
        let mut errs = String::new();
        let options = ShellExecOptions {
            on_stdout: Some(Box::new(|chunk: &str| out.push_str(chunk))),
            on_stderr: Some(Box::new(|chunk: &str| errs.push_str(chunk))),
            ..Default::default()
        };
        let result = env.exec("cmd", options).unwrap();
        assert_eq!(result.stdout, "ab");
        assert_eq!(result.stderr, "e");
        assert_eq!(result.exit_code, 0);
        assert_eq!(out, "ab");
        assert_eq!(errs, "e");
    }
}
