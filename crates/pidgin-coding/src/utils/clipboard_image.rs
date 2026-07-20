//! Read an image from the system clipboard.
//!
//! Port of pi's `utils/clipboard-image.ts`. pi reads clipboard images through a
//! mix of platform subprocesses and a native Node addon, then normalizes the
//! result to a supported image format. This module ports the **subprocess**
//! paths faithfully via the crate's binary-safe
//! [`spawn_process_sync`](super::child_process::spawn_process_sync) (whose
//! `stdout` is `Vec<u8>`, correct for raw PNG bytes):
//!
//! * Wayland / WSL -> `wl-paste --list-types` then `wl-paste --type <t>
//!   --no-newline`.
//! * X11 -> `xclip -selection clipboard -t TARGETS -o` then per-mime
//!   `xclip -selection clipboard -t <mime> -o`.
//! * WSL fallback -> a PowerShell one-liner
//!   (`[System.Windows.Forms.Clipboard]::GetImage()`) that saves a PNG to a
//!   temp file, with the path bridged through `wslpath -w`.
//!
//! It also ports pi's interactive consumer behavior: write the raw bytes to a
//! temp file `pi-clipboard-<uuid>.<ext>` and return its path (the interactive
//! editor inserts that path at the cursor rather than inlining the bytes).
//!
//! ## Deferrals
//!
//! * **Native addon (`getImageBinary`).** pi's non-Wayland Linux and
//!   non-Linux paths read via the `@mariozechner/clipboard` native Node addon.
//!   There is no Rust equivalent (see [`super::clipboard_native`]), so
//!   [`read_via_native_clipboard`] is a documented stub that always yields
//!   `None`.
//! * **BMP -> PNG conversion.** pi converts unsupported clipboard formats (e.g.
//!   BMP from WSLg) to PNG with Photon/WASM. That image pipeline is being
//!   ported separately (`port/image-pipeline`) and is unavailable here, so
//!   [`convert_to_png`] is a documented stub. Rather than dropping the image
//!   as pi does when Photon is unavailable, [`read_clipboard_image`] surfaces
//!   the **raw bytes** with their true mime type so the subprocess read is not
//!   wasted; the conversion will land with the image pipeline.
//! * **Per-command timeout.** pi passes a `timeout` to `spawnSync` (1-5s).
//!   `spawn_process_sync` wraps `std::process::Command`, which has no built-in
//!   timeout; the timeout values are kept as constants for parity but are not
//!   currently enforced. The `maxBuffer` size guard (50MB) *is* enforced, but
//!   only after capture (std cannot cap a pipe mid-stream the way Node does).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use pidgin_agent::harness::session::uuidv7;

use super::child_process::spawn_process_sync;

/// A raw image read from the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    /// The raw image bytes as emitted by the clipboard tool.
    pub bytes: Vec<u8>,
    /// The base mime type (e.g. `image/png`).
    pub mime_type: String,
}

/// A snapshot of environment variables, mirroring pi's injectable
/// `NodeJS.ProcessEnv`. Build one with [`current_env`] in production or a plain
/// map in tests.
pub type EnvMap = HashMap<String, String>;

/// Image mime types pi treats as directly usable (no conversion needed), in
/// preference order.
const SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Timeout for the type-listing commands (`wl-paste --list-types`, `xclip
/// TARGETS`, `wslpath`). Parity with pi; see the module deferral note.
const DEFAULT_LIST_TIMEOUT_MS: u64 = 1000;
/// Timeout for the data-read commands. Parity with pi; see the deferral note.
const DEFAULT_READ_TIMEOUT_MS: u64 = 3000;
/// Timeout for the PowerShell fallback. Parity with pi; see the deferral note.
const DEFAULT_POWERSHELL_TIMEOUT_MS: u64 = 5000;
/// Maximum captured stdout size (pi's `maxBuffer`). Output larger than this is
/// treated as a failure.
const DEFAULT_MAX_BUFFER_BYTES: usize = 50 * 1024 * 1024;

/// Whether an env var is "truthy" in the JS sense pi relies on: present and
/// non-empty.
fn env_truthy(env: &EnvMap, key: &str) -> bool {
    env.get(key).is_some_and(|v| !v.is_empty())
}

/// Mirror pi's `isWaylandSession`: `WAYLAND_DISPLAY` set (truthy) or
/// `XDG_SESSION_TYPE === "wayland"`.
pub fn is_wayland_session(env: &EnvMap) -> bool {
    env_truthy(env, "WAYLAND_DISPLAY")
        || env.get("XDG_SESSION_TYPE").map(String::as_str) == Some("wayland")
}

/// Mirror pi's `baseMimeType`: strip parameters, trim, lowercase.
fn base_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or(mime_type)
        .trim()
        .to_lowercase()
}

/// Mirror pi's `extensionForImageMimeType`: file extension for a supported
/// image mime type, or `None` for anything else.
pub fn extension_for_image_mime_type(mime_type: &str) -> Option<&'static str> {
    match base_mime_type(mime_type).as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// Mirror pi's `selectPreferredImageMimeType`: prefer the supported types in
/// order, else the first `image/*`, returning the original (trimmed) raw type.
fn select_preferred_image_mime_type(mime_types: &[String]) -> Option<String> {
    let normalized: Vec<(String, String)> = mime_types
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| (t.to_string(), base_mime_type(t)))
        .collect();

    for preferred in SUPPORTED_IMAGE_MIME_TYPES {
        if let Some((raw, _)) = normalized.iter().find(|(_, base)| base == preferred) {
            return Some(raw.clone());
        }
    }

    normalized
        .iter()
        .find(|(_, base)| base.starts_with("image/"))
        .map(|(raw, _)| raw.clone())
}

/// Mirror pi's `isSupportedImageMimeType`.
fn is_supported_image_mime_type(mime_type: &str) -> bool {
    let base = base_mime_type(mime_type);
    SUPPORTED_IMAGE_MIME_TYPES.iter().any(|t| *t == base)
}

/// Deferred BMP -> PNG conversion seam (pi's `convertToPng` via Photon/WASM).
///
/// The Photon-backed image pipeline is being ported separately
/// (`port/image-pipeline`); until it lands there is no in-crate way to decode
/// and re-encode, so this always returns `None`. See the module deferral note.
fn convert_to_png(_bytes: &[u8]) -> Option<Vec<u8>> {
    None
}

/// Result of a clipboard subprocess, mirroring pi's `runCommand` return
/// (`{ stdout, ok }`).
struct CommandResult {
    stdout: Vec<u8>,
    ok: bool,
}

/// Run a clipboard tool to completion and capture its binary stdout.
///
/// Faithful analogue of pi's `runCommand` (a `spawnSync` wrapper). A spawn
/// error, a non-zero exit, or output exceeding `max_buffer_bytes` all map to
/// `ok: false` with empty stdout. `_timeout_ms` carries pi's per-command
/// timeout for parity but is not enforced (`std::process::Command` has no
/// built-in timeout); see the module deferral note.
fn run_command(
    command: &str,
    args: &[String],
    _timeout_ms: u64,
    max_buffer_bytes: usize,
) -> CommandResult {
    let output = match spawn_process_sync(command, args, |_| {}) {
        Ok(output) => output,
        Err(_) => {
            return CommandResult {
                stdout: Vec::new(),
                ok: false,
            }
        }
    };

    if !output.status.success() {
        return CommandResult {
            stdout: Vec::new(),
            ok: false,
        };
    }

    if output.stdout.len() > max_buffer_bytes {
        return CommandResult {
            stdout: Vec::new(),
            ok: false,
        };
    }

    CommandResult {
        stdout: output.stdout,
        ok: true,
    }
}

/// Split captured stdout into trimmed, non-empty lines (pi splits on
/// `/\r?\n/`; `str::lines` matches that and strips a trailing `\r`).
fn parse_type_lines(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Mirror pi's `readClipboardImageViaWlPaste`.
fn read_via_wl_paste() -> Option<ClipboardImage> {
    let list = run_command(
        "wl-paste",
        &["--list-types".to_string()],
        DEFAULT_LIST_TIMEOUT_MS,
        DEFAULT_MAX_BUFFER_BYTES,
    );
    if !list.ok {
        return None;
    }

    let types = parse_type_lines(&list.stdout);
    let selected_type = select_preferred_image_mime_type(&types)?;

    let data = run_command(
        "wl-paste",
        &[
            "--type".to_string(),
            selected_type.clone(),
            "--no-newline".to_string(),
        ],
        DEFAULT_READ_TIMEOUT_MS,
        DEFAULT_MAX_BUFFER_BYTES,
    );
    if !data.ok || data.stdout.is_empty() {
        return None;
    }

    Some(ClipboardImage {
        bytes: data.stdout,
        mime_type: base_mime_type(&selected_type),
    })
}

/// Mirror pi's `readClipboardImageViaXclip`.
fn read_via_xclip() -> Option<ClipboardImage> {
    let targets = run_command(
        "xclip",
        &[
            "-selection".to_string(),
            "clipboard".to_string(),
            "-t".to_string(),
            "TARGETS".to_string(),
            "-o".to_string(),
        ],
        DEFAULT_LIST_TIMEOUT_MS,
        DEFAULT_MAX_BUFFER_BYTES,
    );

    let candidate_types = if targets.ok {
        parse_type_lines(&targets.stdout)
    } else {
        Vec::new()
    };

    let preferred = if candidate_types.is_empty() {
        None
    } else {
        select_preferred_image_mime_type(&candidate_types)
    };

    let mut try_types: Vec<String> = Vec::new();
    if let Some(preferred) = preferred {
        try_types.push(preferred);
    }
    try_types.extend(SUPPORTED_IMAGE_MIME_TYPES.iter().map(|t| t.to_string()));

    for mime_type in &try_types {
        let data = run_command(
            "xclip",
            &[
                "-selection".to_string(),
                "clipboard".to_string(),
                "-t".to_string(),
                mime_type.clone(),
                "-o".to_string(),
            ],
            DEFAULT_READ_TIMEOUT_MS,
            DEFAULT_MAX_BUFFER_BYTES,
        );
        if data.ok && !data.stdout.is_empty() {
            return Some(ClipboardImage {
                bytes: data.stdout,
                mime_type: base_mime_type(mime_type),
            });
        }
    }

    None
}

/// Mirror pi's `isWSL`: WSL env vars, else a `/proc/version` sniff.
fn is_wsl(env: &EnvMap) -> bool {
    if env_truthy(env, "WSL_DISTRO_NAME") || env_truthy(env, "WSLENV") {
        return true;
    }

    match fs::read_to_string("/proc/version") {
        Ok(release) => {
            let lower = release.to_lowercase();
            lower.contains("microsoft") || lower.contains("wsl")
        }
        Err(_) => false,
    }
}

/// Mirror pi's `readClipboardImageViaPowerShell`.
///
/// On WSL the Linux clipboard does not receive image data from Windows
/// screenshots (Win+Shift+S). PowerShell can reach the Windows clipboard
/// directly: it saves the image to a temp file (path bridged via `wslpath -w`)
/// which we then read back. The temp file is always cleaned up.
fn read_via_powershell() -> Option<ClipboardImage> {
    let tmp_file = std::env::temp_dir().join(format!("pi-wsl-clip-{}.png", uuidv7()));

    let result = read_via_powershell_into(&tmp_file);

    // pi's `finally`: best-effort cleanup, ignoring errors.
    let _ = fs::remove_file(&tmp_file);

    result
}

/// The body of [`read_via_powershell`], factored out so the temp file is
/// cleaned up on every return path (pi's `try/finally`).
fn read_via_powershell_into(tmp_file: &PathBuf) -> Option<ClipboardImage> {
    let tmp_str = tmp_file.to_string_lossy().to_string();

    let win_path_result = run_command(
        "wslpath",
        &["-w".to_string(), tmp_str],
        DEFAULT_LIST_TIMEOUT_MS,
        DEFAULT_MAX_BUFFER_BYTES,
    );
    if !win_path_result.ok {
        return None;
    }

    let win_path = String::from_utf8_lossy(&win_path_result.stdout)
        .trim()
        .to_string();
    if win_path.is_empty() {
        return None;
    }

    let ps_quoted_win_path = win_path.replace('\'', "''");
    let ps_script = [
        "Add-Type -AssemblyName System.Windows.Forms".to_string(),
        "Add-Type -AssemblyName System.Drawing".to_string(),
        format!("$path = '{ps_quoted_win_path}'"),
        "$img = [System.Windows.Forms.Clipboard]::GetImage()".to_string(),
        "if ($img) { $img.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' } else { Write-Output 'empty' }".to_string(),
    ]
    .join("; ");

    let result = run_command(
        "powershell.exe",
        &["-NoProfile".to_string(), "-Command".to_string(), ps_script],
        DEFAULT_POWERSHELL_TIMEOUT_MS,
        DEFAULT_MAX_BUFFER_BYTES,
    );
    if !result.ok {
        return None;
    }

    let output = String::from_utf8_lossy(&result.stdout).trim().to_string();
    if output != "ok" {
        return None;
    }

    let bytes = fs::read(tmp_file).ok()?;
    if bytes.is_empty() {
        return None;
    }

    Some(ClipboardImage {
        bytes,
        mime_type: "image/png".to_string(),
    })
}

/// Deferred native-addon read (pi's `readClipboardImageViaNativeClipboard`).
///
/// pi calls the `@mariozechner/clipboard` addon's `getImageBinary()`. There is
/// no Rust equivalent (see [`super::clipboard_native`]), so this always returns
/// `None`. It is kept in the strategy list to preserve pi's dispatch shape.
fn read_via_native_clipboard() -> Option<ClipboardImage> {
    None
}

/// One clipboard-read strategy, in the order pi attempts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadStrategy {
    /// `wl-paste` (Wayland / WSL).
    WlPaste,
    /// `xclip` (X11).
    Xclip,
    /// PowerShell fallback (WSL).
    PowerShell,
    /// Native addon (deferred).
    Native,
}

/// The ordered strategies pi's `readClipboardImage` tries, encoding its
/// platform dispatch. `wsl` is passed in because [`is_wsl`] may read the
/// filesystem; keeping it a parameter makes this selection pure and testable.
///
/// The runtime loop stops at the first strategy that yields an image, so this
/// list is equivalent to pi's short-circuiting `a ?? b` chains — including the
/// intentional repeat of `xclip` on non-Wayland WSL.
fn clipboard_read_strategies(platform: &str, env: &EnvMap, wsl: bool) -> Vec<ReadStrategy> {
    if env_truthy(env, "TERMUX_VERSION") {
        return Vec::new();
    }

    let mut strategies = Vec::new();

    if platform == "linux" {
        let wayland = is_wayland_session(env);

        if wayland || wsl {
            strategies.push(ReadStrategy::WlPaste);
            strategies.push(ReadStrategy::Xclip);
        }
        if wsl {
            strategies.push(ReadStrategy::PowerShell);
        }
        if !wayland {
            strategies.push(ReadStrategy::Native);
            strategies.push(ReadStrategy::Xclip);
        }
    } else {
        strategies.push(ReadStrategy::Native);
    }

    strategies
}

/// Environment snapshot for the current process (pi's default `process.env`).
pub fn current_env() -> EnvMap {
    std::env::vars().collect()
}

/// The current platform in pi's `NodeJS.Platform` vocabulary (`linux`,
/// `darwin`, `win32`), mapped from Rust's `std::env::consts::OS`.
pub fn current_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Read an image from the clipboard, mirroring pi's `readClipboardImage`.
///
/// `env`/`platform` are injectable (pi's optional options). Strategies are
/// tried in [`clipboard_read_strategies`] order until one yields an image.
///
/// Unsupported-format handling deviates from pi by design: pi converts to PNG
/// via Photon and returns `None` when conversion is unavailable. Here the
/// conversion is deferred (see [`convert_to_png`]), so instead of discarding a
/// successful subprocess read we surface the **raw bytes** with their true mime
/// type. The image pipeline port will perform the conversion.
pub fn read_clipboard_image(env: &EnvMap, platform: &str) -> Option<ClipboardImage> {
    let wsl = platform == "linux" && is_wsl(env);

    let mut image: Option<ClipboardImage> = None;
    for strategy in clipboard_read_strategies(platform, env, wsl) {
        image = match strategy {
            ReadStrategy::WlPaste => read_via_wl_paste(),
            ReadStrategy::Xclip => read_via_xclip(),
            ReadStrategy::PowerShell => read_via_powershell(),
            ReadStrategy::Native => read_via_native_clipboard(),
        };
        if image.is_some() {
            break;
        }
    }

    let image = image?;

    // Convert unsupported formats (e.g. BMP from WSLg) to PNG. Conversion is
    // deferred; surface the raw bytes rather than dropping the image.
    if !is_supported_image_mime_type(&image.mime_type) {
        if let Some(png_bytes) = convert_to_png(&image.bytes) {
            return Some(ClipboardImage {
                bytes: png_bytes,
                mime_type: "image/png".to_string(),
            });
        }
        return Some(image);
    }

    Some(image)
}

/// Extension for the clipboard temp file.
///
/// Mirrors pi's consumer, which uses `extensionForImageMimeType(mime) ?? "png"`.
/// pi only reaches that fallback for already-supported types (it pre-converts
/// everything else to PNG). Because BMP -> PNG conversion is deferred here,
/// [`read_clipboard_image`] can surface raw bytes of an unsupported type; to
/// keep the temp path honest (a BMP becomes a `.bmp` path, not a mislabeled
/// `.png`) we derive the extension from the mime subtype for such types before
/// falling back to `png`. For the four supported types this is byte-identical
/// to pi.
fn temp_file_extension(mime_type: &str) -> String {
    if let Some(ext) = extension_for_image_mime_type(mime_type) {
        return ext.to_string();
    }

    let base = base_mime_type(mime_type);
    if let Some(subtype) = base.strip_prefix("image/") {
        let cleaned: String = subtype
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    "png".to_string()
}

/// Write clipboard image bytes to a temp file and return its path.
///
/// Mirrors pi's interactive consumer: bytes are written to
/// `<tmpdir>/pi-clipboard-<uuid>.<ext>` and the path is returned so the editor
/// can insert it at the cursor (the interactive lane never inlines the bytes).
///
/// Port note: pi mints the id with `crypto.randomUUID` (v4); this uses the
/// crate's [`uuidv7`] (as `settings_manager` does) to avoid a new dependency.
/// The observable shape — `pi-clipboard-<uuid>.<ext>` under the system temp dir
/// — is preserved.
pub fn write_clipboard_image_to_temp_file(image: &ClipboardImage) -> std::io::Result<PathBuf> {
    let ext = temp_file_extension(&image.mime_type);
    let file_name = format!("pi-clipboard-{}.{ext}", uuidv7());
    let file_path = std::env::temp_dir().join(file_name);
    fs::write(&file_path, &image.bytes)?;
    Ok(file_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> EnvMap {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn base_mime_type_strips_params_and_lowercases() {
        assert_eq!(base_mime_type("Image/PNG"), "image/png");
        assert_eq!(base_mime_type("image/png; charset=binary"), "image/png");
        assert_eq!(base_mime_type("  image/JPEG  "), "image/jpeg");
    }

    #[test]
    fn extension_for_image_mime_type_matches_pi() {
        assert_eq!(extension_for_image_mime_type("image/png"), Some("png"));
        assert_eq!(extension_for_image_mime_type("image/jpeg"), Some("jpg"));
        assert_eq!(extension_for_image_mime_type("image/webp"), Some("webp"));
        assert_eq!(extension_for_image_mime_type("image/gif"), Some("gif"));
        assert_eq!(
            extension_for_image_mime_type("image/png; charset=binary"),
            Some("png")
        );
        assert_eq!(extension_for_image_mime_type("image/bmp"), None);
        assert_eq!(extension_for_image_mime_type("text/plain"), None);
    }

    #[test]
    fn select_preferred_prefers_supported_in_order() {
        // jpeg present alongside png -> png wins (earlier in preference list).
        let types = vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "text/plain".to_string(),
        ];
        assert_eq!(
            select_preferred_image_mime_type(&types),
            Some("image/png".to_string())
        );
    }

    #[test]
    fn select_preferred_returns_raw_type() {
        // The raw (trimmed) form is returned, not the normalized base.
        let types = vec!["  image/PNG  ".to_string()];
        assert_eq!(
            select_preferred_image_mime_type(&types),
            Some("image/PNG".to_string())
        );
    }

    #[test]
    fn select_preferred_falls_back_to_any_image() {
        let types = vec!["text/plain".to_string(), "image/bmp".to_string()];
        assert_eq!(
            select_preferred_image_mime_type(&types),
            Some("image/bmp".to_string())
        );
    }

    #[test]
    fn select_preferred_none_when_no_image() {
        let types = vec!["text/plain".to_string(), "text/html".to_string()];
        assert_eq!(select_preferred_image_mime_type(&types), None);
    }

    #[test]
    fn is_supported_checks_base_type() {
        assert!(is_supported_image_mime_type("image/png"));
        assert!(is_supported_image_mime_type("image/jpeg; foo=bar"));
        assert!(!is_supported_image_mime_type("image/bmp"));
    }

    #[test]
    fn is_wayland_session_detects_env() {
        assert!(is_wayland_session(&env_of(&[(
            "WAYLAND_DISPLAY",
            "wayland-0"
        )])));
        assert!(is_wayland_session(&env_of(&[(
            "XDG_SESSION_TYPE",
            "wayland"
        )])));
        assert!(!is_wayland_session(&env_of(&[("WAYLAND_DISPLAY", "")])));
        assert!(!is_wayland_session(&env_of(&[("XDG_SESSION_TYPE", "x11")])));
        assert!(!is_wayland_session(&env_of(&[])));
    }

    #[test]
    fn is_wsl_detects_env_vars() {
        assert!(is_wsl(&env_of(&[("WSL_DISTRO_NAME", "Ubuntu")])));
        assert!(is_wsl(&env_of(&[("WSLENV", "PATH/l")])));
    }

    #[test]
    fn parse_type_lines_trims_and_filters() {
        let stdout = b"image/png\r\n  image/jpeg  \n\n text/plain \n";
        assert_eq!(
            parse_type_lines(stdout),
            vec![
                "image/png".to_string(),
                "image/jpeg".to_string(),
                "text/plain".to_string(),
            ]
        );
    }

    #[test]
    fn strategies_non_linux_uses_native_only() {
        let env = env_of(&[]);
        assert_eq!(
            clipboard_read_strategies("darwin", &env, false),
            vec![ReadStrategy::Native]
        );
        assert_eq!(
            clipboard_read_strategies("win32", &env, false),
            vec![ReadStrategy::Native]
        );
    }

    #[test]
    fn strategies_linux_wayland() {
        let env = env_of(&[("WAYLAND_DISPLAY", "wayland-0")]);
        assert_eq!(
            clipboard_read_strategies("linux", &env, false),
            vec![ReadStrategy::WlPaste, ReadStrategy::Xclip]
        );
    }

    #[test]
    fn strategies_linux_plain_x11() {
        // Not Wayland, not WSL: native (deferred) then xclip.
        let env = env_of(&[("XDG_SESSION_TYPE", "x11")]);
        assert_eq!(
            clipboard_read_strategies("linux", &env, false),
            vec![ReadStrategy::Native, ReadStrategy::Xclip]
        );
    }

    #[test]
    fn strategies_linux_wsl_not_wayland() {
        // Faithful to pi: wl-paste, xclip, powershell, then native + xclip
        // again (the repeat is intentional).
        let env = env_of(&[("WSL_DISTRO_NAME", "Ubuntu")]);
        assert_eq!(
            clipboard_read_strategies("linux", &env, true),
            vec![
                ReadStrategy::WlPaste,
                ReadStrategy::Xclip,
                ReadStrategy::PowerShell,
                ReadStrategy::Native,
                ReadStrategy::Xclip,
            ]
        );
    }

    #[test]
    fn strategies_linux_wsl_and_wayland() {
        let env = env_of(&[
            ("WSL_DISTRO_NAME", "Ubuntu"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ]);
        assert_eq!(
            clipboard_read_strategies("linux", &env, true),
            vec![
                ReadStrategy::WlPaste,
                ReadStrategy::Xclip,
                ReadStrategy::PowerShell,
            ]
        );
    }

    #[test]
    fn strategies_termux_is_empty() {
        let env = env_of(&[
            ("TERMUX_VERSION", "0.118.0"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ]);
        assert!(clipboard_read_strategies("linux", &env, false).is_empty());
    }

    #[test]
    fn temp_file_extension_matches_pi_for_supported() {
        assert_eq!(temp_file_extension("image/png"), "png");
        assert_eq!(temp_file_extension("image/jpeg"), "jpg");
        assert_eq!(temp_file_extension("image/webp"), "webp");
        assert_eq!(temp_file_extension("image/gif"), "gif");
    }

    #[test]
    fn temp_file_extension_derives_subtype_for_unsupported() {
        // Deferred-conversion path: keep the true extension so a BMP is not
        // written under a misleading .png name.
        assert_eq!(temp_file_extension("image/bmp"), "bmp");
        assert_eq!(temp_file_extension("image/tiff"), "tiff");
        // Nothing sensible to derive -> pi's "png" fallback.
        assert_eq!(temp_file_extension("application/octet-stream"), "png");
    }

    #[test]
    fn write_temp_file_uses_expected_shape() {
        let image = ClipboardImage {
            bytes: b"\x89PNG\r\n\x1a\nfake-bytes".to_vec(),
            mime_type: "image/png".to_string(),
        };
        let path = write_clipboard_image_to_temp_file(&image).expect("write temp file");

        assert_eq!(path.parent().unwrap(), std::env::temp_dir());
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("pi-clipboard-"), "unexpected name: {name}");
        assert!(name.ends_with(".png"), "unexpected name: {name}");

        let written = fs::read(&path).expect("read back");
        assert_eq!(written, image.bytes);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_temp_file_uses_derived_extension_for_unsupported() {
        let image = ClipboardImage {
            bytes: b"BMfake".to_vec(),
            mime_type: "image/bmp".to_string(),
        };
        let path = write_clipboard_image_to_temp_file(&image).expect("write temp file");
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.ends_with(".bmp"), "unexpected name: {name}");
        let _ = fs::remove_file(&path);
    }
}
