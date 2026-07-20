//! Path normalization, resolution, and relativization helpers.
//!
//! Ported from pi's `utils/paths.ts`. Covers the pure path logic plus
//! `canonicalize_path` (a realpath wrapper with a raw fallback). pi's
//! `markPathIgnoredByCloudSync` shells out to `xattr`/`setfattr` and is
//! intentionally not ported.
//!
//! The lexical helpers here implement POSIX (`/`-separated) semantics matching
//! Node's `path` module on Unix, which is what pi's tests exercise. Windows
//! backslash handling and drive-letter URLs are out of scope.

use regex::Regex;
use std::sync::OnceLock;

/// Error returned when a `file://` URL cannot be converted to a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError(pub String);

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PathError {}

/// Options controlling [`normalize_path`].
#[derive(Debug, Clone)]
pub struct PathInputOptions {
    /// Trim leading/trailing whitespace before normalization.
    pub trim: bool,
    /// Expand a leading `~` to a home directory. Defaults to `true`.
    pub expand_tilde: bool,
    /// Home directory used for `~` expansion. Defaults to `$HOME`.
    pub home_dir: Option<String>,
    /// Strip a leading `@`, used for CLI `@file` paths.
    pub strip_at_prefix: bool,
    /// Normalize unicode space variants to regular spaces.
    pub normalize_unicode_spaces: bool,
}

impl Default for PathInputOptions {
    fn default() -> Self {
        Self {
            trim: false,
            expand_tilde: true,
            home_dir: None,
            strip_at_prefix: false,
            normalize_unicode_spaces: false,
        }
    }
}

fn unicode_spaces_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new("[\\u{00A0}\\u{2000}-\\u{200A}\\u{202F}\\u{205F}\\u{3000}]")
            .expect("valid unicode-spaces regex")
    })
}

fn home_directory() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Lexically normalize a POSIX path, resolving `.` and `..` without touching
/// the filesystem (mirrors `path.posix.normalize` for resolved paths).
fn posix_normalize(path: &str) -> String {
    super::bytes::posix_normalize(path, false)
}

/// Resolve a single path against the current directory, like `path.resolve`.
fn resolve_one(path: &str) -> String {
    if path.starts_with('/') {
        posix_normalize(path)
    } else {
        posix_normalize(&format!("{}/{}", current_dir(), path))
    }
}

/// Resolve `path` against `base`, like `path.resolve(base, path)`.
fn resolve_two(base: &str, path: &str) -> String {
    if path.starts_with('/') {
        return posix_normalize(path);
    }
    let combined = format!("{base}/{path}");
    if base.starts_with('/') {
        posix_normalize(&combined)
    } else {
        posix_normalize(&format!("{}/{}", current_dir(), combined))
    }
}

/// Compute a relative path from `from` to `to`, like `path.relative` for two
/// already-absolute POSIX paths.
fn posix_relative(from: &str, to: &str) -> String {
    let from = posix_normalize(from);
    let to = posix_normalize(to);
    let from_parts: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();

    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }

    let mut out: Vec<&str> = vec![".."; from_parts.len() - common];
    out.extend_from_slice(&to_parts[common..]);
    out.join("/")
}

/// Decode a percent-encoded string into UTF-8, erroring on malformed escapes.
fn percent_decode(input: &str) -> Result<String, PathError> {
    super::bytes::percent_decode(input)
        .ok_or_else(|| PathError(format!("malformed percent-encoding in {input:?}")))
}

/// Convert a `file://` URL to a filesystem path (POSIX semantics).
fn file_url_to_path(url: &str) -> Result<String, PathError> {
    let rest = &url["file://".len()..];
    // An authority (host) component runs until the next `/`.
    let pathname = if rest.starts_with('/') {
        rest
    } else {
        match rest.find('/') {
            Some(slash) => {
                let host = &rest[..slash];
                if !host.is_empty() && host != "localhost" {
                    return Err(PathError(format!("unsupported file URL host in {url:?}")));
                }
                &rest[slash..]
            }
            None => return Err(PathError(format!("invalid file URL {url:?}"))),
        }
    };
    // Strip a query/fragment if present (pathToFileURL never adds them, but be
    // defensive) before percent-decoding.
    let end = pathname.find(['?', '#']).unwrap_or(pathname.len());
    percent_decode(&pathname[..end])
}

/// Resolve a path to its canonical (real) form, following symlinks. Falls back
/// to the raw path when resolution fails (e.g. the target does not exist).
pub fn canonicalize_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// Returns true if the value is NOT a package source (`npm:`, `git:`, etc.) or
/// a remote URL protocol. Bare names, relative paths, and `file:` URLs are
/// considered local.
pub fn is_local_path(value: &str) -> bool {
    let trimmed = value.trim();
    !(trimmed.starts_with("npm:")
        || trimmed.starts_with("git:")
        || trimmed.starts_with("github:")
        || trimmed.starts_with("http:")
        || trimmed.starts_with("https:")
        || trimmed.starts_with("ssh:"))
}

/// Normalize a path input: trimming, unicode-space folding, `@`-prefix
/// stripping, `~` expansion, and `file://` conversion.
pub fn normalize_path(input: &str, options: &PathInputOptions) -> Result<String, PathError> {
    let mut normalized = if options.trim {
        input.trim().to_string()
    } else {
        input.to_string()
    };

    if options.normalize_unicode_spaces {
        normalized = unicode_spaces_regex()
            .replace_all(&normalized, " ")
            .into_owned();
    }

    if options.strip_at_prefix && normalized.starts_with('@') {
        normalized = normalized[1..].to_string();
    }

    if options.expand_tilde {
        let home = options.home_dir.clone().unwrap_or_else(home_directory);
        if normalized == "~" {
            return Ok(home);
        }
        if let Some(rest) = normalized.strip_prefix("~/") {
            return Ok(posix_normalize(&format!("{home}/{rest}")));
        }
    }

    if normalized.starts_with("file://") {
        return file_url_to_path(&normalized);
    }

    Ok(normalized)
}

/// Resolve `input` against `base_dir`, applying [`normalize_path`] first.
pub fn resolve_path(
    input: &str,
    base_dir: &str,
    options: &PathInputOptions,
) -> Result<String, PathError> {
    let normalized = normalize_path(input, options)?;
    let normalized_base = normalize_path(base_dir, &PathInputOptions::default())?;
    if normalized.starts_with('/') {
        Ok(resolve_one(&normalized))
    } else {
        Ok(resolve_two(&normalized_base, &normalized))
    }
}

/// Return the path of `file_path` relative to `cwd` if it lies inside `cwd`,
/// otherwise `None`.
pub fn get_cwd_relative_path(file_path: &str, cwd: &str) -> Result<Option<String>, PathError> {
    let base = current_dir();
    let resolved_cwd = resolve_path(cwd, &base, &PathInputOptions::default())?;
    let resolved_path = resolve_path(file_path, &resolved_cwd, &PathInputOptions::default())?;
    let relative_path = posix_relative(&resolved_cwd, &resolved_path);

    let is_inside_cwd = relative_path.is_empty()
        || (relative_path != ".."
            && !relative_path.starts_with("../")
            && !relative_path.starts_with('/'));

    if is_inside_cwd {
        Ok(Some(if relative_path.is_empty() {
            ".".to_string()
        } else {
            relative_path
        }))
    } else {
        Ok(None)
    }
}

/// Format `file_path` relative to `cwd` when it is inside `cwd`, otherwise as an
/// absolute path. Always uses `/` separators.
pub fn format_path_relative_to_cwd_or_absolute(
    file_path: &str,
    cwd: &str,
) -> Result<String, PathError> {
    let absolute_path = resolve_path(file_path, cwd, &PathInputOptions::default())?;
    let relative = get_cwd_relative_path(&absolute_path, cwd)?;
    Ok(relative.unwrap_or(absolute_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("pidgin-paths-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn canonicalize_returns_real_path_for_regular_file() {
        let dir = scratch_dir();
        let file = dir.join("file.txt");
        fs::write(&file, "hello").unwrap();
        let expected = fs::canonicalize(&file)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(canonicalize_path(file.to_str().unwrap()), expected);
        fs::remove_file(&file).ok();
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_resolves_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = scratch_dir();
        let target = dir.join("target-canon.txt");
        let link = dir.join("link-canon.txt");
        fs::write(&target, "hello").unwrap();
        let _ = fs::remove_file(&link);
        symlink(&target, &link).unwrap();
        let expected = fs::canonicalize(&target)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(canonicalize_path(link.to_str().unwrap()), expected);
        fs::remove_file(&link).ok();
        fs::remove_file(&target).ok();
    }

    #[test]
    fn canonicalize_falls_back_when_target_missing() {
        let dir = scratch_dir();
        let nonexistent = dir.join("no-such-file");
        let s = nonexistent.to_str().unwrap();
        assert_eq!(canonicalize_path(s), s);
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_falls_back_for_dangling_symlink() {
        use std::os::unix::fs::symlink;
        let dir = scratch_dir();
        let target = dir.join("dangling-target.txt");
        let link = dir.join("dangling-link.txt");
        let _ = fs::remove_file(&link);
        symlink(&target, &link).unwrap();
        let s = link.to_str().unwrap();
        assert_eq!(canonicalize_path(s), s);
        fs::remove_file(&link).ok();
    }

    #[test]
    fn get_cwd_relative_keeps_dot_prefixed_names() {
        let cwd = "/tmp/pi-paths-cwd";
        let file = "/tmp/pi-paths-cwd/..config/AGENTS.md";
        assert_eq!(
            get_cwd_relative_path(file, cwd).unwrap(),
            Some("..config/AGENTS.md".to_string())
        );
    }

    #[test]
    fn get_cwd_relative_rejects_parent_traversal() {
        let cwd = "/tmp/pi-paths-cwd";
        let file = "/tmp/AGENTS.md";
        assert_eq!(get_cwd_relative_path(file, cwd).unwrap(), None);
    }

    #[test]
    fn expands_only_home_tilde_shortcuts() {
        let cwd = "/tmp/pi-paths-cwd";
        let opts = PathInputOptions::default();
        let home = home_directory();
        assert_eq!(normalize_path("~", &opts).unwrap(), home);
        assert_eq!(
            normalize_path("~/file.txt", &opts).unwrap(),
            posix_normalize(&format!("{home}/file.txt"))
        );
        assert_eq!(
            resolve_path("~draft.md", cwd, &opts).unwrap(),
            "/tmp/pi-paths-cwd/~draft.md"
        );
        assert_eq!(normalize_path("~draft.md", &opts).unwrap(), "~draft.md");
    }

    #[test]
    fn resolves_relative_paths_against_base() {
        let cwd = "/tmp/pi-paths-cwd";
        let opts = PathInputOptions::default();
        assert_eq!(
            resolve_path("subdir/file.txt", cwd, &opts).unwrap(),
            "/tmp/pi-paths-cwd/subdir/file.txt"
        );
        // A file:// base URL is normalized to a path before resolving.
        assert_eq!(
            resolve_path("subdir/file.txt", "file:///tmp/pi-paths-cwd", &opts).unwrap(),
            "/tmp/pi-paths-cwd/subdir/file.txt"
        );
    }

    #[test]
    fn accepts_file_urls_with_encoded_spaces() {
        let opts = PathInputOptions::default();
        let url = "file:///tmp/dir/file%20with%20spaces.txt";
        assert_eq!(
            resolve_path(url, "/tmp/dir/base", &opts).unwrap(),
            "/tmp/dir/file with spaces.txt"
        );
    }

    #[test]
    fn throws_for_invalid_file_urls() {
        let opts = PathInputOptions::default();
        assert!(resolve_path("file:///%E0%A4%A", "/tmp", &opts).is_err());
    }

    #[test]
    fn preserves_absolute_paths_with_literal_percent_sequences() {
        let opts = PathInputOptions::default();
        for path in [
            "/tmp/dir/report%2026.md",
            "/tmp/dir/foo%2Fbar",
            "/tmp/dir/malformed%A.md",
        ] {
            assert_eq!(resolve_path(path, "/tmp/dir/base", &opts).unwrap(), path);
        }
    }

    #[test]
    fn is_local_path_classifies_sources() {
        assert!(is_local_path("my-package"));
        assert!(is_local_path("./foo"));
        assert!(is_local_path("file:///tmp/foo"));
        assert!(!is_local_path("npm:package"));
        assert!(!is_local_path("git://repo"));
        assert!(!is_local_path("https://example.com"));
    }

    #[test]
    fn normalizes_unicode_spaces_and_at_prefix() {
        let opts = PathInputOptions {
            normalize_unicode_spaces: true,
            strip_at_prefix: true,
            ..Default::default()
        };
        assert_eq!(normalize_path("@foo\u{00A0}bar", &opts).unwrap(), "foo bar");
    }

    #[test]
    fn format_path_relative_or_absolute() {
        let cwd = "/tmp/pi-paths-cwd";
        assert_eq!(
            format_path_relative_to_cwd_or_absolute("/tmp/pi-paths-cwd/sub/file.txt", cwd).unwrap(),
            "sub/file.txt"
        );
        assert_eq!(
            format_path_relative_to_cwd_or_absolute("/tmp/other/file.txt", cwd).unwrap(),
            "/tmp/other/file.txt"
        );
    }
}
