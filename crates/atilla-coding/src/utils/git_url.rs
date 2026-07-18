//! Parse and validate git source URLs.
//!
//! Ported from pi's `utils/git.ts`. Produces a [`GitUrl`] from a git source
//! string, enforcing pi's SSRF / path-traversal safety checks (rejecting
//! absolute paths, backslashes, null bytes, `..` segments, and encoded-unsafe
//! inputs).
//!
//! pi delegates GitHub/GitLab/etc. canonicalization to the `hosted-git-info`
//! npm package. For the GitHub cases pi's tests exercise, the generic parser in
//! this module (protocol URLs, scp-like `git@host:path`, and `host/path`
//! shorthand) yields identical results, so `hosted-git-info` is not a
//! dependency here. The rules pi's tests pin are reproduced exactly:
//! - full protocol URLs (`https`, `http`, `ssh`, `git`) are accepted without a
//!   `git:` prefix;
//! - shorthand forms are accepted only with a `git:` prefix;
//! - an `@ref` suffix is split out and marks the source as pinned.

use regex::Regex;
use std::sync::OnceLock;

/// Parsed git source information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitUrl {
    /// Always `"git"` for git sources.
    pub kind: &'static str,
    /// Clone URL (valid for `git clone`, without any `@ref` suffix).
    pub repo: String,
    /// Git host domain (e.g. `github.com`).
    pub host: String,
    /// Repository path (e.g. `user/repo`).
    pub path: String,
    /// Git ref (branch, tag, commit) if specified.
    pub git_ref: Option<String>,
    /// True if a ref was specified (the source will not be auto-updated).
    pub pinned: bool,
}

fn protocol_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(https?|ssh|git)://").expect("valid protocol regex"))
}

fn scp_like_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^git@([^:]+):(.+)$").expect("valid scp-like regex"))
}

/// A minimally-parsed URL: scheme, authority (host + optional userinfo/port),
/// and path (including the leading `/`).
struct ParsedUrl {
    scheme: String,
    authority: String,
    path: String,
}

fn parse_url(url: &str) -> Option<ParsedUrl> {
    let idx = url.find("://")?;
    let scheme = url[..idx].to_string();
    let rest = &url[idx + 3..];
    match rest.find('/') {
        Some(path_start) => Some(ParsedUrl {
            scheme,
            authority: rest[..path_start].to_string(),
            path: rest[path_start..].to_string(),
        }),
        None => Some(ParsedUrl {
            scheme,
            authority: rest.to_string(),
            path: String::new(),
        }),
    }
}

/// Extract the hostname from an authority, stripping userinfo and port.
fn hostname_of(authority: &str) -> String {
    let after_user = authority.rsplit('@').next().unwrap_or(authority);
    after_user
        .split(':')
        .next()
        .unwrap_or(after_user)
        .to_string()
}

fn strip_leading_slashes(value: &str) -> &str {
    value.trim_start_matches('/')
}

struct SplitRef {
    repo: String,
    git_ref: Option<String>,
}

/// Split an `@ref` suffix off the repo path. `path_with_maybe_ref` is the
/// portion after the host; `rebuild_repo` reconstructs the ref-free clone URL
/// from the repo path for the matched form. Falls back to the unmodified `url`
/// with no ref when there is no `@`, or either side of it is empty.
fn split_at_ref<F>(url: &str, path_with_maybe_ref: &str, rebuild_repo: F) -> SplitRef
where
    F: FnOnce(&str) -> String,
{
    match path_with_maybe_ref.find('@') {
        None => SplitRef {
            repo: url.to_string(),
            git_ref: None,
        },
        Some(sep) => {
            let repo_path = &path_with_maybe_ref[..sep];
            let git_ref = &path_with_maybe_ref[sep + 1..];
            if repo_path.is_empty() || git_ref.is_empty() {
                SplitRef {
                    repo: url.to_string(),
                    git_ref: None,
                }
            } else {
                SplitRef {
                    repo: rebuild_repo(repo_path),
                    git_ref: Some(git_ref.to_string()),
                }
            }
        }
    }
}

fn split_ref(url: &str) -> SplitRef {
    if let Some(caps) = scp_like_regex().captures(url) {
        let host = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let path_with_maybe_ref = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        return split_at_ref(url, path_with_maybe_ref, |repo_path| {
            format!("git@{host}:{repo_path}")
        });
    }

    if url.contains("://") {
        return match parse_url(url) {
            Some(parsed) => {
                let path_with_maybe_ref = strip_leading_slashes(&parsed.path);
                split_at_ref(url, path_with_maybe_ref, |repo_path| {
                    format!("{}://{}/{}", parsed.scheme, parsed.authority, repo_path)
                })
            }
            None => SplitRef {
                repo: url.to_string(),
                git_ref: None,
            },
        };
    }

    match url.find('/') {
        None => SplitRef {
            repo: url.to_string(),
            git_ref: None,
        },
        Some(slash_index) => {
            let host = &url[..slash_index];
            let path_with_maybe_ref = &url[slash_index + 1..];
            split_at_ref(url, path_with_maybe_ref, |repo_path| {
                format!("{host}/{repo_path}")
            })
        }
    }
}

fn has_unsafe_git_install_part(value: &str, allow_slash: bool) -> bool {
    // A malformed escape (a `decodeURIComponent` that throws) is treated as
    // unsafe.
    let decoded = match super::bytes::percent_decode(value) {
        Some(d) => d,
        None => return true,
    };
    for candidate in [value.to_string(), decoded] {
        if candidate.contains('\0') || candidate.contains('\\') || candidate.starts_with('/') {
            return true;
        }
        if !allow_slash && candidate.contains('/') {
            return true;
        }
        if candidate.split('/').any(|part| part == "..") {
            return true;
        }
    }
    false
}

fn build_git_source(
    repo: String,
    host: String,
    path: &str,
    git_ref: Option<String>,
) -> Option<GitUrl> {
    if path.starts_with('/') {
        return None;
    }
    let without_git = path.strip_suffix(".git").unwrap_or(path);
    let normalized_path = strip_leading_slashes(without_git).to_string();
    if host.is_empty() || normalized_path.is_empty() || normalized_path.split('/').count() < 2 {
        return None;
    }
    if has_unsafe_git_install_part(&host, false)
        || has_unsafe_git_install_part(&normalized_path, true)
    {
        return None;
    }

    let pinned = git_ref.is_some();
    Some(GitUrl {
        kind: "git",
        repo,
        host,
        path: normalized_path,
        git_ref,
        pinned,
    })
}

fn parse_generic_git_url(url: &str) -> Option<GitUrl> {
    let split = split_ref(url);
    let repo_without_ref = split.repo;
    let mut repo = repo_without_ref.clone();
    let host;
    let path;

    if let Some(caps) = scp_like_regex().captures(&repo_without_ref) {
        host = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        path = caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
    } else if repo_without_ref.starts_with("https://")
        || repo_without_ref.starts_with("http://")
        || repo_without_ref.starts_with("ssh://")
        || repo_without_ref.starts_with("git://")
    {
        let parsed = parse_url(&repo_without_ref)?;
        host = hostname_of(&parsed.authority);
        path = strip_leading_slashes(&parsed.path).to_string();
    } else {
        let slash_index = repo_without_ref.find('/')?;
        host = repo_without_ref[..slash_index].to_string();
        path = repo_without_ref[slash_index + 1..].to_string();
        if !host.contains('.') && host != "localhost" {
            return None;
        }
        repo = format!("https://{repo_without_ref}");
    }

    build_git_source(repo, host, &path, split.git_ref)
}

/// Parse a git source string into a [`GitUrl`], or `None` if invalid.
///
/// With a `git:` prefix, all historical shorthand forms are accepted. Without
/// it, only explicit protocol URLs are accepted.
pub fn parse_git_url(source: &str) -> Option<GitUrl> {
    let trimmed = source.trim();
    let has_git_prefix = trimmed.starts_with("git:");
    let url = if has_git_prefix {
        trimmed[4..].trim()
    } else {
        trimmed
    };

    if !has_git_prefix && !protocol_regex().is_match(url) {
        return None;
    }

    parse_generic_git_url(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url() {
        let result = parse_git_url("https://github.com/user/repo").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.repo, "https://github.com/user/repo");
        assert_eq!(result.git_ref, None);
        assert!(!result.pinned);
    }

    #[test]
    fn parses_ssh_url() {
        let result = parse_git_url("ssh://git@github.com/user/repo").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.repo, "ssh://git@github.com/user/repo");
    }

    #[test]
    fn parses_protocol_url_with_ref() {
        let result = parse_git_url("https://github.com/user/repo@v1.0.0").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.git_ref.as_deref(), Some("v1.0.0"));
        assert_eq!(result.repo, "https://github.com/user/repo");
        assert!(result.pinned);
    }

    #[test]
    fn parses_scp_like_with_git_prefix() {
        let result = parse_git_url("git:git@github.com:user/repo").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.repo, "git@github.com:user/repo");
    }

    #[test]
    fn parses_host_path_shorthand_with_git_prefix() {
        let result = parse_git_url("git:github.com/user/repo").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.repo, "https://github.com/user/repo");
    }

    #[test]
    fn parses_shorthand_with_ref_and_git_prefix() {
        let result = parse_git_url("git:git@github.com:user/repo@v1.0.0").unwrap();
        assert_eq!(result.host, "github.com");
        assert_eq!(result.path, "user/repo");
        assert_eq!(result.git_ref.as_deref(), Some("v1.0.0"));
        assert_eq!(result.repo, "git@github.com:user/repo");
    }

    #[test]
    fn rejects_unsafe_git_install_path_inputs() {
        for source in [
            "git:git@evil.example:../../victim/repo",
            "https://evil.example/..%2F..%2Fvictim/repo",
            "https://evil.example/..%2F..%2Fvictim/repo%",
            "git:git@evil.example:/absolute/repo",
            "git:git@evil.example:user\\repo/name",
            "git:git@evil.example:user/repo\0name",
        ] {
            assert_eq!(parse_git_url(source), None, "expected None for {source:?}");
        }
    }

    #[test]
    fn rejects_shorthand_without_git_prefix() {
        assert_eq!(parse_git_url("git@github.com:user/repo"), None);
        assert_eq!(parse_git_url("github.com/user/repo"), None);
        assert_eq!(parse_git_url("user/repo"), None);
    }
}
