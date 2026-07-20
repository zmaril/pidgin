//! Locate and provision the external CLI tools pi shells out to (`fd`, `rg`).
//!
//! Ported from pi's `utils/tools-manager.ts`. pi resolves a tool by checking a
//! managed bin directory, then the system `PATH`, and — when neither has it —
//! downloads the matching GitHub release archive for the host os/arch and
//! extracts the binary into the bin directory.
//!
//! This module ports the *pure decision logic* faithfully: os/arch/platform
//! detection and the string mapping pi's asset-name closures expect,
//! release-URL construction, asset/filename selection, archive layout and
//! extract-path resolution, the offline/Termux gates, and the bin-dir-driven
//! local path. The effectful steps (GitHub HTTP, file download, subprocess
//! extraction, `command --version` probing) live behind the [`ToolProvisioner`]
//! trait seam; see its doc comment for ownership facts and the deferred
//! concrete implementation.
//!
//! ## Delta from pi
//!
//! pi reads `platform()`/`arch()` from Node (`process.platform`/`process.arch`,
//! e.g. `"darwin"`/`"win32"` and `"arm64"`/`"x64"`). Rust's
//! `std::env::consts::{OS, ARCH}` use different spellings (`"macos"`/`"windows"`
//! and `"aarch64"`/`"x86_64"`), so [`platform`] and [`arch`] map the host back
//! to pi's Node-style strings. Every downstream comparison then matches pi
//! byte-for-byte (`plat == "darwin"`, `architecture == "arm64"`, and so on).

// straitjacket-allow-file:duplication

use std::path::{Path, PathBuf};

/// Application name pi interpolates into the GitHub `User-Agent`. pi imports
/// this from `../config.ts` (default `"pi"`). The full `config` module is not
/// yet ported, so it is inlined here — matching how `core::slash_commands`
/// seams the same constant. Replace with the real accessor when `config` lands.
pub const APP_NAME: &str = "pi";

/// Network timeout pi applies to the GitHub "latest release" request
/// (`NETWORK_TIMEOUT_MS`).
pub const NETWORK_TIMEOUT_MS: u64 = 10_000;

/// Download timeout pi applies to the archive fetch (`DOWNLOAD_TIMEOUT_MS`).
pub const DOWNLOAD_TIMEOUT_MS: u64 = 120_000;

/// The tools pi knows how to provision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `sharkdp/fd`.
    Fd,
    /// `BurntSushi/ripgrep` (invoked as `rg`).
    Rg,
}

/// Static description of a provisionable tool, mirroring pi's `ToolConfig`
/// (minus the `getAssetName` closure, which is ported as the free function
/// [`get_asset_name`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolConfig {
    /// Human-facing name used in log lines (pi's `name`).
    pub name: &'static str,
    /// GitHub repository, e.g. `"sharkdp/fd"`.
    pub repo: &'static str,
    /// Name of the binary inside the archive.
    pub binary_name: &'static str,
    /// Alternative system command names to try before downloading. pi defaults
    /// this to `[binary_name]` when absent; here an empty slice carries the
    /// same meaning (see [`system_binary_names`]).
    pub system_binary_names: &'static [&'static str],
    /// Prefix for release tags (`"v"` for `v1.0.0`, `""` for `1.0.0`).
    pub tag_prefix: &'static str,
}

/// Configuration for a [`Tool`], mirroring pi's `TOOLS` record.
pub fn tool_config(tool: Tool) -> ToolConfig {
    match tool {
        Tool::Fd => ToolConfig {
            name: "fd",
            repo: "sharkdp/fd",
            binary_name: "fd",
            system_binary_names: &["fd", "fdfind"],
            tag_prefix: "v",
        },
        Tool::Rg => ToolConfig {
            name: "ripgrep",
            repo: "BurntSushi/ripgrep",
            binary_name: "rg",
            system_binary_names: &[],
            tag_prefix: "",
        },
    }
}

/// System command names to probe, in order, before downloading. Mirrors pi's
/// `config.systemBinaryNames ?? [config.binaryName]`.
pub fn system_binary_names(config: &ToolConfig) -> Vec<&'static str> {
    if config.system_binary_names.is_empty() {
        vec![config.binary_name]
    } else {
        config.system_binary_names.to_vec()
    }
}

/// Host platform in pi's Node `process.platform` spelling
/// (`"darwin"`/`"linux"`/`"win32"`/`"android"`/…). See the module delta note.
pub fn platform() -> String {
    map_platform(std::env::consts::OS)
}

/// Map a Rust `std::env::consts::OS` value to pi's Node platform string.
pub fn map_platform(os: &str) -> String {
    match os {
        "macos" => "darwin".to_string(),
        "windows" => "win32".to_string(),
        other => other.to_string(),
    }
}

/// Host architecture in pi's Node `process.arch` spelling
/// (`"arm64"`/`"x64"`/`"ia32"`/…). See the module delta note.
pub fn arch() -> String {
    map_arch(std::env::consts::ARCH)
}

/// Map a Rust `std::env::consts::ARCH` value to pi's Node arch string.
pub fn map_arch(arch: &str) -> String {
    match arch {
        "aarch64" => "arm64".to_string(),
        "x86_64" => "x64".to_string(),
        "x86" => "ia32".to_string(),
        other => other.to_string(),
    }
}

/// Whether pi's offline mode is enabled. Mirrors `isOfflineModeEnabled`'s
/// early-return shape: an unset or empty `PI_OFFLINE` is `false`; otherwise the
/// value is truthy for `"1"`, or `"true"`/`"yes"` case-insensitively.
pub fn is_offline_mode_enabled() -> bool {
    let value = std::env::var("PI_OFFLINE").unwrap_or_default();
    if value.is_empty() {
        return false;
    }
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

/// Termux package name to suggest on Android, mirroring pi's `TERMUX_PACKAGES`
/// (falling back to the tool key, as pi's `?? tool` does).
pub fn termux_package(tool: Tool) -> &'static str {
    match tool {
        Tool::Fd => "fd",
        Tool::Rg => "ripgrep",
    }
}

/// Architecture token used by both asset-name schemes: `aarch64` for arm64,
/// `x86_64` otherwise. Mirrors the inline `archStr` ternary.
fn arch_str(architecture: &str) -> &'static str {
    if architecture == "arm64" {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// Resolve the release asset filename for a tool on a platform/arch, or `None`
/// for an unsupported platform. Faithful port of each tool's `getAssetName`.
pub fn get_asset_name(tool: Tool, version: &str, plat: &str, architecture: &str) -> Option<String> {
    match tool {
        Tool::Fd => {
            let arch_token = arch_str(architecture);
            match plat {
                "darwin" => Some(format!("fd-v{version}-{arch_token}-apple-darwin.tar.gz")),
                "linux" => Some(format!(
                    "fd-v{version}-{arch_token}-unknown-linux-gnu.tar.gz"
                )),
                "win32" => Some(format!("fd-v{version}-{arch_token}-pc-windows-msvc.zip")),
                _ => None,
            }
        }
        Tool::Rg => match plat {
            "darwin" => {
                let arch_token = arch_str(architecture);
                Some(format!(
                    "ripgrep-{version}-{arch_token}-apple-darwin.tar.gz"
                ))
            }
            "linux" => {
                if architecture == "arm64" {
                    Some(format!(
                        "ripgrep-{version}-aarch64-unknown-linux-gnu.tar.gz"
                    ))
                } else {
                    Some(format!(
                        "ripgrep-{version}-x86_64-unknown-linux-musl.tar.gz"
                    ))
                }
            }
            "win32" => {
                let arch_token = arch_str(architecture);
                Some(format!(
                    "ripgrep-{version}-{arch_token}-pc-windows-msvc.zip"
                ))
            }
            _ => None,
        },
    }
}

/// Apply pi's version pin: `fd` on darwin/x64 is forced to `10.3.0` regardless
/// of the fetched latest. Mirrors the `if (tool === "fd" && plat === "darwin"
/// && architecture === "x64")` override in `downloadTool`.
pub fn apply_version_override(tool: Tool, version: &str, plat: &str, architecture: &str) -> String {
    if tool == Tool::Fd && plat == "darwin" && architecture == "x64" {
        "10.3.0".to_string()
    } else {
        version.to_string()
    }
}

/// GitHub "latest release" API URL for a repo (`getLatestVersion`).
pub fn latest_release_api_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

/// `User-Agent` pi sends with the latest-release request:
/// `` `${APP_NAME}-coding-agent` ``.
pub fn github_user_agent() -> String {
    format!("{APP_NAME}-coding-agent")
}

/// Strip a single leading `v` from a release tag, mirroring
/// `data.tag_name.replace(/^v/, "")`.
pub fn strip_leading_v(tag: &str) -> String {
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

/// Download URL for a release asset:
/// `https://github.com/{repo}/releases/download/{tagPrefix}{version}/{asset}`.
pub fn download_url(config: &ToolConfig, version: &str, asset_name: &str) -> String {
    format!(
        "https://github.com/{}/releases/download/{}{}/{}",
        config.repo, config.tag_prefix, version, asset_name
    )
}

/// Executable extension for a platform: `.exe` on Windows, empty elsewhere.
pub fn binary_ext(plat: &str) -> &'static str {
    if plat == "win32" {
        ".exe"
    } else {
        ""
    }
}

/// Filename of the binary inside the archive, including the platform extension
/// (`config.binaryName + binaryExt`).
pub fn binary_file_name(config: &ToolConfig, plat: &str) -> String {
    format!("{}{}", config.binary_name, binary_ext(plat))
}

/// Managed local path for a tool inside `tools_dir`
/// (`join(TOOLS_DIR, binaryName + ext)`), the first place [`get_tool_path`]
/// logic checks. `tools_dir` is pi's `getBinDir()`; this crate's replica lives
/// in [`crate::utils::shell::get_bin_dir`].
pub fn local_tool_path(tools_dir: &Path, config: &ToolConfig, plat: &str) -> PathBuf {
    tools_dir.join(binary_file_name(config, plat))
}

/// The archive layouts pi extracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// `*.tar.gz`.
    TarGz,
    /// `*.zip`.
    Zip,
}

impl ArchiveKind {
    /// Classify by suffix, mirroring the `endsWith(".tar.gz")` /
    /// `endsWith(".zip")` dispatch. `None` for an unsupported format.
    pub fn from_asset_name(asset_name: &str) -> Option<ArchiveKind> {
        if asset_name.ends_with(".tar.gz") {
            Some(ArchiveKind::TarGz)
        } else if asset_name.ends_with(".zip") {
            Some(ArchiveKind::Zip)
        } else {
            None
        }
    }
}

/// Strip a trailing `.tar.gz` or `.zip`, mirroring
/// `assetName.replace(/\.(tar\.gz|zip)$/, "")`.
pub fn strip_archive_extension(asset_name: &str) -> String {
    if let Some(stem) = asset_name.strip_suffix(".tar.gz") {
        stem.to_string()
    } else if let Some(stem) = asset_name.strip_suffix(".zip") {
        stem.to_string()
    } else {
        asset_name.to_string()
    }
}

/// Ordered candidate paths for the extracted binary, mirroring pi's
/// `extractedBinaryCandidates`: first the versioned subdirectory
/// (`extractDir/<asset-stem>/<binary>`), then the archive root
/// (`extractDir/<binary>`). If neither exists, pi falls back to a recursive
/// search — an effectful step owned by the [`ToolProvisioner`] seam.
pub fn extracted_binary_candidates(
    extract_dir: &Path,
    asset_name: &str,
    binary_file_name: &str,
) -> Vec<PathBuf> {
    let extracted_dir = extract_dir.join(strip_archive_extension(asset_name));
    vec![
        extracted_dir.join(binary_file_name),
        extract_dir.join(binary_file_name),
    ]
}

/// Unique per-download extraction directory name, mirroring pi's
/// `extract_tmp_{binaryName}_{pid}_{Date.now()}_{random}`. The caller supplies
/// the volatile pid/timestamp/random components so this stays pure and testable
/// (pi builds them inline from `process.pid`, `Date.now()`, `Math.random`).
pub fn extract_dir_name(binary_name: &str, pid: u32, timestamp_ms: u128, random: &str) -> String {
    format!("extract_tmp_{binary_name}_{pid}_{timestamp_ms}_{random}")
}

/// Path pi prefers for `tar.exe` on Windows: `{SystemRoot}/System32/tar.exe`.
/// pi returns this only when the file exists (an effectful check owned by the
/// seam), otherwise falling back to the bare `"tar.exe"` on `PATH`.
pub fn windows_system_tar_path(system_root: &str) -> PathBuf {
    Path::new(system_root).join("System32").join("tar.exe")
}

/// Fallback Windows tar command when the System32 binary is absent.
pub const WINDOWS_TAR_FALLBACK: &str = "tar.exe";

/// PowerShell script pi runs as a zip-extraction fallback on Windows.
pub const WINDOWS_POWERSHELL_EXTRACT_SCRIPT: &str = "& { param($archive, $destination) $ErrorActionPreference = 'Stop'; Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force }";

/// One extraction attempt: a command and its arguments. pi tries these in order
/// and stops at the first success (`runExtractionCommand` returning no failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionAttempt {
    /// Command to spawn.
    pub command: String,
    /// Owned argument vector — shaped for
    /// [`crate::utils::child_process::spawn_process_sync`], whose `args` is
    /// `&[String]`.
    pub args: Vec<String>,
}

/// Ordered extraction plan for an archive on a platform, mirroring
/// `extractTarGzArchive` / `extractZipArchive`.
///
/// - `tar.gz` (any platform): `tar xzf <archive> -C <dir>`.
/// - `zip` on `win32`: the resolved Windows tar (`xf`), then a PowerShell
///   `Expand-Archive` fallback.
/// - `zip` elsewhere: `unzip -q`, then `tar xf` as a fallback.
///
/// `windows_tar` is the command pi resolved via [`windows_system_tar_path`] /
/// [`WINDOWS_TAR_FALLBACK`]; it is ignored off Windows.
pub fn extraction_plan(
    kind: ArchiveKind,
    plat: &str,
    archive_path: &Path,
    extract_dir: &Path,
    windows_tar: &str,
) -> Vec<ExtractionAttempt> {
    let archive = archive_path.to_string_lossy().into_owned();
    let dir = extract_dir.to_string_lossy().into_owned();

    match kind {
        ArchiveKind::TarGz => vec![ExtractionAttempt {
            command: "tar".to_string(),
            args: vec!["xzf".to_string(), archive, "-C".to_string(), dir],
        }],
        ArchiveKind::Zip if plat == "win32" => vec![
            ExtractionAttempt {
                command: windows_tar.to_string(),
                args: vec![
                    "xf".to_string(),
                    archive.clone(),
                    "-C".to_string(),
                    dir.clone(),
                ],
            },
            ExtractionAttempt {
                command: "powershell.exe".to_string(),
                args: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-Command".to_string(),
                    WINDOWS_POWERSHELL_EXTRACT_SCRIPT.to_string(),
                    archive,
                    dir,
                ],
            },
        ],
        ArchiveKind::Zip => vec![
            ExtractionAttempt {
                command: "unzip".to_string(),
                args: vec![
                    "-q".to_string(),
                    archive.clone(),
                    "-d".to_string(),
                    dir.clone(),
                ],
            },
            ExtractionAttempt {
                command: "tar".to_string(),
                args: vec!["xf".to_string(), archive, "-C".to_string(), dir],
            },
        ],
    }
}

// SEAM: effectful provisioning operations tools-manager needs — GitHub HTTP,
// file download, subprocess extraction, and `command --version` probing. The
// pure logic above decides *what* to fetch/run; this trait performs it. Kept
// local (not a shared abstraction) and intentionally unimplemented in this PR.
//
// Ownership / wiring facts for the deferred concrete implementation:
//
// - DOWNLOAD is owned by THIS lane. There is NO HTTP download in the shared
//   `core::http_dispatcher` — that module is sync timeout math only
//   (`DEFAULT_HTTP_IDLE_TIMEOUT_MS`, `parse_http_idle_timeout_ms`/format). The
//   concrete `fetch_latest_version`/`download_file` will be a reqwest GET
//   configured with `pidgin_coding::core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS`;
//   this does NOT realign onto any http-dispatcher download trait. Real
//   networking is a deferred follow-up PR, not implemented here.
// - EXTRACT/spawn is already landed: `crate::utils::child_process::spawn_process_sync`
//   (`spawn_process_sync(command: &str, args: &[String], configure) -> io::Result<std::process::Output>`,
//   args owned). The concrete `command_exists`/`run_extraction` will wire each
//   `ExtractionAttempt` to
//   `child_process::spawn_process_sync(&attempt.command, &attempt.args, |_| {})`
//   in the follow-up wiring PR.
/// Effectful seam for tool provisioning (see the `SEAM:` comment above).
pub trait ToolProvisioner {
    /// Fetch the latest release tag for `repo` from GitHub, sending
    /// `user_agent`. The raw tag should be normalized with [`strip_leading_v`].
    fn fetch_latest_version(&self, repo: &str, user_agent: &str) -> std::io::Result<String>;

    /// Download `url` to `dest`.
    fn download_file(&self, url: &str, dest: &Path) -> std::io::Result<()>;

    /// Whether `command --version` runs without a spawn error (pi's
    /// `commandExists`).
    fn command_exists(&self, command: &str) -> bool;

    /// Run one [`ExtractionAttempt`]. Returns `None` on success, or
    /// `Some(message)` describing the failure (pi's `runExtractionCommand`).
    fn run_extraction(&self, attempt: &ExtractionAttempt) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_and_arch_mapping() {
        assert_eq!(map_platform("macos"), "darwin");
        assert_eq!(map_platform("windows"), "win32");
        assert_eq!(map_platform("linux"), "linux");
        assert_eq!(map_platform("android"), "android");
        assert_eq!(map_platform("freebsd"), "freebsd");

        assert_eq!(map_arch("aarch64"), "arm64");
        assert_eq!(map_arch("x86_64"), "x64");
        assert_eq!(map_arch("x86"), "ia32");
        assert_eq!(map_arch("powerpc64"), "powerpc64");
    }

    #[test]
    fn fd_asset_names_per_platform() {
        assert_eq!(
            get_asset_name(Tool::Fd, "9.0.0", "darwin", "arm64").as_deref(),
            Some("fd-v9.0.0-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(
            get_asset_name(Tool::Fd, "9.0.0", "darwin", "x64").as_deref(),
            Some("fd-v9.0.0-x86_64-apple-darwin.tar.gz")
        );
        assert_eq!(
            get_asset_name(Tool::Fd, "9.0.0", "linux", "arm64").as_deref(),
            Some("fd-v9.0.0-aarch64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            get_asset_name(Tool::Fd, "9.0.0", "linux", "x64").as_deref(),
            Some("fd-v9.0.0-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            get_asset_name(Tool::Fd, "9.0.0", "win32", "x64").as_deref(),
            Some("fd-v9.0.0-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(get_asset_name(Tool::Fd, "9.0.0", "android", "arm64"), None);
    }

    #[test]
    fn rg_asset_names_per_platform() {
        assert_eq!(
            get_asset_name(Tool::Rg, "14.1.0", "darwin", "arm64").as_deref(),
            Some("ripgrep-14.1.0-aarch64-apple-darwin.tar.gz")
        );
        // linux arm64 uses gnu; other linux uses musl.
        assert_eq!(
            get_asset_name(Tool::Rg, "14.1.0", "linux", "arm64").as_deref(),
            Some("ripgrep-14.1.0-aarch64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            get_asset_name(Tool::Rg, "14.1.0", "linux", "x64").as_deref(),
            Some("ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz")
        );
        assert_eq!(
            get_asset_name(Tool::Rg, "14.1.0", "win32", "arm64").as_deref(),
            Some("ripgrep-14.1.0-aarch64-pc-windows-msvc.zip")
        );
        assert_eq!(get_asset_name(Tool::Rg, "14.1.0", "freebsd", "x64"), None);
    }

    #[test]
    fn fd_darwin_x64_version_pin() {
        assert_eq!(
            apply_version_override(Tool::Fd, "10.9.9", "darwin", "x64"),
            "10.3.0"
        );
        // Any other combination is left untouched.
        assert_eq!(
            apply_version_override(Tool::Fd, "10.9.9", "darwin", "arm64"),
            "10.9.9"
        );
        assert_eq!(
            apply_version_override(Tool::Fd, "10.9.9", "linux", "x64"),
            "10.9.9"
        );
        assert_eq!(
            apply_version_override(Tool::Rg, "14.1.0", "darwin", "x64"),
            "14.1.0"
        );
    }

    #[test]
    fn download_url_construction() {
        // fd tag prefix "v".
        let fd = tool_config(Tool::Fd);
        assert_eq!(
            download_url(&fd, "9.0.0", "fd-v9.0.0-x86_64-unknown-linux-gnu.tar.gz"),
            "https://github.com/sharkdp/fd/releases/download/v9.0.0/fd-v9.0.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        // rg tag prefix "".
        let rg = tool_config(Tool::Rg);
        assert_eq!(
            download_url(&rg, "14.1.0", "ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz"),
            "https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz"
        );
    }

    #[test]
    fn api_url_user_agent_and_tag_normalization() {
        assert_eq!(
            latest_release_api_url("sharkdp/fd"),
            "https://api.github.com/repos/sharkdp/fd/releases/latest"
        );
        assert_eq!(github_user_agent(), "pi-coding-agent");
        assert_eq!(strip_leading_v("v9.0.0"), "9.0.0");
        assert_eq!(strip_leading_v("14.1.0"), "14.1.0");
        assert_eq!(strip_leading_v("vvv"), "vv");
    }

    #[test]
    fn binary_ext_and_local_path() {
        assert_eq!(binary_ext("win32"), ".exe");
        assert_eq!(binary_ext("linux"), "");

        let fd = tool_config(Tool::Fd);
        assert_eq!(binary_file_name(&fd, "win32"), "fd.exe");
        assert_eq!(binary_file_name(&fd, "linux"), "fd");

        let tools_dir = Path::new("/home/user/.pi/agent/bin");
        assert_eq!(
            local_tool_path(tools_dir, &fd, "linux"),
            PathBuf::from("/home/user/.pi/agent/bin/fd")
        );
        assert_eq!(
            local_tool_path(tools_dir, &fd, "win32"),
            PathBuf::from("/home/user/.pi/agent/bin/fd.exe")
        );
    }

    #[test]
    fn system_binary_name_defaulting() {
        // fd declares explicit alternatives.
        assert_eq!(
            system_binary_names(&tool_config(Tool::Fd)),
            vec!["fd", "fdfind"]
        );
        // rg falls back to its binary name.
        assert_eq!(system_binary_names(&tool_config(Tool::Rg)), vec!["rg"]);
    }

    #[test]
    fn archive_kind_and_extension_stripping() {
        assert_eq!(
            ArchiveKind::from_asset_name("fd-v9.0.0-x86_64-unknown-linux-gnu.tar.gz"),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(
            ArchiveKind::from_asset_name("fd-v9.0.0-x86_64-pc-windows-msvc.zip"),
            Some(ArchiveKind::Zip)
        );
        assert_eq!(ArchiveKind::from_asset_name("fd-v9.0.0.deb"), None);

        assert_eq!(
            strip_archive_extension("ripgrep-14.1.0-x86_64-apple-darwin.tar.gz"),
            "ripgrep-14.1.0-x86_64-apple-darwin"
        );
        assert_eq!(
            strip_archive_extension("fd-v9.0.0-x86_64-pc-windows-msvc.zip"),
            "fd-v9.0.0-x86_64-pc-windows-msvc"
        );
        assert_eq!(strip_archive_extension("plain"), "plain");
    }

    #[test]
    fn extracted_candidate_paths() {
        let extract_dir = Path::new("/tmp/extract_tmp_rg_1_2_abc");
        let candidates = extracted_binary_candidates(
            extract_dir,
            "ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz",
            "rg",
        );
        assert_eq!(
            candidates,
            vec![
                PathBuf::from(
                    "/tmp/extract_tmp_rg_1_2_abc/ripgrep-14.1.0-x86_64-unknown-linux-musl/rg"
                ),
                PathBuf::from("/tmp/extract_tmp_rg_1_2_abc/rg"),
            ]
        );
    }

    #[test]
    fn extract_dir_naming() {
        assert_eq!(
            extract_dir_name("fd", 4242, 1_700_000_000_000, "a1b2c3d4"),
            "extract_tmp_fd_4242_1700000000000_a1b2c3d4"
        );
    }

    #[test]
    fn windows_tar_path_construction() {
        assert_eq!(
            windows_system_tar_path("C:\\Windows"),
            PathBuf::from("C:\\Windows")
                .join("System32")
                .join("tar.exe")
        );
        assert_eq!(WINDOWS_TAR_FALLBACK, "tar.exe");
    }

    #[test]
    fn tar_gz_extraction_plan() {
        let plan = extraction_plan(
            ArchiveKind::TarGz,
            "linux",
            Path::new("/tools/a.tar.gz"),
            Path::new("/tools/out"),
            WINDOWS_TAR_FALLBACK,
        );
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].command, "tar");
        assert_eq!(
            plan[0].args,
            vec![
                "xzf".to_string(),
                "/tools/a.tar.gz".to_string(),
                "-C".to_string(),
                "/tools/out".to_string()
            ]
        );
    }

    #[test]
    fn zip_extraction_plan_unix() {
        let plan = extraction_plan(
            ArchiveKind::Zip,
            "linux",
            Path::new("/tools/a.zip"),
            Path::new("/tools/out"),
            WINDOWS_TAR_FALLBACK,
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].command, "unzip");
        assert_eq!(
            plan[0].args,
            vec![
                "-q".to_string(),
                "/tools/a.zip".to_string(),
                "-d".to_string(),
                "/tools/out".to_string()
            ]
        );
        assert_eq!(plan[1].command, "tar");
        assert_eq!(
            plan[1].args,
            vec![
                "xf".to_string(),
                "/tools/a.zip".to_string(),
                "-C".to_string(),
                "/tools/out".to_string()
            ]
        );
    }

    #[test]
    fn zip_extraction_plan_windows() {
        let plan = extraction_plan(
            ArchiveKind::Zip,
            "win32",
            Path::new("C:\\tools\\a.zip"),
            Path::new("C:\\tools\\out"),
            "C:\\Windows\\System32\\tar.exe",
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].command, "C:\\Windows\\System32\\tar.exe");
        assert_eq!(plan[0].args[0], "xf");
        assert_eq!(plan[1].command, "powershell.exe");
        // The PowerShell fallback ends with archive then destination.
        assert_eq!(plan[1].args[6], WINDOWS_POWERSHELL_EXTRACT_SCRIPT);
        assert_eq!(plan[1].args[7], "C:\\tools\\a.zip");
        assert_eq!(plan[1].args[8], "C:\\tools\\out");
    }

    #[test]
    fn termux_package_names() {
        assert_eq!(termux_package(Tool::Fd), "fd");
        assert_eq!(termux_package(Tool::Rg), "ripgrep");
    }
}
