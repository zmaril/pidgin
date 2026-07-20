//! Port of pi's `utils/clipboard.ts` — writing text to the system clipboard,
//! plus [`read_clipboard_text`].
//!
//! ## What is ported
//!
//! [`copy_to_clipboard`] mirrors pi's `copyToClipboard`: it shells out to the
//! platform clipboard tools (`pbcopy` on macOS, `clip` on Windows,
//! `termux-clipboard-set` / `wl-copy` / `xclip` / `xsel` on Linux) and falls
//! back to an OSC 52 terminal escape sequence for SSH/MOSH remote sessions and
//! whenever no native tool succeeds. The OSC 52 path is a plain `stdout` write
//! of `\x1b]52;c;<base64>\x07` — no subprocess — matching pi exactly (pi does
//! **not** wrap the sequence in tmux/screen passthrough).
//!
//! ## Deferred: the native-addon fast path
//!
//! pi prefers a native Node addon (`@mariozechner/clipboard`, loaded by
//! `clipboard-native.ts`) over the subprocess tools on every platform except
//! Linux, and `readClipboardText` reads text *only* through that addon. There is
//! no Rust equivalent of that addon yet (see [`super::clipboard_native`]), so —
//! following the sdk.rs precedent of documenting a hook with no destination
//! rather than faking one — the native branch is omitted, not stubbed with a
//! fabricated backend:
//!
//! - In [`copy_to_clipboard`] the native branch is where pi sets `copied = true`
//!   before the subprocess tools run. With the addon absent, `copied` simply
//!   starts `false`, so control always flows to the subprocess tools and the
//!   OSC 52 fallback. pi's `if (copied && !remote) return;` short-circuit is
//!   preserved structurally even though it is unreachable while the addon is
//!   deferred.
//! - [`read_clipboard_text`] has no non-addon path in pi, so it always returns
//!   `None` here until a Rust-native clipboard backend is selected.
//!
//! ## Subprocess primitive
//!
//! The shell-outs write the payload to the child's **stdin** (pi's
//! `execSync(cmd, { input: text })`). The crate's synchronous primitive
//! [`super::child_process::spawn_process_sync`] runs `Command::output()`
//! internally, which closes stdin before anything can be written, so it cannot
//! carry the `input`. This port therefore uses its async sibling
//! [`super::child_process::spawn_process`] (which returns a `Child` whose stdin
//! we can write to — the same pattern `core/tools/bash.rs` uses) and awaits the
//! exit. stdout/stderr are discarded (pi's `stdio: ["pipe", "ignore",
//! "ignore"]`); crucially this means `wl-copy`, which daemonizes to keep
//! selection ownership, does not hang the wait on an inherited pipe (the bug pi
//! avoided by using `spawn` instead of `execSync` for `wl-copy`).

use std::process::Stdio;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use tokio::io::AsyncWriteExt;

use super::child_process::spawn_process;

/// Largest base64 payload pi will emit as OSC 52 (`MAX_OSC52_ENCODED_LENGTH`).
/// Larger payloads can desynchronize terminal rendering, so they are dropped.
const MAX_OSC52_ENCODED_LENGTH: usize = 100_000;

/// Timeout applied to each clipboard-tool subprocess (pi's `timeout: 5000`).
const WRITE_TIMEOUT_MS: u64 = 5000;

/// Host platform, mirroring the cases pi branches on from `os.platform()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    /// macOS (`"darwin"`).
    Darwin,
    /// Windows (`"win32"`).
    Win32,
    /// Everything else. pi's `else` branch (Linux and any other platform) runs
    /// the Linux clipboard-tool logic.
    Other,
}

/// Detect the current platform. `os.platform()` returns `"darwin"`/`"win32"`;
/// Rust's [`std::env::consts::OS`] returns `"macos"`/`"windows"`.
fn current_platform() -> Platform {
    match std::env::consts::OS {
        "macos" => Platform::Darwin,
        "windows" => Platform::Win32,
        _ => Platform::Other,
    }
}

/// A clipboard-write tool to shell out to, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteTool {
    /// macOS `pbcopy`.
    Pbcopy,
    /// Windows `clip`.
    Clip,
    /// Termux `termux-clipboard-set`.
    Termux,
    /// Wayland `wl-copy`.
    WlCopy,
    /// X11 `xclip -selection clipboard`.
    Xclip,
    /// X11 `xsel --clipboard --input` (pi's `copyToX11Clipboard` fallback after
    /// `xclip`).
    Xsel,
}

/// Look up an environment variable, treating an empty value as absent to match
/// JavaScript's `Boolean(process.env.X)` truthiness.
fn system_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Whether this is a remote session (pi's `isRemoteSession`): any of
/// `SSH_CONNECTION`, `SSH_CLIENT`, or `MOSH_CONNECTION` is set.
fn is_remote_session<E: Fn(&str) -> Option<String>>(env: &E) -> bool {
    env("SSH_CONNECTION").is_some()
        || env("SSH_CLIENT").is_some()
        || env("MOSH_CONNECTION").is_some()
}

/// Whether this is a Wayland session (pi's `isWaylandSession`, defined in
/// `clipboard-image.ts`): `WAYLAND_DISPLAY` is set, or `XDG_SESSION_TYPE` is
/// exactly `"wayland"`.
///
/// pi imports this from `clipboard-image.ts`; that module is still deferred
/// ([`super::clipboard_image`]), so the helper is reproduced here (a single-file
/// duplication, resolved when the image module lands).
fn is_wayland_session<E: Fn(&str) -> Option<String>>(env: &E) -> bool {
    env("WAYLAND_DISPLAY").is_some() || env("XDG_SESSION_TYPE").as_deref() == Some("wayland")
}

/// The Linux clipboard tools to try, in priority order (pi's Linux branch).
///
/// Termux (if `TERMUX_VERSION`) is tried first; then Wayland (`wl-copy`) when
/// this is a Wayland session with a `WAYLAND_DISPLAY`, falling back to X11
/// (`xclip` then `xsel`) when `DISPLAY` is set; otherwise X11 alone when
/// `DISPLAY` is set. pi attempts each in turn and swallows failures, which the
/// caller reproduces by trying the list until one succeeds.
fn linux_write_tools<E: Fn(&str) -> Option<String>>(env: &E) -> Vec<WriteTool> {
    let mut tools = Vec::new();

    if env("TERMUX_VERSION").is_some() {
        tools.push(WriteTool::Termux);
    }

    let has_wayland_display = env("WAYLAND_DISPLAY").is_some();
    let has_x11_display = env("DISPLAY").is_some();
    let is_wayland = is_wayland_session(env);

    if is_wayland && has_wayland_display {
        tools.push(WriteTool::WlCopy);
        // pi's `copyToX11Clipboard` fallback if `wl-copy` throws.
        if has_x11_display {
            tools.push(WriteTool::Xclip);
            tools.push(WriteTool::Xsel);
        }
    } else if has_x11_display {
        tools.push(WriteTool::Xclip);
        tools.push(WriteTool::Xsel);
    }

    tools
}

/// The clipboard-write tools to try for `platform`, in priority order.
fn write_tools<E: Fn(&str) -> Option<String>>(platform: Platform, env: &E) -> Vec<WriteTool> {
    match platform {
        Platform::Darwin => vec![WriteTool::Pbcopy],
        Platform::Win32 => vec![WriteTool::Clip],
        // pi's `else`: Linux and any other platform run the Linux logic.
        Platform::Other => linux_write_tools(env),
    }
}

/// The command and arguments for a [`WriteTool`].
fn write_command(tool: WriteTool) -> (&'static str, &'static [&'static str]) {
    match tool {
        WriteTool::Pbcopy => ("pbcopy", &[]),
        WriteTool::Clip => ("clip", &[]),
        WriteTool::Termux => ("termux-clipboard-set", &[]),
        WriteTool::WlCopy => ("wl-copy", &[]),
        WriteTool::Xclip => ("xclip", &["-selection", "clipboard"]),
        WriteTool::Xsel => ("xsel", &["--clipboard", "--input"]),
    }
}

/// Build the OSC 52 clipboard escape sequence for `text`, or `None` when the
/// base64 payload exceeds [`MAX_OSC52_ENCODED_LENGTH`] (pi's `emitOsc52`
/// length guard). The sequence is `\x1b]52;c;<base64>\x07` with standard,
/// padded base64 (pi's `Buffer.from(text).toString("base64")`).
fn osc52_sequence(text: &str) -> Option<String> {
    let encoded = BASE64_STANDARD.encode(text.as_bytes());
    if encoded.len() > MAX_OSC52_ENCODED_LENGTH {
        return None;
    }
    Some(format!("\u{1b}]52;c;{encoded}\u{7}"))
}

/// Emit the OSC 52 clipboard sequence for `text` to stdout, returning whether it
/// was emitted. Mirrors pi's `emitOsc52`: `false` when the payload is too large;
/// otherwise the sequence is written (write errors are ignored, as in pi where
/// `process.stdout.write` is not checked) and `true` is returned.
fn emit_osc52(text: &str) -> bool {
    match osc52_sequence(text) {
        Some(sequence) => {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(sequence.as_bytes());
            let _ = stdout.flush();
            true
        }
        None => false,
    }
}

/// Write `input` to `command`'s stdin and wait for it to exit successfully.
///
/// Mirrors pi's `execSync(command, { input, timeout: 5000, stdio: ["pipe",
/// "ignore", "ignore"] })`. stdout/stderr are discarded so a daemonizing tool
/// (`wl-copy`) does not stall the wait on an inherited pipe. A missing tool
/// surfaces as a spawn `Err`, which the caller treats as "try the next tool".
async fn write_via_command(command: &str, args: &[&str], input: &str) -> std::io::Result<()> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let mut child = spawn_process(command, &args, |cmd| {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        // Ignore EPIPE if the tool exits before consuming all input (pi ignores
        // `proc.stdin.on("error")`).
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let status =
        match tokio::time::timeout(Duration::from_millis(WRITE_TIMEOUT_MS), child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.start_kill();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "clipboard write timed out",
                ));
            }
        };

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "clipboard tool exited with a failure",
        ))
    }
}

/// Read plain text from the system clipboard, if native clipboard access is
/// available (pi's `readClipboardText`).
///
/// pi reads text solely through the native addon (`clipboard.getText()`). That
/// addon is deferred ([`super::clipboard_native`]) and there is no non-addon
/// read path, so this always returns `None` until a Rust-native clipboard
/// backend is selected.
pub fn read_clipboard_text() -> Option<String> {
    // Native addon fast path deferred (see module docs): no backend, no text.
    None
}

/// Copy `text` to the system clipboard (pi's `copyToClipboard`).
///
/// Prefers the platform clipboard tools, then falls back to OSC 52 for remote
/// sessions or when no tool succeeds. Returns an error only when nothing —
/// neither a tool nor OSC 52 — managed to copy.
///
/// The native-addon fast path pi tries first is deferred (see the module docs),
/// so `copied` starts `false` and control always reaches the subprocess tools
/// and the OSC 52 fallback.
pub async fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    let mut copied = false;
    let platform = current_platform();
    let remote = is_remote_session(&system_env);

    // pi sets `copied = true` in the native-addon branch here; that branch is
    // deferred, so `copied` stays `false`. The short-circuit is kept for
    // structural fidelity even though it is currently unreachable.
    if copied && !remote {
        return Ok(());
    }

    if !copied {
        for tool in write_tools(platform, &system_env) {
            let (command, args) = write_command(tool);
            if write_via_command(command, args, text).await.is_ok() {
                copied = true;
                break;
            }
        }
    }

    if remote || !copied {
        let osc52_copied = emit_osc52(text);
        copied = copied || osc52_copied;
    }

    if !copied {
        anyhow::bail!("Failed to copy to clipboard");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build an env lookup over a fixed map, filtering empty values like
    /// [`system_env`] does.
    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn osc52_sequence_wraps_standard_base64() {
        // "hi" -> base64 "aGk=".
        assert_eq!(
            osc52_sequence("hi"),
            Some("\u{1b}]52;c;aGk=\u{7}".to_string())
        );
        // Empty text -> empty base64 payload, still a valid sequence.
        assert_eq!(osc52_sequence(""), Some("\u{1b}]52;c;\u{7}".to_string()));
    }

    #[test]
    fn osc52_sequence_matches_padded_base64_of_bytes() {
        let text = "The quick brown fox";
        let expected = BASE64_STANDARD.encode(text.as_bytes());
        let sequence = osc52_sequence(text).unwrap();
        assert_eq!(sequence, format!("\u{1b}]52;c;{expected}\u{7}"));
        // Standard base64 keeps padding.
        assert!(expected.ends_with('='));
    }

    #[test]
    fn osc52_sequence_rejects_oversized_payload() {
        // 3 raw bytes -> 4 base64 chars, so > 75_000 bytes exceeds the 100_000
        // char cap.
        let text = "a".repeat(76_000);
        assert!(osc52_sequence(&text).is_none());
        // And the emitter reports failure without writing.
        assert!(!emit_osc52(&text));
    }

    #[test]
    fn emit_osc52_reports_success_for_small_payload() {
        assert!(emit_osc52("small"));
    }

    #[test]
    fn is_remote_session_detects_ssh_and_mosh() {
        assert!(is_remote_session(&env_from(&[(
            "SSH_CONNECTION",
            "1.2.3.4 5 6.7.8.9 22"
        )])));
        assert!(is_remote_session(&env_from(&[(
            "SSH_CLIENT",
            "1.2.3.4 5 22"
        )])));
        assert!(is_remote_session(&env_from(&[("MOSH_CONNECTION", "1")])));
        assert!(!is_remote_session(&env_from(&[])));
        // Empty value is falsy, matching JS `Boolean(env.X)`.
        assert!(!is_remote_session(&env_from(&[("SSH_CONNECTION", "")])));
    }

    #[test]
    fn is_wayland_session_matches_pi() {
        assert!(is_wayland_session(&env_from(&[(
            "WAYLAND_DISPLAY",
            "wayland-0"
        )])));
        assert!(is_wayland_session(&env_from(&[(
            "XDG_SESSION_TYPE",
            "wayland"
        )])));
        assert!(!is_wayland_session(&env_from(&[(
            "XDG_SESSION_TYPE",
            "x11"
        )])));
        assert!(!is_wayland_session(&env_from(&[])));
    }

    #[test]
    fn write_tools_darwin_and_win32() {
        let env = env_from(&[]);
        assert_eq!(write_tools(Platform::Darwin, &env), vec![WriteTool::Pbcopy]);
        assert_eq!(write_tools(Platform::Win32, &env), vec![WriteTool::Clip]);
    }

    #[test]
    fn linux_termux_only() {
        let tools = write_tools(Platform::Other, &env_from(&[("TERMUX_VERSION", "0.118")]));
        assert_eq!(tools, vec![WriteTool::Termux]);
    }

    #[test]
    fn linux_wayland_without_x11() {
        let env = env_from(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_SESSION_TYPE", "wayland"),
        ]);
        assert_eq!(write_tools(Platform::Other, &env), vec![WriteTool::WlCopy]);
    }

    #[test]
    fn linux_wayland_with_x11_fallback() {
        let env = env_from(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]);
        assert_eq!(
            write_tools(Platform::Other, &env),
            vec![WriteTool::WlCopy, WriteTool::Xclip, WriteTool::Xsel]
        );
    }

    #[test]
    fn linux_x11_only() {
        let env = env_from(&[("DISPLAY", ":0")]);
        assert_eq!(
            write_tools(Platform::Other, &env),
            vec![WriteTool::Xclip, WriteTool::Xsel]
        );
    }

    #[test]
    fn linux_wayland_type_without_display_uses_x11() {
        // pi requires `isWayland && hasWaylandDisplay`; XDG type alone with no
        // WAYLAND_DISPLAY falls through to X11.
        let env = env_from(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")]);
        assert_eq!(
            write_tools(Platform::Other, &env),
            vec![WriteTool::Xclip, WriteTool::Xsel]
        );
    }

    #[test]
    fn linux_termux_then_wayland_then_x11() {
        let env = env_from(&[
            ("TERMUX_VERSION", "0.118"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
        ]);
        assert_eq!(
            write_tools(Platform::Other, &env),
            vec![
                WriteTool::Termux,
                WriteTool::WlCopy,
                WriteTool::Xclip,
                WriteTool::Xsel
            ]
        );
    }

    #[test]
    fn linux_headless_has_no_tools() {
        assert!(write_tools(Platform::Other, &env_from(&[])).is_empty());
    }

    #[test]
    fn write_command_maps_tools() {
        assert_eq!(write_command(WriteTool::Pbcopy), ("pbcopy", &[][..]));
        assert_eq!(write_command(WriteTool::Clip), ("clip", &[][..]));
        assert_eq!(
            write_command(WriteTool::Xclip),
            ("xclip", &["-selection", "clipboard"][..])
        );
        assert_eq!(
            write_command(WriteTool::Xsel),
            ("xsel", &["--clipboard", "--input"][..])
        );
    }

    #[test]
    fn read_clipboard_text_is_deferred() {
        // Native addon deferred: no text is ever returned.
        assert_eq!(read_clipboard_text(), None);
    }
}
