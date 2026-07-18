//! The real, host-backed execution environment, mirroring
//! `packages/agent/src/harness/env/nodejs.ts`.
//!
//! [`NodeExecutionEnv`] is the production implementation of the harness
//! [`FileSystem`]/[`Shell`]/[`ExecutionEnv`] contract, backed by [`std::fs`],
//! [`std::path`], and real subprocesses via [`std::process::Command`]. Where
//! pi's version leans on `node:fs/promises`, `node:child_process`, `node:os`,
//! `node:path`, `node:crypto`, and `node:readline`, this port uses the standard
//! library directly.
//!
//! # Faithful divergences from pi
//!
//! - **Synchronous.** Like the rest of the harness port, every method is
//!   eager and returns `Result<T, E>` directly — no async/tokio. Shell output
//!   is streamed to the `on_stdout`/`on_stderr` callbacks concurrently using
//!   reader [`std::thread`]s that pipe raw chunks back to the calling thread,
//!   which invokes the (non-`Send`, borrowing) callbacks in order.
//! - **No `AbortSignal`.** The contract drops pi's cooperative-cancellation
//!   parameter, so the `aborted`-on-signal branches of `exec`/the cancellable
//!   file operations are unreachable. The [`ExecutionErrorCode::Aborted`] and
//!   [`FileErrorCode::Aborted`] codes remain part of the enum surface.
//! - **No callback errors.** pi's stream callbacks may `throw`, producing a
//!   `callback_error`. The Rust callback type is `FnMut(&str)` with no error
//!   channel, so that branch cannot occur; the [`ExecutionErrorCode::CallbackError`]
//!   code is retained for parity.
//! - **Process-tree kill.** pi kills the whole process group (`kill(-pid)` /
//!   `taskkill /T`). To avoid a `libc` dependency this port kills the direct
//!   child via [`std::process::Child::kill`], which is sufficient for the shells
//!   used here (they `exec` a single external command in the `-c` form).

// straitjacket-allow-file[:duplication] — the file operations share one faithful
// resolve-path / call-`std::fs` / map-to-`FileError` shape mirroring pi's parallel
// `node:fs/promises` wrappers; the near-identical io-error-mapping arms are kept
// distinct rather than collapsed to preserve that one-to-one correspondence.

use std::collections::BTreeMap;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{
    ExecutionError, ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileKind,
    FileSystem, Shell, ShellExecOptions, ShellExecOutput,
};

/// Largest timeout accepted, mirroring pi's `MAX_TIMEOUT_MS` (`2**31 - 1`).
const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
/// Same ceiling in seconds, used for the error message. Mirrors
/// `MAX_TIMEOUT_SECONDS`.
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;

// ---------------------------------------------------------------------------
// Path helpers (`node:path` resolve/join/isAbsolute)
// ---------------------------------------------------------------------------

/// Whether `path` is absolute in the POSIX namespace. Mirrors `isAbsolute` for
/// the platforms this port targets.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// Normalize a POSIX path, collapsing `.`/`..`/redundant separators and
/// dropping any trailing slash (except the root). Mirrors `node:path`'s
/// normalization used by `resolve`/`join`.
fn normalize(input: &str) -> String {
    let is_abs = is_absolute(input);
    let mut out: Vec<&str> = Vec::new();
    for seg in input.split('/') {
        match seg {
            "" | "." => {}
            ".." => match out.last() {
                Some(&last) if last != ".." => {
                    out.pop();
                }
                Some(_) => out.push(".."),
                None => {
                    if !is_abs {
                        out.push("..");
                    }
                }
            },
            seg => out.push(seg),
        }
    }
    let joined = out.join("/");
    if is_abs {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Resolve `path` against `cwd`, mirroring `resolvePath(cwd, path)` (`resolve`
/// when relative, the path itself when absolute).
fn resolve_path(cwd: &str, path: &str) -> String {
    if is_absolute(path) {
        normalize(path)
    } else {
        normalize(&format!("{cwd}/{path}"))
    }
}

/// Join path segments in the filesystem namespace. Mirrors `node:path`'s
/// `join(...parts)`.
fn join_parts(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        ".".to_string()
    } else {
        normalize(&joined)
    }
}

/// Basename of `path` after stripping trailing slashes. Mirrors pi's
/// `path.replace(/\/+$/, "").split("/").pop() ?? path`.
fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return path.to_string();
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

// ---------------------------------------------------------------------------
// Error mapping (`toFileError`)
// ---------------------------------------------------------------------------

/// Map an [`io::Error`] to a [`FileError`], mirroring pi's `toFileError` errno
/// switch. The POSIX errno numbers are matched first (the faithful equivalent
/// of pi's `error.code` switch), falling back to [`io::ErrorKind`].
fn to_file_error(error: &io::Error, path: Option<&str>) -> FileError {
    let code = match error.raw_os_error() {
        Some(2) => FileErrorCode::NotFound,                    // ENOENT
        Some(13) | Some(1) => FileErrorCode::PermissionDenied, // EACCES / EPERM
        Some(20) => FileErrorCode::NotDirectory,               // ENOTDIR
        Some(21) => FileErrorCode::IsDirectory,                // EISDIR
        Some(22) => FileErrorCode::Invalid,                    // EINVAL
        _ => match error.kind() {
            io::ErrorKind::NotFound => FileErrorCode::NotFound,
            io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
            _ => FileErrorCode::Unknown,
        },
    };
    match path {
        Some(path) => FileError::with_path(code, error.to_string(), path),
        None => FileError::new(code, error.to_string()),
    }
}

/// Whether an [`io::Error`] means the addressed path does not exist.
fn is_not_found(error: &io::Error) -> bool {
    error.raw_os_error() == Some(2) || error.kind() == io::ErrorKind::NotFound
}

// ---------------------------------------------------------------------------
// FileInfo helpers (`fileKindFromStats` / `fileInfoFromStats`)
// ---------------------------------------------------------------------------

/// Millisecond mtime for `metadata`. Mirrors `stats.mtimeMs`.
fn mtime_ms(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |dur| dur.as_millis() as i64)
}

/// Classify `metadata` (from an `lstat`-style call) into a [`FileKind`].
/// Mirrors `fileKindFromStats`.
fn file_kind_from_stats(metadata: &Metadata) -> Option<FileKind> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Some(FileKind::File)
    } else if file_type.is_dir() {
        Some(FileKind::Directory)
    } else if file_type.is_symlink() {
        Some(FileKind::Symlink)
    } else {
        None
    }
}

/// Build a [`FileInfo`] from `lstat`-style metadata. Mirrors
/// `fileInfoFromStats`; an unsupported kind yields an `invalid` error.
fn file_info_from_stats(path: &str, metadata: &Metadata) -> Result<FileInfo, FileError> {
    let Some(kind) = file_kind_from_stats(metadata) else {
        return Err(FileError::with_path(
            FileErrorCode::Invalid,
            "Unsupported file type",
            path,
        ));
    };
    Ok(FileInfo {
        name: basename(path),
        path: path.to_string(),
        kind,
        size: metadata.len(),
        mtime_ms: mtime_ms(metadata),
    })
}

// ---------------------------------------------------------------------------
// Shell configuration (`getShellConfig` / `getBashShellConfig`)
// ---------------------------------------------------------------------------

/// A resolved shell invocation. Mirrors pi's `ShellConfig`.
struct ShellConfig {
    shell: String,
    args: Vec<String>,
    /// When set, the command is written to the shell's stdin (legacy WSL bash).
    stdin_transport: bool,
}

/// Whether `path` is a legacy WSL `System32`/`Sysnative` `bash.exe`. Mirrors
/// `isLegacyWslBashPath`.
fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    let bytes = normalized.as_bytes();
    // ^[a-z]:\windows\(system32|sysnative)\bash.exe$
    if bytes.len() < 3 || !bytes[0].is_ascii_lowercase() || bytes[1] != b':' {
        return false;
    }
    let rest = &normalized[2..];
    rest == "\\windows\\system32\\bash.exe" || rest == "\\windows\\sysnative\\bash.exe"
}

/// Build a [`ShellConfig`] for a bash `shell` path. Mirrors
/// `getBashShellConfig`.
fn get_bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-s".to_string()],
            stdin_transport: true,
        }
    } else {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-c".to_string()],
            stdin_transport: false,
        }
    }
}

/// Whether `path` exists (following symlinks). Mirrors `pathExists`
/// (`access(path, F_OK)`).
fn path_exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Run `command`/`args`, capturing stdout with a best-effort timeout. Mirrors
/// pi's `runCommand`, used only by `findBashOnPath`.
fn run_command(command: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take();
    let reader = thread::spawn(move || {
        let mut buffer = String::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_string(&mut buffer);
        }
        buffer
    });
    let status = wait_with_timeout(&mut child, Some(timeout));
    let output = reader.join().unwrap_or_default();
    match status {
        Some(status) if status.success() => Some(output),
        _ => None,
    }
}

/// Locate `bash` on `PATH`. Mirrors `findBashOnPath` (POSIX branch).
fn find_bash_on_path() -> Option<String> {
    let output = run_command("which", &["bash"], Duration::from_millis(5000))?;
    let first = output.lines().next()?.trim();
    if !first.is_empty() && path_exists(first) {
        Some(first.to_string())
    } else {
        None
    }
}

/// Resolve the shell configuration. Mirrors `getShellConfig` (POSIX branch);
/// a custom path is honored on any platform.
fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, ExecutionError> {
    if let Some(custom) = custom_shell_path {
        if path_exists(custom) {
            return Ok(get_bash_shell_config(custom));
        }
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!("Custom shell path not found: {custom}"),
        ));
    }
    if path_exists("/bin/bash") {
        return Ok(get_bash_shell_config("/bin/bash"));
    }
    if let Some(bash) = find_bash_on_path() {
        return Ok(get_bash_shell_config(&bash));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        stdin_transport: false,
    })
}

/// Validate and convert `timeout` (seconds) to milliseconds. Mirrors
/// `resolveTimeoutMs`.
fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<f64>, ExecutionError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
        ));
    }
    Ok(Some(timeout_ms))
}

/// Kill the direct child. Mirrors `killProcessTree` (simplified to the child).
fn kill_process_tree(child: &mut Child) {
    let _ = child.kill();
}

/// Block until `child` exits or `timeout` elapses, killing it on timeout.
/// Returns the exit status when the process exited on its own.
fn wait_with_timeout(
    child: &mut Child,
    timeout: Option<Duration>,
) -> Option<std::process::ExitStatus> {
    let deadline = timeout.map(|dur| Instant::now() + dur);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// A random 6-character base-36 token for temp-name generation, standing in for
/// `mkdtemp`'s random suffix / `randomUUID`.
fn random_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |dur| dur.as_nanos() as u64);
    let mut state = nanos ^ counter.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x1234_5678_9abc_def0;
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut token = String::with_capacity(6);
    for _ in 0..6 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        token.push(ALPHABET[(state % 36) as usize] as char);
    }
    token
}

/// The system temporary directory as a string. Mirrors `tmpdir()`.
fn tmpdir() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// NodeExecutionEnv
// ---------------------------------------------------------------------------

/// The host-backed [`ExecutionEnv`]. Mirrors pi's `NodeExecutionEnv`.
pub struct NodeExecutionEnv {
    /// Current working directory for relative paths. Public, mirroring pi.
    pub cwd: String,
    shell_path: Option<String>,
    shell_env: Option<BTreeMap<String, String>>,
}

impl NodeExecutionEnv {
    /// An environment rooted at `cwd`, using the default shell resolution.
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            shell_path: None,
            shell_env: None,
        }
    }

    /// Use a custom shell path (mirrors pi's `shellPath` option).
    pub fn with_shell_path(mut self, shell_path: impl Into<String>) -> Self {
        self.shell_path = Some(shell_path.into());
        self
    }

    /// Layer additional base environment variables on top of the inherited env
    /// (mirrors pi's `shellEnv` option).
    pub fn with_shell_env(mut self, shell_env: BTreeMap<String, String>) -> Self {
        self.shell_env = Some(shell_env);
        self
    }

    fn resolved(&self, path: &str) -> String {
        resolve_path(&self.cwd, path)
    }
}

/// A tagged output chunk streamed from a reader thread to the calling thread.
enum Chunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

/// Spawn a reader thread that streams raw chunks from `pipe` to `sender`.
fn spawn_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    sender: mpsc::Sender<Chunk>,
    tag: fn(Vec<u8>) -> Chunk,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else { return };
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if sender.send(tag(buffer[..read].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

impl FileSystem for NodeExecutionEnv {
    fn cwd(&self) -> String {
        self.cwd.clone()
    }

    fn absolute_path(&self, path: &str) -> Result<String, FileError> {
        Ok(self.resolved(path))
    }

    fn join_path(&self, parts: &[&str]) -> Result<String, FileError> {
        Ok(join_parts(parts))
    }

    fn read_text_file(&self, path: &str) -> Result<String, FileError> {
        let resolved = self.resolved(path);
        match fs::read(&resolved) {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) => Err(to_file_error(&error, Some(&resolved))),
        }
    }

    fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
    ) -> Result<Vec<String>, FileError> {
        let resolved = self.resolved(path);
        if max_lines == Some(0) {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&resolved).map_err(|error| to_file_error(&error, Some(&resolved)))?;
        let text = String::from_utf8_lossy(&bytes);
        let mut segments: Vec<&str> = text.split('\n').collect();
        // A trailing newline does not yield a final empty line (readline
        // semantics); other empty lines are preserved.
        if segments.last().is_some_and(|last| last.is_empty()) {
            segments.pop();
        }
        let mut lines = Vec::new();
        for segment in segments {
            lines.push(segment.strip_suffix('\r').unwrap_or(segment).to_string());
            if max_lines.is_some_and(|max| lines.len() >= max) {
                break;
            }
        }
        Ok(lines)
    }

    fn read_binary_file(&self, path: &str) -> Result<Vec<u8>, FileError> {
        let resolved = self.resolved(path);
        fs::read(&resolved).map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    fn write_file(&self, path: &str, content: FileContent<'_>) -> Result<(), FileError> {
        let resolved = self.resolved(path);
        if let Some(parent) = Path::new(&resolved).parent() {
            fs::create_dir_all(parent).map_err(|error| to_file_error(&error, Some(&resolved)))?;
        }
        fs::write(&resolved, content.as_bytes())
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    fn append_file(&self, path: &str, content: FileContent<'_>) -> Result<(), FileError> {
        let resolved = self.resolved(path);
        if let Some(parent) = Path::new(&resolved).parent() {
            fs::create_dir_all(parent).map_err(|error| to_file_error(&error, Some(&resolved)))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&resolved)
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        file.write_all(content.as_bytes())
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    fn file_info(&self, path: &str) -> Result<FileInfo, FileError> {
        let resolved = self.resolved(path);
        let metadata = fs::symlink_metadata(&resolved)
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        file_info_from_stats(&resolved, &metadata)
    }

    fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>, FileError> {
        let resolved = self.resolved(path);
        let entries =
            fs::read_dir(&resolved).map_err(|error| to_file_error(&error, Some(&resolved)))?;
        let mut infos = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| to_file_error(&error, Some(&resolved)))?;
            let entry_path = resolve_path(&resolved, &entry.file_name().to_string_lossy());
            let metadata = fs::symlink_metadata(&entry_path)
                .map_err(|error| to_file_error(&error, Some(&entry_path)))?;
            if let Ok(info) = file_info_from_stats(&entry_path, &metadata) {
                infos.push(info);
            }
        }
        Ok(infos)
    }

    fn canonical_path(&self, path: &str) -> Result<String, FileError> {
        let resolved = self.resolved(path);
        fs::canonicalize(&resolved)
            .map(|canonical| canonical.to_string_lossy().into_owned())
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    fn exists(&self, path: &str) -> Result<bool, FileError> {
        match self.file_info(path) {
            Ok(_) => Ok(true),
            Err(error) if error.code == FileErrorCode::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn create_dir(&self, path: &str, recursive: bool) -> Result<(), FileError> {
        let resolved = self.resolved(path);
        let result = if recursive {
            fs::create_dir_all(&resolved)
        } else {
            fs::create_dir(&resolved)
        };
        result.map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    fn remove(&self, path: &str, recursive: bool, force: bool) -> Result<(), FileError> {
        let resolved = self.resolved(path);
        let metadata = match fs::symlink_metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(error) if force && is_not_found(&error) => return Ok(()),
            Err(error) => return Err(to_file_error(&error, Some(&resolved))),
        };
        let result = if metadata.is_dir() {
            if recursive {
                fs::remove_dir_all(&resolved)
            } else {
                fs::remove_dir(&resolved)
            }
        } else {
            fs::remove_file(&resolved)
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) if force && is_not_found(&error) => Ok(()),
            Err(error) => Err(to_file_error(&error, Some(&resolved))),
        }
    }

    fn create_temp_dir(&self, prefix: &str) -> Result<String, FileError> {
        let base = tmpdir();
        let base = base.trim_end_matches('/');
        for _ in 0..1000 {
            let candidate = format!("{base}/{prefix}{}", random_token());
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(to_file_error(&error, Some(&candidate))),
            }
        }
        Err(FileError::new(
            FileErrorCode::Unknown,
            "failed to create a unique temporary directory",
        ))
    }

    fn create_temp_file(&self, prefix: &str, suffix: &str) -> Result<String, FileError> {
        let dir = self.create_temp_dir("tmp-")?;
        let file_path = format!(
            "{}/{prefix}{}{suffix}",
            dir.trim_end_matches('/'),
            random_token()
        );
        fs::write(&file_path, b"").map_err(|error| to_file_error(&error, Some(&file_path)))?;
        Ok(file_path)
    }

    fn cleanup(&self) {}
}

impl Shell for NodeExecutionEnv {
    fn exec(
        &self,
        command: &str,
        mut options: ShellExecOptions<'_>,
    ) -> Result<ShellExecOutput, ExecutionError> {
        let timeout_ms = resolve_timeout_ms(options.timeout)?;
        let cwd = match &options.cwd {
            Some(cwd) => resolve_path(&self.cwd, cwd),
            None => self.cwd.clone(),
        };
        let shell_config = get_shell_config(self.shell_path.as_deref())?;

        let mut cmd = Command::new(&shell_config.shell);
        if shell_config.stdin_transport {
            cmd.args(&shell_config.args);
        } else {
            cmd.args(&shell_config.args).arg(command);
        }
        cmd.current_dir(&cwd);
        if let Some(shell_env) = &self.shell_env {
            for (key, value) in shell_env {
                cmd.env(key, value);
            }
        }
        if let Some(env) = &options.env {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }
        cmd.stdin(if shell_config.stdin_transport {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::SpawnError,
                    error.to_string(),
                ));
            }
        };

        if shell_config.stdin_transport {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(command.as_bytes());
                // Dropping `stdin` closes the pipe so the shell sees EOF.
            }
        }

        let (sender, receiver) = mpsc::channel::<Chunk>();
        let stdout_handle = spawn_reader(child.stdout.take(), sender.clone(), Chunk::Stdout);
        let stderr_handle = spawn_reader(child.stderr.take(), sender, Chunk::Stderr);

        let mut stdout_bytes: Vec<u8> = Vec::new();
        let mut stderr_bytes: Vec<u8> = Vec::new();
        let mut timed_out = false;
        let mut deadline =
            timeout_ms.map(|ms| Instant::now() + Duration::from_secs_f64(ms / 1000.0));

        loop {
            let message = match deadline {
                Some(when) => {
                    let remaining = when.saturating_duration_since(Instant::now());
                    match receiver.recv_timeout(remaining) {
                        Ok(message) => message,
                        Err(RecvTimeoutError::Timeout) => {
                            timed_out = true;
                            kill_process_tree(&mut child);
                            deadline = None;
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match receiver.recv() {
                    Ok(message) => message,
                    Err(_) => break,
                },
            };
            match message {
                Chunk::Stdout(bytes) => {
                    if let Some(callback) = options.on_stdout.as_mut() {
                        callback(&String::from_utf8_lossy(&bytes));
                    }
                    stdout_bytes.extend_from_slice(&bytes);
                }
                Chunk::Stderr(bytes) => {
                    if let Some(callback) = options.on_stderr.as_mut() {
                        callback(&String::from_utf8_lossy(&bytes));
                    }
                    stderr_bytes.extend_from_slice(&bytes);
                }
            }
        }

        let _ = stdout_handle.join();
        let _ = stderr_handle.join();
        let status = child.wait();

        if timed_out {
            let seconds = options.timeout.unwrap_or(0.0);
            return Err(ExecutionError::new(
                ExecutionErrorCode::Timeout,
                format!("timeout:{seconds}"),
            ));
        }

        let exit_code = match status {
            Ok(status) => status.code().unwrap_or(0),
            Err(error) => {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::SpawnError,
                    error.to_string(),
                ));
            }
        };

        Ok(ShellExecOutput {
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
            exit_code,
        })
    }

    fn cleanup(&self) {}
}

#[cfg(test)]
#[path = "nodejs_tests.rs"]
mod tests;
