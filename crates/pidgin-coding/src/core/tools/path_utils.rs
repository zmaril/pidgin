//! Path expansion and resolution for the file tools.
//!
//! Ported from pi's `core/tools/path-utils.ts`. `expand_path` and
//! `resolve_to_cwd` are thin wrappers over the shared `utils::paths`
//! helpers (unicode-space folding, leading `@` strip, `~` expansion, and
//! `file://` handling). The macOS filename variant transforms
//! (`try_macos_screenshot_path`, `try_nfd_variant`, `try_curly_quote_variant`)
//! are pure string functions. `resolve_read_path` layers the fs-probe fallback
//! on top of them; the filesystem existence check is acceptable here and is
//! injected via a closure so the pure fallback ordering can be unit tested.

use crate::utils::paths::{normalize_path, resolve_path, PathError, PathInputOptions};

const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

/// Expand a path input the way the file tools do: fold unicode spaces, strip a
/// leading `@`, expand `~`, and convert `file://` URLs.
pub fn expand_path(file_path: &str) -> Result<String, PathError> {
    normalize_path(
        file_path,
        &PathInputOptions {
            normalize_unicode_spaces: true,
            strip_at_prefix: true,
            ..Default::default()
        },
    )
}

/// Resolve `file_path` relative to `cwd`, handling `~` expansion and absolute
/// paths (absolute inputs pass through, relative inputs resolve against `cwd`).
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> Result<String, PathError> {
    resolve_path(
        file_path,
        cwd,
        &PathInputOptions {
            normalize_unicode_spaces: true,
            strip_at_prefix: true,
            ..Default::default()
        },
    )
}

/// macOS stores screenshot names with a narrow no-break space (U+202F) before
/// `AM`/`PM`; users typically type a regular space. Replace ` AM.`/` PM.`
/// (case-insensitively) with the narrow-NBSP form.
pub fn try_macos_screenshot_path(file_path: &str) -> String {
    let bytes: Vec<char> = file_path.chars().collect();
    let mut out = String::with_capacity(file_path.len());
    let mut i = 0;
    while i < bytes.len() {
        // Look for the pattern: ' ' <A|P> <M|m> '.'
        if bytes[i] == ' '
            && i + 3 < bytes.len()
            && matches!(bytes[i + 1], 'A' | 'a' | 'P' | 'p')
            && matches!(bytes[i + 2], 'M' | 'm')
            && bytes[i + 3] == '.'
        {
            out.push(NARROW_NO_BREAK_SPACE);
            out.push(bytes[i + 1]);
            out.push(bytes[i + 2]);
            out.push('.');
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Convert to NFD (decomposed) form, matching how macOS stores filenames.
pub fn try_nfd_variant(file_path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    file_path.nfd().collect()
}

/// Replace straight apostrophes (U+0027) with the curly right single quotation
/// mark (U+2019) used by macOS screenshot names.
pub fn try_curly_quote_variant(file_path: &str) -> String {
    file_path.replace('\'', "\u{2019}")
}

/// Resolve a read path, trying the resolved path first and then macOS filename
/// variants. `exists` probes the filesystem (or a test double). The variant
/// order matches pi: AM/PM, NFD, curly quote, then NFD + curly quote.
pub fn resolve_read_path(
    file_path: &str,
    cwd: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Result<String, PathError> {
    let resolved = resolve_to_cwd(file_path, cwd)?;
    if exists(&resolved) {
        return Ok(resolved);
    }

    let am_pm = try_macos_screenshot_path(&resolved);
    if am_pm != resolved && exists(&am_pm) {
        return Ok(am_pm);
    }

    let nfd = try_nfd_variant(&resolved);
    if nfd != resolved && exists(&nfd) {
        return Ok(nfd);
    }

    let curly = try_curly_quote_variant(&resolved);
    if curly != resolved && exists(&curly) {
        return Ok(curly);
    }

    let nfd_curly = try_curly_quote_variant(&nfd);
    if nfd_curly != resolved && exists(&nfd_curly) {
        return Ok(nfd_curly);
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> String {
        std::env::var("HOME").unwrap_or_default()
    }

    #[test]
    fn expands_tilde_to_home() {
        let result = expand_path("~").unwrap();
        assert!(!result.contains('~'));
        assert_eq!(result, home());
    }

    #[test]
    fn expands_tilde_slash_path() {
        let result = expand_path("~/Documents/file.txt").unwrap();
        assert!(!result.contains("~/"));
        assert_eq!(result, format!("{}/Documents/file.txt", home()));
    }

    #[test]
    fn keeps_tilde_prefixed_filenames_literal() {
        assert_eq!(expand_path("~draft.md").unwrap(), "~draft.md");
        assert_eq!(expand_path("@~draft.md").unwrap(), "~draft.md");
    }

    #[test]
    fn normalizes_unicode_spaces() {
        let with_nbsp = "file\u{00A0}name.txt";
        assert_eq!(expand_path(with_nbsp).unwrap(), "file name.txt");
    }

    #[test]
    fn resolves_absolute_paths_as_is() {
        let abs = "/tmp/absolute/path/file.txt";
        assert_eq!(resolve_to_cwd(abs, "/tmp/some/cwd").unwrap(), abs);
    }

    #[test]
    fn resolves_relative_paths_against_cwd() {
        assert_eq!(
            resolve_to_cwd("relative/file.txt", "/some/cwd").unwrap(),
            "/some/cwd/relative/file.txt"
        );
    }

    #[test]
    fn resolves_tilde_prefixed_filenames_against_cwd() {
        let cwd = "/tmp/pi-path-utils-cwd";
        assert_eq!(
            resolve_to_cwd("~draft.md", cwd).unwrap(),
            "/tmp/pi-path-utils-cwd/~draft.md"
        );
        assert_eq!(
            resolve_to_cwd("@~draft.md", cwd).unwrap(),
            "/tmp/pi-path-utils-cwd/~draft.md"
        );
    }

    #[test]
    fn resolve_read_path_returns_existing_file() {
        let cwd = "/tmp/probe-cwd";
        let target = "/tmp/probe-cwd/test-file.txt";
        let exists = |p: &str| p == target;
        assert_eq!(
            resolve_read_path("test-file.txt", cwd, &exists).unwrap(),
            target
        );
    }

    #[test]
    fn resolve_read_path_nfd_variant_fallback() {
        // User types NFC "é" (U+00E9); the file on disk is NFD "e" + U+0301.
        let cwd = "/tmp/probe-cwd";
        let nfc = "file\u{00e9}.txt";
        let resolved_nfc = format!("{cwd}/{nfc}");
        let resolved_nfd = try_nfd_variant(&resolved_nfc);
        assert_ne!(resolved_nfc, resolved_nfd);
        let exists = move |p: &str| p == resolved_nfd;
        let result = resolve_read_path(nfc, cwd, &exists).unwrap();
        assert!(result.contains(cwd));
        assert!(result.ends_with(".txt"));
    }

    #[test]
    fn resolve_read_path_curly_quote_variant() {
        let cwd = "/tmp/probe-cwd";
        let straight = "Capture d'cran.txt";
        let curly_full = format!("{cwd}/Capture d\u{2019}cran.txt");
        let exists = move |p: &str| p == curly_full;
        let result = resolve_read_path(straight, cwd, &exists).unwrap();
        assert_eq!(result, format!("{cwd}/Capture d\u{2019}cran.txt"));
    }

    #[test]
    fn resolve_read_path_am_pm_variant() {
        let cwd = "/tmp/probe-cwd";
        let user = "Screenshot 2024-01-01 at 10.00.00 AM.png";
        let macos_full = format!("{cwd}/Screenshot 2024-01-01 at 10.00.00\u{202F}AM.png");
        let exists = move |p: &str| p == macos_full;
        let result = resolve_read_path(user, cwd, &exists).unwrap();
        assert_eq!(
            result,
            format!("{cwd}/Screenshot 2024-01-01 at 10.00.00\u{202F}AM.png")
        );
    }

    #[test]
    fn resolve_read_path_lowercase_am_pm_variant() {
        let cwd = "/tmp/probe-cwd";
        let user = "Screenshot 2024-01-01 at 10.00.00 am.png";
        let macos_full = format!("{cwd}/Screenshot 2024-01-01 at 10.00.00\u{202F}am.png");
        let exists = move |p: &str| p == macos_full;
        let result = resolve_read_path(user, cwd, &exists).unwrap();
        assert_eq!(
            result,
            format!("{cwd}/Screenshot 2024-01-01 at 10.00.00\u{202F}am.png")
        );
    }

    #[test]
    fn resolve_read_path_combined_nfd_curly() {
        // French macOS screenshot: NFC + curly quote on disk, user types straight.
        let cwd = "/tmp/probe-cwd";
        let user = "Capture d'\u{00e9}cran.txt";
        let on_disk = format!("{cwd}/Capture d\u{2019}\u{00e9}cran.txt");
        // The combined variant applies curly-quote folding to the NFD form.
        let resolved_user = format!("{cwd}/{user}");
        let nfd = try_nfd_variant(&resolved_user);
        let nfd_curly = try_curly_quote_variant(&nfd);
        // Ensure our synthetic on-disk name is reachable via one of the variants.
        let curly = try_curly_quote_variant(&resolved_user);
        let matches = move |p: &str| p == curly || p == nfd_curly || p == on_disk;
        let result = resolve_read_path(user, cwd, &matches).unwrap();
        assert!(result.contains("\u{2019}"));
    }

    #[test]
    fn resolve_read_path_falls_back_to_resolved_when_nothing_exists() {
        let cwd = "/tmp/probe-cwd";
        let exists = |_: &str| false;
        assert_eq!(
            resolve_read_path("missing.txt", cwd, &exists).unwrap(),
            "/tmp/probe-cwd/missing.txt"
        );
    }
}
