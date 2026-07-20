//! Native content search reproducing pi's grep tool output.
//!
//! Ported from pi's `core/tools/grep.ts`. pi shells out to `rg --json`; this
//! reimplements the search natively with the ripgrep-family `grep-regex`
//! matcher plus the gitignore-aware `ignore` directory walker, then applies the
//! same pure formatting/relativization/notice/limit layer. Observable output is
//! preserved exactly: match lines `path:line: text`, context lines
//! `path-line- text`, the `[N matches limit reached ...]` notice, literal
//! treatment of flag-like patterns (no shell, so nothing is interpreted as a
//! flag), `No matches found`, long-line truncation, and the byte cap via
//! `truncate_head`.
//!
//! Deferred seam: the filesystem access (directory walk + file reads) is done
//! directly here rather than through pi's injectable `GrepOperations`; a custom
//! operations backend (for example SSH) would be layered on later.
//!
//! The TUI render hooks ([`grep_render_call`]/[`grep_render_result`]) are ported
//! here as **stateless** functions (pi reuses a `Text` via
//! `context.lastComponent`, but the output is a pure function of its inputs).
//! grep uses the DEFAULT render shell, so the returned `Text` is composed into
//! the shell's call/result box.

use std::path::{Path, PathBuf};

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
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
    format_size, truncate_head, truncate_line, TruncationOptions, TruncationResult,
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH,
};

const DEFAULT_LIMIT: usize = 100;

/// Parameters for [`run_grep`].
#[derive(Debug, Clone)]
pub struct GrepParams<'a> {
    /// Search pattern (regex, or literal when `literal` is set).
    pub pattern: &'a str,
    /// Directory or file to search (default: current directory).
    pub path: Option<&'a str>,
    /// Optional glob filter for files (directory search only).
    pub glob: Option<&'a str>,
    /// Case-insensitive search.
    pub ignore_case: bool,
    /// Treat the pattern as a literal string.
    pub literal: bool,
    /// Lines of context before and after each match.
    pub context: usize,
    /// Maximum matches (default 100, minimum 1).
    pub limit: Option<usize>,
}

/// The result of a grep run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepResult {
    /// The formatted output text (matches + notices, or "No matches found").
    pub text: String,
    /// The limit that was reached, if any.
    pub match_limit_reached: Option<usize>,
    /// Byte-cap truncation accounting, if truncation occurred.
    pub truncation: Option<TruncationResult>,
    /// Whether any line was truncated to `GREP_MAX_LINE_LENGTH`.
    pub lines_truncated: bool,
}

struct Match {
    file_path: PathBuf,
    line_number: usize,
}

fn format_path(search_path: &Path, is_directory: bool, file_path: &Path) -> String {
    if is_directory {
        if let Ok(rel) = file_path.strip_prefix(search_path) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !rel.is_empty() && !rel.starts_with("..") {
                return rel;
            }
        }
    }
    file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_file_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => content
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .map(|s| s.to_string())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn glob_matches(glob: &globset::GlobMatcher, search_path: &Path, file_path: &Path) -> bool {
    // Match the glob against the path relative to the search root (basename for
    // simple patterns), matching rg's `--glob` semantics closely enough for the
    // supported fixtures.
    if let Ok(rel) = file_path.strip_prefix(search_path) {
        if glob.is_match(rel) {
            return true;
        }
    }
    if let Some(name) = file_path.file_name() {
        return glob.is_match(name);
    }
    false
}

/// Run a native grep and format its output exactly like pi's grep tool.
pub fn run_grep(cwd: &str, params: &GrepParams) -> Result<GrepResult, String> {
    let search_path_str =
        resolve_to_cwd(params.path.unwrap_or("."), cwd).map_err(|e| e.to_string())?;
    let search_path = PathBuf::from(&search_path_str);

    let metadata = std::fs::metadata(&search_path)
        .map_err(|_| format!("Path not found: {search_path_str}"))?;
    let is_directory = metadata.is_dir();

    let effective_limit = params.limit.unwrap_or(DEFAULT_LIMIT).max(1);

    let effective_pattern = if params.literal {
        regex::escape(params.pattern)
    } else {
        params.pattern.to_string()
    };
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(params.ignore_case)
        .build(&effective_pattern)
        .map_err(|e| format!("Failed to build search pattern: {e}"))?;

    let glob_matcher = match params.glob {
        Some(g) => Some(
            globset::GlobBuilder::new(g)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("error parsing glob '{g}': {e}"))?
                .compile_matcher(),
        ),
        None => None,
    };

    // Collect the files to search.
    let mut files: Vec<PathBuf> = Vec::new();
    if is_directory {
        let walk = WalkBuilder::new(&search_path)
            .hidden(false)
            .git_global(false)
            .require_git(false)
            .build();
        for entry in walk.flatten() {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let path = entry.into_path();
                if let Some(gm) = &glob_matcher {
                    if !glob_matches(gm, &search_path, &path) {
                        continue;
                    }
                }
                files.push(path);
            }
        }
        files.sort();
    } else {
        files.push(search_path.clone());
    }

    // Find matches in order, capped at the effective limit.
    let mut matches: Vec<Match> = Vec::new();
    let mut match_count = 0usize;
    let mut match_limit_reached = false;
    'outer: for file in &files {
        let lines = read_file_lines(file);
        for (i, line) in lines.iter().enumerate() {
            if matcher.is_match(line.as_bytes()).unwrap_or(false) {
                match_count += 1;
                matches.push(Match {
                    file_path: file.clone(),
                    line_number: i + 1,
                });
                if match_count >= effective_limit {
                    match_limit_reached = true;
                    break 'outer;
                }
            }
        }
    }

    if match_count == 0 {
        return Ok(GrepResult {
            text: "No matches found".to_string(),
            match_limit_reached: None,
            truncation: None,
            lines_truncated: false,
        });
    }

    let mut lines_truncated = false;
    let mut output_lines: Vec<String> = Vec::new();
    for m in &matches {
        let relative_path = format_path(&search_path, is_directory, &m.file_path);
        let file_lines = read_file_lines(&m.file_path);
        if file_lines.is_empty() {
            output_lines.push(format!(
                "{relative_path}:{}: (unable to read file)",
                m.line_number
            ));
            continue;
        }
        let start = if params.context > 0 {
            m.line_number.saturating_sub(params.context).max(1)
        } else {
            m.line_number
        };
        let end = if params.context > 0 {
            (m.line_number + params.context).min(file_lines.len())
        } else {
            m.line_number
        };
        for current in start..=end {
            let line_text = file_lines
                .get(current - 1)
                .map(String::as_str)
                .unwrap_or("");
            let sanitized = line_text.replace('\r', "");
            let tl = truncate_line(&sanitized, GREP_MAX_LINE_LENGTH);
            if tl.was_truncated {
                lines_truncated = true;
            }
            if current == m.line_number {
                output_lines.push(format!("{relative_path}:{current}: {}", tl.text));
            } else {
                output_lines.push(format!("{relative_path}-{current}- {}", tl.text));
            }
        }
    }

    let raw_output = output_lines.join("\n");
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: usize::MAX,
            max_bytes: DEFAULT_MAX_BYTES,
        },
    );
    let mut output = truncation.content.clone();

    let mut result = GrepResult {
        text: String::new(),
        match_limit_reached: None,
        truncation: None,
        lines_truncated: false,
    };
    let mut notices: Vec<String> = Vec::new();
    if match_limit_reached {
        notices.push(format!(
            "{effective_limit} matches limit reached. Use limit={} for more, or refine pattern",
            effective_limit * 2
        ));
        result.match_limit_reached = Some(effective_limit);
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        result.truncation = Some(truncation);
    }
    if lines_truncated {
        notices.push(format!(
            "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
        ));
        result.lines_truncated = true;
    }
    if !notices.is_empty() {
        output += &format!("\n\n[{}]", notices.join(". "));
    }

    result.text = output;
    Ok(result)
}

// ---------------------------------------------------------------------------
// TUI render hooks (pi's `renderCall` / `renderResult`, `grep.ts:370` / `:375`)
// ---------------------------------------------------------------------------

/// Local `theme.fg` wrapper falling back to unstyled text on an unknown color
/// key (pi's `theme.fg` is infallible; the ported [`Theme::fg`] returns a
/// `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// Format the grep call header (pi's `formatGrepCall`):
/// `grep /pattern/ in <path> (glob) limit N`, with `[invalid arg]` for a
/// wrong-typed pattern or path.
fn format_grep_call(args: &Value, theme: &Theme) -> String {
    let pattern = str_json(args.get("pattern"));
    let raw_path = str_json(args.get("path"));
    let invalid = invalid_arg_text(theme);

    let pattern_part = match &pattern {
        None => invalid.clone(),
        Some(p) => fg(theme, "accent", &format!("/{p}/")),
    };
    let path_part = match &raw_path {
        None => invalid.clone(),
        Some(p) => shorten_path_home(if p.is_empty() { "." } else { p }),
    };
    let mut text = format!(
        "{} {}{}",
        fg(theme, "toolTitle", &theme.bold("grep")),
        pattern_part,
        fg(theme, "toolOutput", &format!(" in {path_part}"))
    );
    if let Some(glob) = str_json(args.get("glob")).filter(|g| !g.is_empty()) {
        text += &fg(theme, "toolOutput", &format!(" ({glob})"));
    }
    if let Some(limit) = args.get("limit") {
        text += &fg(
            theme,
            "toolOutput",
            &format!(" limit {}", json_number_display(limit)),
        );
    }
    text
}

/// Format the grep result body (pi's `formatGrepResult`): the matches (up to 15
/// lines unless expanded) plus a `[Truncated: …]` footer for match/byte/line
/// caps.
fn format_grep_result(
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
        let max_lines = if options.expanded { lines.len() } else { 15 };
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

    let match_limit = result
        .details
        .get("matchLimitReached")
        .and_then(Value::as_u64)
        .filter(|&m| m != 0);
    let truncation = result.details.get("truncation");
    let truncated = truncation
        .and_then(|t| t.get("truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lines_truncated = result
        .details
        .get("linesTruncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if match_limit.is_some() || truncated || lines_truncated {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(m) = match_limit {
            warnings.push(format!("{m} matches limit"));
        }
        if truncated {
            let max_bytes = truncation
                .and_then(|t| detail_usize(t, "maxBytes"))
                .unwrap_or(DEFAULT_MAX_BYTES);
            warnings.push(format!("{} limit", format_size(max_bytes)));
        }
        if lines_truncated {
            warnings.push("some lines truncated".to_string());
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

/// Custom rendering for the grep tool call (pi's `renderCall`, `grep.ts:370`).
pub fn grep_render_call(
    args: &Value,
    theme: &Theme,
    _context: &ToolRenderContext,
) -> Box<dyn Component> {
    Box::new(Text::new(&format_grep_call(args, theme), 0, 0, None))
}

/// Custom rendering for the grep tool result (pi's `renderResult`,
/// `grep.ts:375`).
pub fn grep_render_result(
    result: &AgentToolResult,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    Box::new(Text::new(
        &format_grep_result(result, options, theme, context.show_images),
        0,
        0,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::test_support::TempDir;

    fn params<'a>(pattern: &'a str, path: &'a str) -> GrepParams<'a> {
        GrepParams {
            pattern,
            path: Some(path),
            glob: None,
            ignore_case: false,
            literal: false,
            context: 0,
            limit: None,
        }
    }

    #[test]
    fn includes_filename_for_single_file() {
        let dir = TempDir::new("single");
        let file = dir.write("example.txt", "first line\nmatch line\nlast line");
        let out = run_grep(
            dir.path.to_str().unwrap(),
            &params("match", file.to_str().unwrap()),
        )
        .unwrap();
        assert!(
            out.text.contains("example.txt:2: match line"),
            "got: {}",
            out.text
        );
    }

    #[test]
    fn respects_limit_and_includes_context() {
        let dir = TempDir::new("context");
        let content = [
            "before",
            "match one",
            "after",
            "middle",
            "match two",
            "after two",
        ]
        .join("\n");
        let file = dir.write("context.txt", &content);
        let mut p = params("match", file.to_str().unwrap());
        p.limit = Some(1);
        p.context = 1;
        let out = run_grep(dir.path.to_str().unwrap(), &p).unwrap();
        assert!(
            out.text.contains("context.txt-1- before"),
            "got: {}",
            out.text
        );
        assert!(out.text.contains("context.txt:2: match one"));
        assert!(out.text.contains("context.txt-3- after"));
        assert!(out
            .text
            .contains("[1 matches limit reached. Use limit=2 for more, or refine pattern]"));
        assert!(!out.text.contains("match two"));
    }

    #[test]
    fn treats_flag_like_pattern_as_literal_text() {
        let dir = TempDir::new("flag");
        let payload = dir.write("payload.sh", "#!/bin/sh\necho executed\ncat \"$1\"\n");
        dir.write("target.txt", "target\n");
        let pattern = format!("--pre={}", payload.to_string_lossy());
        let out = run_grep(
            dir.path.to_str().unwrap(),
            &params(&pattern, dir.path.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(out.text, "No matches found");
    }

    #[test]
    fn reports_no_matches() {
        let dir = TempDir::new("nomatch");
        let file = dir.write("a.txt", "nothing here\n");
        let out = run_grep(
            dir.path.to_str().unwrap(),
            &params("zzz", file.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(out.text, "No matches found");
    }

    #[test]
    fn errors_on_missing_path() {
        let dir = TempDir::new("missing");
        let missing = dir.path.join("does-not-exist");
        let err = run_grep(
            dir.path.to_str().unwrap(),
            &params("x", missing.to_str().unwrap()),
        )
        .unwrap_err();
        assert!(err.starts_with("Path not found:"), "got: {err}");
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
    fn call_renders_pattern_in_slashes_and_default_dot_path() {
        let theme = dark_theme();
        let args = json!({ "pattern": "needle" });
        let text = format_grep_call(&args, &theme);
        assert!(text.contains("grep"));
        assert!(text.contains("/needle/"), "got: {text:?}");
        assert!(text.contains(" in ."), "got: {text:?}");
    }

    #[test]
    fn call_renders_glob_and_limit_suffixes() {
        let theme = dark_theme();
        let args = json!({ "pattern": "foo", "path": "src", "glob": "*.rs", "limit": 50 });
        let text = format_grep_call(&args, &theme);
        assert!(text.contains("/foo/"));
        assert!(text.contains(" in src"));
        assert!(text.contains("(*.rs)"), "got: {text:?}");
        assert!(text.contains("limit 50"), "got: {text:?}");
    }

    #[test]
    fn call_invalid_pattern_shows_invalid_marker() {
        let theme = dark_theme();
        let args = json!({ "pattern": 42 });
        let text = format_grep_call(&args, &theme);
        assert!(text.contains("[invalid arg]"), "got: {text:?}");
    }

    #[test]
    fn result_lists_matches() {
        let theme = dark_theme();
        let result = text_result("src/a.rs:1: let x = 1;\nsrc/b.rs:5: fn y() {}", Value::Null);
        let body = format_grep_result(&result, &opts(false), &theme, false);
        assert!(body.contains("src/a.rs:1:"));
        assert!(body.contains("src/b.rs:5:"));
    }

    #[test]
    fn result_footer_reports_match_and_line_truncation() {
        let theme = dark_theme();
        let details = json!({ "matchLimitReached": 100, "linesTruncated": true });
        let result = text_result("src/a.rs:1: x", details);
        let body = format_grep_result(&result, &opts(false), &theme, false);
        assert!(body.contains("100 matches limit"), "got: {body:?}");
        assert!(body.contains("some lines truncated"), "got: {body:?}");
    }
}
