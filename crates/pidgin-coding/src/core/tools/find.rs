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
//!
//! The TUI render hooks ([`find_render_call`]/[`find_render_result`]) are ported
//! here as **stateless** functions (pi reuses a `Text` via
//! `context.lastComponent`, but the output is a pure function of its inputs).
//! find uses the DEFAULT render shell, so the returned `Text` is composed into
//! the shell's call/result box.

// straitjacket-allow-file:duplication — the find/grep/ls call+result render
// helpers (and their `render_tests` fixtures) faithfully mirror pi's per-tool
// `format<Tool>Call`/`format<Tool>Result`, which duplicate the same shape across
// tools; extracting them would diverge from the source-of-truth structure.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde_json::Value;

use pidgin_agent::types::AgentToolResult;
use pidgin_tui::renderer::Component;
use pidgin_tui::Text;

use crate::core::extensions::types::{ToolRenderContext, ToolRenderResultOptions};
use crate::modes::interactive::theme::runtime::Theme;

use super::path_utils::resolve_to_cwd;
use super::render_utils::{
    detail_usize, get_text_output_from_blocks, invalid_arg_text, json_number_display,
    shorten_path_home, str_json, tools_expand_hint,
};
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

// ---------------------------------------------------------------------------
// TUI render hooks (pi's `renderCall` / `renderResult`, `find.ts:359` / `:364`)
// ---------------------------------------------------------------------------

/// Local `theme.fg` wrapper falling back to unstyled text on an unknown color
/// key (pi's `theme.fg` is infallible; the ported [`Theme::fg`] returns a
/// `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// Format the find call header (pi's `formatFindCall`):
/// `find <pattern> in <path> (limit N)`, with `[invalid arg]` for a
/// wrong-typed pattern or path.
fn format_find_call(args: &Value, theme: &Theme) -> String {
    let pattern = str_json(args.get("pattern"));
    let raw_path = str_json(args.get("path"));
    let invalid = invalid_arg_text(theme);

    let pattern_part = match &pattern {
        None => invalid.clone(),
        Some(p) => fg(theme, "accent", p),
    };
    let path_part = match &raw_path {
        None => invalid.clone(),
        Some(p) => shorten_path_home(if p.is_empty() { "." } else { p }),
    };
    let mut text = format!(
        "{} {}{}",
        fg(theme, "toolTitle", &theme.bold("find")),
        pattern_part,
        fg(theme, "toolOutput", &format!(" in {path_part}"))
    );
    if let Some(limit) = args.get("limit") {
        text += &fg(
            theme,
            "toolOutput",
            &format!(" (limit {})", json_number_display(limit)),
        );
    }
    text
}

/// Format the find result body (pi's `formatFindResult`): the matches (up to 20
/// lines unless expanded) plus a `[Truncated: …]` footer for result/byte caps.
fn format_find_result(
    result: &AgentToolResult,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
) -> String {
    let output = get_text_output_from_blocks(&result.content, show_images);
    let output = output.trim();
    let mut text = String::new();
    if !output.is_empty() {
        let lines: Vec<&str> = output.split('\n').collect();
        let max_lines = if options.expanded { lines.len() } else { 20 };
        let display_lines = &lines[..max_lines.min(lines.len())];
        let remaining = lines.len() as isize - max_lines as isize;
        text += &format!(
            "\n{}",
            display_lines
                .iter()
                .map(|line| fg(theme, "toolOutput", line))
                .collect::<Vec<_>>()
                .join("\n")
        );
        if remaining > 0 {
            text += &fg(theme, "muted", &format!("\n... ({remaining} more lines,"));
            text += " ";
            text += &tools_expand_hint(theme, "to expand");
            text += &fg(theme, "muted", ")");
        }
    }

    let result_limit = result
        .details
        .get("resultLimitReached")
        .and_then(Value::as_u64)
        .filter(|&r| r != 0);
    let truncation = result.details.get("truncation");
    let truncated = truncation
        .and_then(|t| t.get("truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if result_limit.is_some() || truncated {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(r) = result_limit {
            warnings.push(format!("{r} results limit"));
        }
        if truncated {
            let max_bytes = truncation
                .and_then(|t| detail_usize(t, "maxBytes"))
                .unwrap_or(DEFAULT_MAX_BYTES);
            warnings.push(format!("{} limit", format_size(max_bytes)));
        }
        text += &format!(
            "\n{}",
            fg(
                theme,
                "warning",
                &format!("[Truncated: {}]", warnings.join(", "))
            )
        );
    }
    text
}

/// Custom rendering for the find tool call (pi's `renderCall`, `find.ts:359`).
pub fn find_render_call(
    args: &Value,
    theme: &Theme,
    _context: &ToolRenderContext,
) -> Box<dyn Component> {
    Box::new(Text::new(&format_find_call(args, theme), 0, 0, None))
}

/// Custom rendering for the find tool result (pi's `renderResult`,
/// `find.ts:364`).
pub fn find_render_result(
    result: &AgentToolResult,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    Box::new(Text::new(
        &format_find_result(result, options, theme, context.show_images),
        0,
        0,
        None,
    ))
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

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode};
    use pidgin_ai::ContentBlock;
    use serde_json::json;
    use std::path::PathBuf;

    fn dark_theme() -> Theme {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/modes/interactive/theme/dark.json");
        let content = std::fs::read_to_string(&path).expect("read dark.json");
        let json = parse_theme_json(&content).expect("parse dark.json");
        create_theme(&json, Some(ColorMode::Color256), None).expect("create dark theme")
    }

    fn text_result(text: &str, details: Value) -> AgentToolResult {
        AgentToolResult {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                text_signature: None,
            }],
            details,
            added_tool_names: None,
            terminate: None,
        }
    }

    fn opts(expanded: bool) -> ToolRenderResultOptions {
        ToolRenderResultOptions {
            expanded,
            is_partial: false,
        }
    }

    #[test]
    fn call_renders_pattern_without_slashes_and_default_dot_path() {
        let theme = dark_theme();
        let args = json!({ "pattern": "*.md" });
        let text = format_find_call(&args, &theme);
        assert!(text.contains("find"));
        assert!(text.contains("*.md"), "got: {text:?}");
        assert!(
            !text.contains("/*.md/"),
            "find must not slash-wrap: {text:?}"
        );
        assert!(text.contains(" in ."), "got: {text:?}");
    }

    #[test]
    fn call_renders_limit_in_parens() {
        let theme = dark_theme();
        let args = json!({ "pattern": "*.rs", "path": "src", "limit": 1000 });
        let text = format_find_call(&args, &theme);
        assert!(text.contains("*.rs"));
        assert!(text.contains(" in src"));
        assert!(text.contains("(limit 1000)"), "got: {text:?}");
    }

    #[test]
    fn call_invalid_path_shows_invalid_marker() {
        let theme = dark_theme();
        let args = json!({ "pattern": "*.rs", "path": 42 });
        let text = format_find_call(&args, &theme);
        assert!(text.contains("[invalid arg]"), "got: {text:?}");
    }

    #[test]
    fn result_lists_paths() {
        let theme = dark_theme();
        let result = text_result("a.rs\nb.rs\nsub/c.rs", Value::Null);
        let body = format_find_result(&result, &opts(false), &theme, false);
        assert!(body.contains("a.rs"));
        assert!(body.contains("sub/c.rs"));
    }

    #[test]
    fn result_footer_reports_result_and_byte_caps() {
        let theme = dark_theme();
        let details = json!({ "resultLimitReached": 1000, "truncation": { "truncated": true, "maxBytes": 51200 } });
        let result = text_result("a.rs", details);
        let body = format_find_result(&result, &opts(false), &theme, false);
        assert!(body.contains("1000 results limit"), "got: {body:?}");
        assert!(body.contains("50.0KB limit"), "got: {body:?}");
    }
}
