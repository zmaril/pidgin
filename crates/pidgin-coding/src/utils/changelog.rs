//! Changelog link normalization and version comparison.
//!
//! Ported from pi's `utils/changelog.ts`. `normalize_changelog_links`
//! canonicalizes legacy `badlogic`/`earendil-works` `pi-mono` repository URLs
//! to `earendil-works/pi`, pins floating `blob`/`tree` `main`/`master` refs to
//! the release tag, and rewrites package-relative links to tag-pinned GitHub
//! source URLs (choosing `tree` for directories and `blob` for files).
//! External and anchor links are left untouched.
//!
//! pi's `parseChangelog` reads a file from disk; this port exposes
//! `parse_changelog_str`, which scans an in-memory string for `## x.y.z`
//! headings instead. `compare_versions` and `get_new_entries` are pure.

use regex::{Captures, Regex};
use std::sync::OnceLock;

const GITHUB_REPO: &str = "earendil-works/pi";
const CHANGELOG_LINK_BASE_PATH: &str = "packages/coding-agent";

/// A single changelog entry: a semantic version and its section content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntry {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub content: String,
}

impl ChangelogEntry {
    fn version(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A version specifier accepted by [`normalize_changelog_links`]: either a
/// raw string or a parsed entry.
pub enum VersionRef<'a> {
    Str(&'a str),
    Entry(&'a ChangelogEntry),
}

impl<'a> From<&'a str> for VersionRef<'a> {
    fn from(value: &'a str) -> Self {
        VersionRef::Str(value)
    }
}

impl<'a> From<&'a ChangelogEntry> for VersionRef<'a> {
    fn from(value: &'a ChangelogEntry) -> Self {
        VersionRef::Entry(value)
    }
}

fn normalize_tag(version: &VersionRef) -> String {
    let version_string = match version {
        VersionRef::Str(s) => (*s).to_string(),
        VersionRef::Entry(e) => e.version(),
    };
    if version_string.starts_with('v') {
        version_string
    } else {
        format!("v{version_string}")
    }
}

fn inline_link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))")
            .expect("valid inline-link regex")
    })
}

fn url_scheme_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("valid url-scheme regex"))
}

/// Canonicalize a legacy `pi-mono` repository URL prefix. Mirrors
/// `LEGACY_REPO_RE`, which uses a lookahead the `regex` crate lacks, so the
/// "followed by `/` or end" check is done manually.
fn canonicalize_legacy_repo(target: &str) -> String {
    for owner in ["badlogic", "earendil-works"] {
        let prefix = format!("https://github.com/{owner}/pi-mono");
        if let Some(rest) = target.strip_prefix(&prefix) {
            if rest.is_empty() || rest.starts_with('/') {
                return format!("https://github.com/{GITHUB_REPO}{rest}");
            }
        }
    }
    target.to_string()
}

struct LocalTarget {
    fragment: String,
    path_part: String,
    query: String,
}

fn split_local_target(target: &str) -> LocalTarget {
    let hash_index = target.find('#');
    let (before_hash, fragment) = match hash_index {
        Some(i) => (&target[..i], target[i..].to_string()),
        None => (target, String::new()),
    };
    match before_hash.find('?') {
        Some(qi) => LocalTarget {
            fragment,
            path_part: before_hash[..qi].to_string(),
            query: before_hash[qi..].to_string(),
        },
        None => LocalTarget {
            fragment,
            path_part: before_hash.to_string(),
            query: String::new(),
        },
    }
}

/// Lexically normalize a POSIX path, preserving a trailing slash when the input
/// had one (mirrors `path.posix.normalize` for the cases these links exercise).
fn posix_normalize_preserve(path: &str) -> String {
    super::bytes::posix_normalize(path, true)
}

fn resolve_repository_path(target_path: &str) -> Option<String> {
    let normalized_target = target_path.replace('\\', "/");
    let joined = if let Some(stripped) = normalized_target.strip_prefix('/') {
        let trimmed = stripped.trim_start_matches('/');
        posix_normalize_preserve(trimmed)
    } else {
        posix_normalize_preserve(&format!("{CHANGELOG_LINK_BASE_PATH}/{normalized_target}"))
    };

    if joined == "." || joined.starts_with("../") || joined == ".." {
        return None;
    }
    Some(joined)
}

fn posix_basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

fn is_directory_target(original_path: &str, repository_path: &str) -> bool {
    if original_path.ends_with('/') {
        return true;
    }
    !posix_basename(repository_path).contains('.')
}

/// Percent-encode like JavaScript `encodeURI`: leave the unreserved and
/// reserved URI characters intact, encode everything else.
fn encode_uri(input: &str) -> String {
    const SAFE: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'();/?:@&=+$,#";
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        if SAFE.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn normalize_changelog_link_target(target: &str, tag: &str) -> String {
    let mut canonical_target = canonicalize_legacy_repo(target);
    let repo_url = format!("https://github.com/{GITHUB_REPO}");

    for route in ["blob", "tree"] {
        for branch in ["main", "master"] {
            let floating_prefix = format!("{repo_url}/{route}/{branch}/");
            if let Some(rest) = canonical_target.strip_prefix(&floating_prefix) {
                canonical_target = format!("{repo_url}/{route}/{tag}/{rest}");
            }
        }
    }

    if canonical_target.starts_with('#')
        || canonical_target.starts_with("//")
        || url_scheme_regex().is_match(&canonical_target)
    {
        return canonical_target;
    }

    let local = split_local_target(&canonical_target);
    if local.path_part.is_empty() {
        return canonical_target;
    }

    let repository_path = match resolve_repository_path(&local.path_part) {
        Some(p) => p,
        None => return canonical_target,
    };

    let route = if is_directory_target(&local.path_part, &repository_path) {
        "tree"
    } else {
        "blob"
    };
    format!(
        "https://github.com/{GITHUB_REPO}/{route}/{tag}/{}{}{}",
        encode_uri(&repository_path),
        local.query,
        local.fragment
    )
}

/// Rewrite the inline markdown links in `markdown` for release `version`.
pub fn normalize_changelog_links<'a>(markdown: &str, version: impl Into<VersionRef<'a>>) -> String {
    let version = version.into();
    let tag = normalize_tag(&version);
    inline_link_regex()
        .replace_all(markdown, |caps: &Captures| {
            let prefix = &caps[1];
            let target = &caps[2];
            let suffix = &caps[3];
            format!(
                "{prefix}{}{suffix}",
                normalize_changelog_link_target(target, &tag)
            )
        })
        .into_owned()
}

/// Parse changelog entries from an in-memory string. Scans `## ` headings and
/// collects content until the next `## ` heading or end of input. Mirrors pi's
/// `parseChangelog` without the filesystem read.
pub fn parse_changelog_str(content: &str) -> Vec<ChangelogEntry> {
    let heading_re = Regex::new(r"##\s+\[?(\d+)\.(\d+)\.(\d+)\]?").expect("valid heading regex");
    let mut entries: Vec<ChangelogEntry> = Vec::new();
    let mut current_lines: Vec<&str> = Vec::new();
    let mut current_version: Option<(u64, u64, u64)> = None;

    let mut flush = |version: (u64, u64, u64), lines: &[&str]| {
        entries.push(ChangelogEntry {
            major: version.0,
            minor: version.1,
            patch: version.2,
            content: lines.join("\n").trim().to_string(),
        });
    };

    for line in content.split('\n') {
        if line.starts_with("## ") {
            if let (Some(version), false) = (current_version, current_lines.is_empty()) {
                flush(version, &current_lines);
            }

            if let Some(caps) = heading_re.captures(line) {
                current_version = Some((
                    caps[1].parse().unwrap_or(0),
                    caps[2].parse().unwrap_or(0),
                    caps[3].parse().unwrap_or(0),
                ));
                current_lines = vec![line];
            } else {
                current_version = None;
                current_lines = Vec::new();
            }
        } else if current_version.is_some() {
            current_lines.push(line);
        }
    }

    if let (Some(version), false) = (current_version, current_lines.is_empty()) {
        flush(version, &current_lines);
    }

    entries
}

/// Compare two entries by semantic version: negative if `v1 < v2`, zero if
/// equal, positive if `v1 > v2`.
pub fn compare_versions(v1: &ChangelogEntry, v2: &ChangelogEntry) -> i64 {
    if v1.major != v2.major {
        return v1.major as i64 - v2.major as i64;
    }
    if v1.minor != v2.minor {
        return v1.minor as i64 - v2.minor as i64;
    }
    v1.patch as i64 - v2.patch as i64
}

/// Return the entries strictly newer than `last_version` (e.g. `"1.2.3"`).
pub fn get_new_entries(entries: &[ChangelogEntry], last_version: &str) -> Vec<ChangelogEntry> {
    let parts: Vec<u64> = last_version
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let last = ChangelogEntry {
        major: parts.first().copied().unwrap_or(0),
        minor: parts.get(1).copied().unwrap_or(0),
        patch: parts.get(2).copied().unwrap_or(0),
        content: String::new(),
    };

    entries
        .iter()
        .filter(|entry| compare_versions(entry, &last) > 0)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_79() -> ChangelogEntry {
        ChangelogEntry {
            major: 0,
            minor: 79,
            patch: 0,
            content: String::new(),
        }
    }

    #[test]
    fn rewrites_package_relative_links_to_tag_pinned_source_links() {
        let markdown = [
            "[Project Trust](README.md#project-trust)",
            "[Extensions](docs/extensions.md#project_trust)",
            "[Examples](examples/extensions/)",
            "[Root README](../../README.md#supply-chain-hardening)",
        ]
        .join("\n");

        let expected = [
            "[Project Trust](https://github.com/earendil-works/pi/blob/v0.79.0/packages/coding-agent/README.md#project-trust)",
            "[Extensions](https://github.com/earendil-works/pi/blob/v0.79.0/packages/coding-agent/docs/extensions.md#project_trust)",
            "[Examples](https://github.com/earendil-works/pi/tree/v0.79.0/packages/coding-agent/examples/extensions/)",
            "[Root README](https://github.com/earendil-works/pi/blob/v0.79.0/README.md#supply-chain-hardening)",
        ]
        .join("\n");

        assert_eq!(normalize_changelog_links(&markdown, &entry_79()), expected);
    }

    #[test]
    fn canonicalizes_old_repository_urls_without_changing_external_links() {
        let markdown = [
            "[#5167](https://github.com/earendil-works/pi-mono/pull/5167)",
            "[#4163](https://github.com/badlogic/pi-mono/issues/4163)",
            "[Agent README](https://github.com/badlogic/pi-mono/blob/main/packages/agent/README.md)",
            "[External](https://example.com/docs)",
            "[Local anchor](#settings)",
        ]
        .join("\n");

        let expected = [
            "[#5167](https://github.com/earendil-works/pi/pull/5167)",
            "[#4163](https://github.com/earendil-works/pi/issues/4163)",
            "[Agent README](https://github.com/earendil-works/pi/blob/v0.79.0/packages/agent/README.md)",
            "[External](https://example.com/docs)",
            "[Local anchor](#settings)",
        ]
        .join("\n");

        assert_eq!(normalize_changelog_links(&markdown, "0.79.0"), expected);
    }

    #[test]
    fn compares_versions() {
        let a = ChangelogEntry {
            major: 1,
            minor: 2,
            patch: 3,
            content: String::new(),
        };
        let b = ChangelogEntry {
            major: 1,
            minor: 2,
            patch: 4,
            content: String::new(),
        };
        assert!(compare_versions(&a, &b) < 0);
        assert!(compare_versions(&b, &a) > 0);
        assert_eq!(compare_versions(&a, &a), 0);
    }

    #[test]
    fn parses_and_selects_new_entries() {
        let changelog = "## 1.1.0\n\nSecond feature\n\n## 1.0.0\n\nFirst release\n";
        let entries = parse_changelog_str(changelog);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].version(), "1.1.0");
        assert_eq!(entries[1].version(), "1.0.0");

        let new_entries = get_new_entries(&entries, "1.0.0");
        assert_eq!(new_entries.len(), 1);
        assert_eq!(new_entries[0].version(), "1.1.0");
    }
}
