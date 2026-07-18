//! Shell discovery, environment assembly, output sanitization, and
//! process-tree termination.
//!
//! Ported from pi's `utils/shell.ts`. Unix is the priority path (and what the
//! tests exercise); the Windows branches mirror pi's Git-Bash / `taskkill`
//! best-effort resolution behind `cfg(windows)` and are not compiled or tested
//! here.
//!
//! Notes on the port:
//! - [`sanitize_binary_output`] is pure and byte-exact vs. pi's code-point
//!   stripping. Rust `&str` cannot hold lone surrogates, so the "lone
//!   surrogate" case pi guards against cannot arise here.
//! - [`get_shell_env`] needs pi's `getBinDir()` (from `config.ts`, not yet
//!   ported). It is replicated minimally in [`get_bin_dir`] — see that fn.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// How a resolved shell wants the command delivered.
///
/// Mirrors pi's `commandTransport?: "argv" | "stdin"`. Legacy WSL `bash.exe`
/// cannot accept `-c <command>` reliably and must be fed the script over stdin
/// (`bash -s`); everything else takes the command as an argv element (`-c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTransport {
    /// Pass the command as an argv element (`bash -c <cmd>`).
    Argv,
    /// Pipe the command to the shell's stdin (`bash -s`).
    Stdin,
}

/// Resolved shell configuration (pi's `ShellConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    /// Absolute path (or PATH-resolvable name) of the shell binary.
    pub shell: String,
    /// Launch arguments (`["-c"]` or `["-s"]`).
    pub args: Vec<String>,
    /// Optional command-delivery transport (set only for legacy WSL bash).
    pub command_transport: Option<CommandTransport>,
}

impl ShellConfig {
    /// Whether the command must be piped over stdin rather than passed as argv.
    pub fn use_stdin_transport(&self) -> bool {
        self.command_transport == Some(CommandTransport::Stdin)
    }
}

/// Detects legacy WSL `bash.exe` paths (`C:\Windows\System32\bash.exe`,
/// `...\Sysnative\bash.exe`), which require the stdin transport.
fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    // `^[a-z]:\windows\(system32|sysnative)\bash.exe$`
    let bytes = normalized.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_lowercase() || &normalized[1..3] != ":\\" {
        return false;
    }
    let rest = &normalized[3..];
    rest == "windows\\system32\\bash.exe" || rest == "windows\\sysnative\\bash.exe"
}

fn bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-s".to_string()],
            command_transport: Some(CommandTransport::Stdin),
        }
    } else {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-c".to_string()],
            command_transport: None,
        }
    }
}

/// The first line of a command's stdout, trimmed, if the command succeeded and
/// that line is non-empty. Shared by [`find_bash_on_path`]'s platform branches.
fn first_nonempty_line(output: &std::process::Output) -> Option<String> {
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next()?.trim();
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

/// Find `bash` on PATH via `which` (Unix) / `where` (Windows).
///
/// On Unix we trust `which`'s output (handles Termux and special filesystems),
/// matching pi. On Windows we additionally verify the path exists because
/// `where` can report stale entries.
fn find_bash_on_path() -> Option<String> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("where")
            .arg("bash.exe")
            .output()
            .ok()?;
        let first = first_nonempty_line(&output)?;
        if Path::new(&first).exists() {
            return Some(first);
        }
        None
    }
    #[cfg(not(windows))]
    {
        let output = std::process::Command::new("which")
            .arg("bash")
            .output()
            .ok()?;
        first_nonempty_line(&output)
    }
}

/// Resolve shell configuration based on platform and an optional explicit path.
///
/// Resolution order (pi parity):
/// 1. User-specified `custom_shell_path` (error if it does not exist).
/// 2. Windows: Git Bash in known locations, then `bash.exe` on PATH.
/// 3. Unix: `/bin/bash`, then `bash` on PATH, then fall back to `sh`.
///
/// Returns `Err` when a custom path is missing, or (Windows only) when no bash
/// can be found. On Unix without a custom path this never errors.
pub fn get_shell_config(custom_shell_path: Option<&str>) -> std::io::Result<ShellConfig> {
    if let Some(custom) = custom_shell_path {
        if Path::new(custom).exists() {
            return Ok(bash_shell_config(custom));
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Custom shell path not found: {custom}"),
        ));
    }

    #[cfg(windows)]
    {
        let mut paths: Vec<String> = Vec::new();
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            paths.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
            paths.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for path in &paths {
            if Path::new(path).exists() {
                return Ok(bash_shell_config(path));
            }
        }
        if let Some(bash_on_path) = find_bash_on_path() {
            return Ok(bash_shell_config(&bash_on_path));
        }
        let searched = paths
            .iter()
            .map(|p| format!("  {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{searched}"
            ),
        ));
    }

    #[cfg(not(windows))]
    {
        if Path::new("/bin/bash").exists() {
            return Ok(bash_shell_config("/bin/bash"));
        }
        if let Some(bash_on_path) = find_bash_on_path() {
            return Ok(bash_shell_config(&bash_on_path));
        }
        Ok(ShellConfig {
            shell: "sh".to_string(),
            args: vec!["-c".to_string()],
            command_transport: None,
        })
    }
}

/// Minimal replica of pi's `config.ts:getBinDir()` (`join(getAgentDir(),
/// "bin")`). The full `config.ts` is not yet ported, so this reproduces just
/// the agent-dir resolution: honor `PI_CODING_AGENT_DIR` (with `~` expansion),
/// else `~/.pi/agent`. When this crate ports `config`, replace this with the
/// real accessor.
pub fn get_bin_dir() -> std::path::PathBuf {
    let agent_dir = match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(dir) if !dir.is_empty() => expand_tilde(&dir),
        _ => home_dir().join(".pi").join("agent"),
    };
    agent_dir.join("bin")
}

fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home_dir().join(rest)
    } else if path == "~" {
        home_dir()
    } else {
        std::path::PathBuf::from(path)
    }
}

fn home_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// Return the current environment with [`get_bin_dir`] prepended to `PATH`
/// (unless already present), mirroring pi's `getShellEnv()`.
///
/// The `PATH` key is matched case-insensitively (Windows uses `Path`); the
/// existing key's casing is preserved.
pub fn get_shell_env() -> Vec<(String, String)> {
    let bin_dir = get_bin_dir();
    let bin_dir_str = bin_dir.to_string_lossy().into_owned();
    let separator = if cfg!(windows) { ';' } else { ':' };

    let mut env: Vec<(String, String)> = std::env::vars().collect();
    let path_key = env
        .iter()
        .find(|(k, _)| k.to_lowercase() == "path")
        .map(|(k, _)| k.clone());

    match path_key {
        Some(key) => {
            for (k, v) in env.iter_mut() {
                if *k == key {
                    let entries: Vec<&str> = v.split(separator).filter(|e| !e.is_empty()).collect();
                    if !entries.iter().any(|e| *e == bin_dir_str) {
                        let mut parts: Vec<String> = Vec::new();
                        if !bin_dir_str.is_empty() {
                            parts.push(bin_dir_str.clone());
                        }
                        if !v.is_empty() {
                            parts.push(v.clone());
                        }
                        *v = parts.join(&separator.to_string());
                    }
                    break;
                }
            }
        }
        None => {
            env.push(("PATH".to_string(), bin_dir_str));
        }
    }
    env
}

/// Sanitize binary output for display/storage.
///
/// Byte-exact port of pi's `sanitizeBinaryOutput`. Removes:
/// - control characters `0x00..=0x1F` except tab (`0x09`), newline (`0x0A`),
///   and carriage return (`0x0D`);
/// - Unicode Format characters `U+FFF9..=U+FFFB` (which crash `string-width`).
///
/// Everything else is retained. Iterates by Unicode scalar value, matching
/// pi's `Array.from(...)` code-point iteration.
pub fn sanitize_binary_output(input: &str) -> String {
    input
        .chars()
        .filter(|&ch| {
            let code = ch as u32;
            // Allow tab, newline, carriage return.
            if code == 0x09 || code == 0x0a || code == 0x0d {
                return true;
            }
            // Filter out control characters (0x00-0x1F).
            if code <= 0x1f {
                return false;
            }
            // Filter out Unicode format characters.
            if (0xfff9..=0xfffb).contains(&code) {
                return false;
            }
            true
        })
        .collect()
}

/// Detached child PIDs, tracked so they can be reaped on parent shutdown
/// signals (SIGHUP/SIGTERM). Mirrors pi's module-level `Set<number>`.
static TRACKED_DETACHED_CHILD_PIDS: LazyLock<Mutex<HashSet<i32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Track a detached child PID for later reaping.
pub fn track_detached_child_pid(pid: i32) {
    TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().insert(pid);
}

/// Stop tracking a detached child PID.
pub fn untrack_detached_child_pid(pid: i32) {
    TRACKED_DETACHED_CHILD_PIDS.lock().unwrap().remove(&pid);
}

/// Kill every tracked detached child (process tree) and clear the set.
pub fn kill_tracked_detached_children() {
    // Snapshot under the lock, release, then kill, mirroring pi iterating the
    // set and clearing it.
    let pids: Vec<i32> = {
        let mut set = TRACKED_DETACHED_CHILD_PIDS.lock().unwrap();
        let pids = set.iter().copied().collect();
        set.clear();
        pids
    };
    for pid in pids {
        kill_process_tree(pid);
    }
}

/// Kill a process and all its children (cross-platform, best-effort).
///
/// Unix: `killpg(pid, SIGKILL)` — kills the whole process group whose PGID is
/// `pid` (pi's `process.kill(-pid, "SIGKILL")`). Falls back to a single-PID
/// `kill(pid, SIGKILL)` if the group kill fails. Windows: `taskkill /F /T`.
pub fn kill_process_tree(pid: i32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(unix)]
    {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;
        let target = Pid::from_raw(pid);
        if killpg(target, Signal::SIGKILL).is_err() {
            // Fallback: kill just the child if the process-group kill failed.
            let _ = nix::sys::signal::kill(target, Signal::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_tab_newline_cr() {
        assert_eq!(sanitize_binary_output("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn sanitize_strips_control_chars() {
        // 0x00 (NUL), 0x07 (BEL), 0x1b (ESC), 0x1f (US) all stripped.
        assert_eq!(
            sanitize_binary_output("a\u{0000}b\u{0007}c\u{001b}d\u{001f}e"),
            "abcde"
        );
    }

    #[test]
    fn sanitize_strips_format_chars_fff9_fffb() {
        assert_eq!(
            sanitize_binary_output("x\u{fff9}y\u{fffa}z\u{fffb}w"),
            "xyzw"
        );
    }

    #[test]
    fn sanitize_keeps_ordinary_unicode() {
        // Emoji, accented chars, CJK — all retained. The emoji is written as an
        // escaped code point (U+1F680 ROCKET) so this source file stays ASCII-safe
        // while still verifying emoji pass-through.
        assert_eq!(
            sanitize_binary_output("héllo 世界 \u{1f680}"),
            "héllo 世界 \u{1f680}"
        );
        // Boundary code points around the stripped format range are kept.
        assert_eq!(
            sanitize_binary_output("\u{fff8}\u{fffc}"),
            "\u{fff8}\u{fffc}"
        );
        // 0x20 (space) is the first kept control-range code point.
        assert_eq!(sanitize_binary_output(" "), " ");
    }

    #[test]
    fn sanitize_empty_string() {
        assert_eq!(sanitize_binary_output(""), "");
    }

    #[test]
    fn legacy_wsl_bash_detection() {
        assert!(is_legacy_wsl_bash_path(r"C:\Windows\System32\bash.exe"));
        assert!(is_legacy_wsl_bash_path(r"C:/Windows/System32/bash.exe"));
        assert!(is_legacy_wsl_bash_path(r"c:\windows\sysnative\bash.exe"));
        assert!(!is_legacy_wsl_bash_path("/bin/bash"));
        assert!(!is_legacy_wsl_bash_path(
            r"C:\Program Files\Git\bin\bash.exe"
        ));
    }

    #[test]
    fn bash_config_uses_dash_c_for_normal_shells() {
        let cfg = bash_shell_config("/bin/bash");
        assert_eq!(cfg.args, vec!["-c".to_string()]);
        assert_eq!(cfg.command_transport, None);
        assert!(!cfg.use_stdin_transport());
    }

    #[test]
    fn bash_config_uses_stdin_for_legacy_wsl() {
        let cfg = bash_shell_config(r"C:\Windows\System32\bash.exe");
        assert_eq!(cfg.args, vec!["-s".to_string()]);
        assert_eq!(cfg.command_transport, Some(CommandTransport::Stdin));
        assert!(cfg.use_stdin_transport());
    }

    #[test]
    fn custom_shell_path_missing_errors() {
        let err = get_shell_config(Some("/nonexistent/shell/xyz")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(err.to_string().contains("Custom shell path not found"));
    }

    #[cfg(unix)]
    #[test]
    fn resolves_a_shell_on_unix() {
        // On Unix without a custom path this always resolves (never errors).
        let cfg = get_shell_config(None).unwrap();
        assert_eq!(cfg.args, vec!["-c".to_string()]);
        assert!(!cfg.shell.is_empty());
    }

    #[test]
    fn shell_env_prepends_bin_dir_to_path() {
        let env = get_shell_env();
        let bin_dir = get_bin_dir().to_string_lossy().into_owned();
        let (_, path) = env
            .iter()
            .find(|(k, _)| k.to_lowercase() == "path")
            .expect("PATH present");
        let sep = if cfg!(windows) { ';' } else { ':' };
        let first = path.split(sep).next().unwrap_or("");
        assert_eq!(first, bin_dir);
    }

    #[test]
    fn detached_pid_tracking_add_remove() {
        // Exercise track/untrack/membership against the shared set using sentinel
        // PIDs that are never real processes. This deliberately does NOT call the
        // process-global reaper `kill_tracked_detached_children()`: that drains the
        // shared set and would kill real detached children spawned by concurrent
        // subprocess tests under cargo's parallel execution. Membership is checked
        // only for our own sentinel PIDs, so concurrent inserts of real PIDs by
        // other tests do not affect these assertions.
        let a = 999_999_991;
        let b = 999_999_992;
        track_detached_child_pid(a);
        track_detached_child_pid(b);
        {
            let set = TRACKED_DETACHED_CHILD_PIDS.lock().unwrap();
            assert!(set.contains(&a));
            assert!(set.contains(&b));
        }
        untrack_detached_child_pid(a);
        {
            let set = TRACKED_DETACHED_CHILD_PIDS.lock().unwrap();
            assert!(!set.contains(&a));
            assert!(set.contains(&b));
        }
        // Clean up only our own sentinel; never touch other tests' PIDs.
        untrack_detached_child_pid(b);
    }

    #[cfg(unix)]
    #[test]
    fn kill_process_tree_reaps_controlled_child() {
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        // Spawn a disposable child as its own process-group leader (setpgid(0,0)
        // via process_group), so `killpg(pid, SIGKILL)` targets exactly this
        // group and nothing else. This exercises the real killpg code path
        // against a child we own, with no reliance on the shared tracking set.
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;

        kill_process_tree(pid);

        // wait() returns once the SIGKILL'd child is reaped; the child was killed,
        // so it must not report success.
        let status = child.wait().expect("wait on child");
        assert!(!status.success());
    }
}
