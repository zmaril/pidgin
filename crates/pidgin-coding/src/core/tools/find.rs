//! Native file-glob search reproducing pi's find tool output.
//!
//! Ported from pi's `core/tools/find.ts`. pi shells out to `fd`; this
//! reimplements the search with the gitignore-aware `ignore` walker plus
//! `globset` glob matching, preserving the observable behavior: hidden files
//! that are not gitignored are included, `.gitignore` rules are scoped to their
//! own subtree (regression 3303), path globs like `src/**/*.spec.ts` match via
//! the `--full-path` + leading `**/` rewrite (regression 3302), posix
//! relativization, and the result-limit / byte-cap notices.
//!
//! The git-ancestor `.git` probe mirrors fd's `--no-require-git` handling:
//! outside a git repo the walker still honors `.gitignore`, while inside one
//! the walker uses git-aware behavior so nested repo boundaries are respected.
//!
//! Deferred seam: the directory walk and `.git` probe touch the filesystem
//! directly here rather than through pi's injectable `FindOperations`.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use super::path_utils::resolve_to_cwd;
use super::truncate::{
    format_size, truncate_head, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
};

const DEFAULT_LIMIT: usize = 1000;

/// The result of a find run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindResult {
    /// Formatted output (matching paths + notices, or the empty-result message).
    pub text: String,
    /// The result limit that was reached, if any.
    pub result_limit_reached: Option<usize>,
    /// Byte-cap truncation accounting, if truncation occurred.
    pub truncation: Option<TruncationResult>,
}

fn to_posix(value: &str) -> String {
    value.replace('\\', "/")
}

fn inside_git_repo(start: &Path) -> bool {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return true;
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    false
}

/// Run a native file search and format its output exactly like pi's find tool.
pub fn run_find(
    cwd: &str,
    pattern: &str,
    path: Option<&str>,
    limit: Option<usize>,
) -> Result<FindResult, String> {
    let search_path_str = resolve_to_cwd(path.unwrap_or("."), cwd).map_err(|e| e.to_string())?;
    let search_path = PathBuf::from(&search_path_str);
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT);

    // fd --glob matches against the basename unless the pattern contains '/',
    // in which case fd matches the full path and a path-containing pattern needs
    // a leading '**/' to match.
    let full_path = pattern.contains('/');
    let effective_pattern =
        if full_path && !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**"
        {
            format!("**/{pattern}")
        } else {
            pattern.to_string()
        };

    let glob = globset::GlobBuilder::new(&effective_pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("error parsing glob '{pattern}': {e}"))?
        .compile_matcher();

    let is_git = inside_git_repo(&search_path);

    let walk = WalkBuilder::new(&search_path)
        .hidden(false)
        .git_global(false)
        .require_git(is_git)
        .build();

    let mut relativized: Vec<String> = Vec::new();
    let mut result_limit_reached = false;
    for entry in walk.flatten() {
        // Skip the search root itself.
        if entry.depth() == 0 {
            continue;
        }
        let entry_path = entry.path();

        let is_match = if full_path {
            glob.is_match(entry_path)
        } else {
            entry_path
                .file_name()
                .map(|n| glob.is_match(n))
                .unwrap_or(false)
        };
        if !is_match {
            continue;
        }

        let relative = match entry_path.strip_prefix(&search_path) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => continue,
        };
        if relative.is_empty() {
            continue;
        }
        relativized.push(to_posix(&relative));

        if relativized.len() >= effective_limit {
            result_limit_reached = true;
            break;
        }
    }

    relativized.sort();

    if relativized.is_empty() {
        return Ok(FindResult {
            text: "No files found matching pattern".to_string(),
            result_limit_reached: None,
            truncation: None,
        });
    }

    let raw_output = relativized.join("\n");
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: usize::MAX,
            max_bytes: DEFAULT_MAX_BYTES,
        },
    );
    let mut result_output = truncation.content.clone();

    let mut result = FindResult {
        text: String::new(),
        result_limit_reached: None,
        truncation: None,
    };
    let mut notices: Vec<String> = Vec::new();
    if result_limit_reached {
        notices.push(format!(
            "{effective_limit} results limit reached. Use limit={} for more, or refine pattern",
            effective_limit * 2
        ));
        result.result_limit_reached = Some(effective_limit);
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        result.truncation = Some(truncation);
    }
    if !notices.is_empty() {
        result_output += &format!("\n\n[{}]", notices.join(". "));
    }

    result.text = result_output;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::test_support::TempDir;

    fn matched_files(text: &str) -> Vec<String> {
        if text == "No files found matching pattern" {
            return Vec::new();
        }
        let mut v: Vec<String> = text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('['))
            .collect();
        v.sort();
        v
    }

    /// Run a find rooted at `dir` and return the sorted matched paths.
    fn find_files(dir: &TempDir, pattern: &str) -> Vec<String> {
        let out = run_find(dir.cwd(), pattern, Some(dir.cwd()), None).unwrap();
        matched_files(&out.text)
    }

    /// Assert that `name` appears among `files`.
    fn assert_has(files: &[String], name: &str) {
        assert!(
            files.contains(&name.to_string()),
            "expected {name} in {files:?}"
        );
    }

    #[test]
    fn includes_hidden_files_not_gitignored() {
        let dir = TempDir::new("hidden");
        dir.mkdir(".secret");
        dir.write(".secret/hidden.txt", "hidden");
        dir.write("visible.txt", "visible");
        let files = find_files(&dir, "**/*.txt");
        assert_has(&files, "visible.txt");
        assert_has(&files, ".secret/hidden.txt");
    }

    #[test]
    fn respects_gitignore() {
        let dir = TempDir::new("gitignore");
        dir.write(".gitignore", "ignored.txt\n");
        dir.write("ignored.txt", "ignored");
        dir.write("kept.txt", "kept");
        let out = run_find(dir.cwd(), "**/*.txt", Some(dir.cwd()), None).unwrap();
        assert!(out.text.contains("kept.txt"));
        assert!(!out.text.contains("ignored.txt"));
    }

    #[test]
    fn surfaces_glob_parse_errors() {
        let dir = TempDir::new("badglob");
        let err = run_find(dir.cwd(), "[", Some(dir.cwd()), None).unwrap_err();
        assert!(err.contains("error parsing glob"), "got: {err}");
    }

    #[test]
    fn treats_flag_like_pattern_as_literal() {
        let dir = TempDir::new("flag");
        dir.write("a.txt", "");
        let out = run_find(dir.cwd(), "--help", Some(dir.cwd()), None).unwrap();
        assert_eq!(out.text, "No files found matching pattern");
    }

    // --- regression 3302: path-based glob patterns ---

    fn setup_3302() -> TempDir {
        let dir = TempDir::new("3302");
        dir.mkdir("some/parent/child");
        dir.mkdir("src/foo/bar");
        dir.write("some/parent/child/file.ext", "");
        dir.write("some/parent/child/test.spec.ts", "");
        dir.write("src/foo/bar/example.spec.ts", "");
        dir
    }

    #[test]
    fn r3302_basename_pattern_matches() {
        let dir = setup_3302();
        let files = find_files(&dir, "*.spec.ts");
        assert_eq!(
            files,
            vec![
                "some/parent/child/test.spec.ts".to_string(),
                "src/foo/bar/example.spec.ts".to_string()
            ]
        );
    }

    #[test]
    fn r3302_directory_prefixed_subtree() {
        let dir = setup_3302();
        let files = find_files(&dir, "some/parent/child/**");
        assert_has(&files, "some/parent/child/file.ext");
        assert_has(&files, "some/parent/child/test.spec.ts");
    }

    #[test]
    fn r3302_leading_wildcard_with_path_segments() {
        let dir = setup_3302();
        let files = find_files(&dir, "**/parent/child/*");
        assert_has(&files, "some/parent/child/file.ext");
        assert_has(&files, "some/parent/child/test.spec.ts");
    }

    #[test]
    fn r3302_src_path_glob_matches_nested_spec() {
        let dir = setup_3302();
        let files = find_files(&dir, "src/**/*.spec.ts");
        assert_eq!(files, vec!["src/foo/bar/example.spec.ts".to_string()]);
    }

    // --- regression 3303: nested .gitignore scoping ---

    fn setup_3303() -> TempDir {
        let dir = TempDir::new("3303");
        dir.mkdir("a");
        dir.mkdir("b");
        dir.write("a/.gitignore", "ignored.txt\n");
        dir.write("a/ignored.txt", "");
        dir.write("a/kept.txt", "");
        dir.write("b/ignored.txt", "");
        dir.write("b/kept.txt", "");
        dir.write("root.txt", "");
        dir
    }

    #[test]
    fn r3303_flat_sibling_scoping() {
        let dir = setup_3303();
        let files = find_files(&dir, "**/*.txt");
        assert_eq!(
            files,
            vec![
                "a/kept.txt".to_string(),
                "b/ignored.txt".to_string(),
                "b/kept.txt".to_string(),
                "root.txt".to_string()
            ]
        );
    }

    #[test]
    fn r3303_deeply_nested_scoping() {
        let dir = setup_3303();
        dir.mkdir("a/deep");
        dir.write("a/deep/.gitignore", "secret.txt\n");
        dir.write("a/deep/ignored.txt", "");
        dir.write("a/deep/secret.txt", "");
        dir.write("a/deep/kept.txt", "");
        let files = find_files(&dir, "**/*.txt");
        assert_eq!(
            files,
            vec![
                "a/deep/kept.txt".to_string(),
                "a/kept.txt".to_string(),
                "b/ignored.txt".to_string(),
                "b/kept.txt".to_string(),
                "root.txt".to_string()
            ]
        );
    }
}
