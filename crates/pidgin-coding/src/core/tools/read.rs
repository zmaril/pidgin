//! Pure text-slicing and continuation-notice layer of the read tool.
//!
//! Ported from pi's `core/tools/read.ts`. The offset/limit slicing (1-indexed
//! to 0-indexed), `truncate_head` application, and the exact continuation
//! notices are pure and live in [`format_text_read`]. The surrounding tool
//! (filesystem read, image detection/processing) is a deferred seam: those need
//! an execution environment plus the image layer, so this module takes the
//! already-read file content as input and returns the formatted text +
//! truncation details.
//!
//! The TUI render hooks ([`read_render_call`]/[`read_render_result`]) are ported
//! here as **stateless** functions (pi threads a reused `Text` component via
//! `context.lastComponent`, but the output is a pure function of
//! `{args, result, options, context}`). read uses the DEFAULT render shell, so
//! the [`ToolExecution`](crate::modes::interactive::components) shell composes
//! the returned `Text` into its call/result box.
//!
//! Divergences: the read compact-call classification's pi-docs branch needs
//! pi's `config.ts` `getReadmePath()` seam (not ported; see
//! [`get_compact_read_classification`]), and valid-language syntax highlighting
//! is the deno-plane seam documented on
//! [`highlight_code`](super::render_utils::highlight_code).

// straitjacket-allow-file:duplication — the read/write call+result render
// helpers (and their `render_tests` fixtures) faithfully mirror pi's per-tool
// `format<Tool>Call`/`format<Tool>Result`, which duplicate the same shape across
// tools; extracting them would diverge from the source-of-truth structure.

use serde_json::Value;

use pidgin_agent::types::AgentToolResult;
use pidgin_tui::renderer::Component;
use pidgin_tui::Text;

use std::path::Path;

use crate::core::extensions::types::{ToolRenderContext, ToolRenderResultOptions};
use crate::modes::interactive::theme::runtime::Theme;
use crate::utils::paths::format_path_relative_to_cwd_or_absolute;

use super::path_utils::resolve_to_cwd;
use super::render_utils::{
    detail_usize, get_language_from_path, get_text_output_from_blocks, highlight_code,
    render_tool_path, replace_tabs, str_json, tools_expand_hint, tools_expand_key_text,
    trim_trailing_empty_lines,
};
use super::truncate::{
    format_size, truncate_head, TruncatedBy, TruncationOptions, TruncationResult,
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

/// The formatted output of a text read: display text plus optional truncation
/// details (present only when truncation or a first-line-too-long condition
/// occurred, matching pi's `details` assignment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadTextOutput {
    /// The text to return to the model.
    pub text: String,
    /// Truncation accounting, when applicable.
    pub details: Option<TruncationResult>,
}

/// Format a text-file read given its full `content`.
///
/// `offset` is a 1-indexed start line; `limit` caps the number of lines. Errors
/// mirror pi's out-of-bounds message. `path` is only used to build the
/// bash-fallback hint for over-long first lines.
pub fn format_text_read(
    content: &str,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ReadTextOutput, String> {
    let all_lines: Vec<&str> = content.split('\n').collect();
    let total_file_lines = all_lines.len();

    // Convert 1-indexed offset to 0-indexed. offset==0 is treated as no offset,
    // matching pi's `offset ? Math.max(0, offset - 1) : 0`.
    let start_line = match offset {
        Some(o) if o > 0 => o - 1,
        _ => 0,
    };
    let start_line_display = start_line + 1;

    if start_line >= all_lines.len() {
        let shown = offset.unwrap_or(0);
        return Err(format!(
            "Offset {shown} is beyond end of file ({total_file_lines} lines total)"
        ));
    }

    let mut user_limited_lines: Option<usize> = None;
    let selected_content: String = if let Some(lim) = limit {
        let end_line = (start_line + lim).min(all_lines.len());
        user_limited_lines = Some(end_line - start_line);
        all_lines[start_line..end_line].join("\n")
    } else {
        all_lines[start_line..].join("\n")
    };

    let truncation = truncate_head(&selected_content, TruncationOptions::default());

    let mut details: Option<TruncationResult> = None;
    let text: String;

    if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len());
        text = format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES)
        );
        details = Some(truncation);
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines - 1;
        let next_offset = end_line_display + 1;
        let mut out = truncation.content.clone();
        if truncation.truncated_by == Some(TruncatedBy::Lines) {
            out += &format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            );
        } else {
            out += &format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            );
        }
        text = out;
        details = Some(truncation);
    } else if let Some(limited) = user_limited_lines {
        if start_line + limited < all_lines.len() {
            let remaining = all_lines.len() - (start_line + limited);
            let next_offset = start_line + limited + 1;
            text = format!(
                "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                truncation.content
            );
        } else {
            text = truncation.content.clone();
        }
    } else {
        text = truncation.content.clone();
    }

    Ok(ReadTextOutput { text, details })
}

// ---------------------------------------------------------------------------
// TUI render hooks (pi's `renderCall` / `renderResult`, `read.ts:329` / `:339`)
// ---------------------------------------------------------------------------

/// Local `theme.fg` wrapper falling back to unstyled text on an unknown color
/// key (pi's `theme.fg` is infallible; the ported [`Theme::fg`] returns a
/// `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// The path argument for display: `file_path` unless nullish, else `path`,
/// coerced through pi's `str` (mirrors `str(args?.file_path ?? args?.path)`).
fn read_path_arg(args: &Value) -> Option<String> {
    let raw = match args.get("file_path") {
        Some(v) if !v.is_null() => Some(v),
        _ => args.get("path"),
    };
    str_json(raw)
}

/// Format the `:start[-end]` line-range suffix (pi's `formatReadLineRange`).
fn format_read_line_range(args: &Value, theme: &Theme) -> String {
    if args.get("offset").is_none() && args.get("limit").is_none() {
        return String::new();
    }
    let start = args.get("offset").and_then(Value::as_i64).unwrap_or(1);
    let end = args
        .get("limit")
        .and_then(Value::as_i64)
        .map(|l| start + l - 1);
    let range = match end {
        Some(e) if e != 0 => format!(":{start}-{e}"),
        _ => format!(":{start}"),
    };
    fg(theme, "warning", &range)
}

/// Format the standard read call header (pi's `formatReadCall`).
fn format_read_call(args: &Value, theme: &Theme, cwd: &str) -> String {
    let path_display = render_tool_path(read_path_arg(args).as_deref(), theme, cwd, None);
    format!(
        "{} {}{}",
        fg(theme, "toolTitle", &theme.bold("read")),
        path_display,
        format_read_line_range(args, theme)
    )
}

/// The compact-read classification kind (pi's `CompactReadClassification.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactReadKind {
    /// pi's `docs` kind. Never constructed here: the only producer is the
    /// deferred pi-docs classification (needs the `config.ts` `getReadmePath`
    /// seam). Retained so the enum mirrors pi and `as_str` stays complete.
    #[allow(dead_code)]
    Docs,
    Resource,
    Skill,
}

impl CompactReadKind {
    fn as_str(self) -> &'static str {
        match self {
            CompactReadKind::Docs => "docs",
            CompactReadKind::Resource => "resource",
            CompactReadKind::Skill => "skill",
        }
    }
}

/// A compact-read classification (pi's `CompactReadClassification`).
struct CompactReadClassification {
    kind: CompactReadKind,
    label: String,
}

/// The resource file names pi treats as compact `resource` reads.
const COMPACT_RESOURCE_FILE_NAMES: [&str; 4] = ["AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// Classify a non-expanded read for the compact call header (pi's
/// `getCompactReadClassification`).
///
/// **Documented divergence — pi-docs branch.** pi additionally classifies reads
/// of its OWN bundled docs (`README.md`, `docs/…`, `examples/…`) via
/// `getPiDocsClassification`, which resolves pi's package root through
/// `config.ts`'s `getReadmePath()`. That config seam is not ported (see
/// [`crate::core::system_prompt`]), and pidgin is not the pi package, so the
/// pi-docs branch is intentionally omitted here. The `SKILL.md` (skill) and
/// `CLAUDE.md`/`AGENTS.md` (resource) branches — which need no package root —
/// are ported faithfully.
fn get_compact_read_classification(args: &Value, cwd: &str) -> Option<CompactReadClassification> {
    let raw_path = match read_path_arg(args) {
        Some(s) if !s.is_empty() => s,
        _ => return None,
    };
    let absolute_path = resolve_to_cwd(&raw_path, cwd).ok()?;
    let path = Path::new(&absolute_path);
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if file_name == "SKILL.md" {
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let label = if parent.is_empty() { file_name } else { parent };
        return Some(CompactReadClassification {
            kind: CompactReadKind::Skill,
            label,
        });
    }
    // pi-docs classification deferred (needs config.ts getReadmePath seam).
    if COMPACT_RESOURCE_FILE_NAMES.contains(&file_name.as_str()) {
        let label =
            format_path_relative_to_cwd_or_absolute(&absolute_path, cwd).unwrap_or(absolute_path);
        return Some(CompactReadClassification {
            kind: CompactReadKind::Resource,
            label,
        });
    }
    None
}

/// Format the compact read call header (pi's `formatCompactReadCall`).
fn format_compact_read_call(
    classification: &CompactReadClassification,
    args: &Value,
    theme: &Theme,
) -> String {
    let expand_hint = fg(
        theme,
        "dim",
        &format!(" ({} to expand)", tools_expand_key_text()),
    );
    if classification.kind == CompactReadKind::Skill {
        return format!(
            "{}{}{}{}",
            fg(theme, "customMessageLabel", "\x1b[1m[skill]\x1b[22m "),
            fg(theme, "customMessageText", &classification.label),
            format_read_line_range(args, theme),
            expand_hint
        );
    }
    format!(
        "{} {}{}{}",
        fg(
            theme,
            "toolTitle",
            &theme.bold(&format!("read {}", classification.kind.as_str()))
        ),
        fg(theme, "accent", &classification.label),
        format_read_line_range(args, theme),
        expand_hint
    )
}

/// Custom rendering for the read tool call (pi's `renderCall`, `read.ts:329`).
///
/// Stateless port: while not expanded, a `SKILL.md`/`CLAUDE.md`/`AGENTS.md`
/// path renders the compact header; otherwise the standard `read <path>` header.
pub fn read_render_call(
    args: &Value,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    let classification = if context.expanded {
        None
    } else {
        get_compact_read_classification(args, context.cwd)
    };
    let text = match classification {
        Some(c) => format_compact_read_call(&c, args, theme),
        None => format_read_call(args, theme, context.cwd),
    };
    Box::new(Text::new(&text, 0, 0, None))
}

/// Format the read result body (pi's `formatReadResult`). Empty unless expanded
/// or an error, matching pi's early return.
fn format_read_result(
    args: &Value,
    result: &AgentToolResult,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
    is_error: bool,
) -> String {
    if !options.expanded && !is_error {
        return String::new();
    }

    let raw_path = read_path_arg(args);
    let output = get_text_output_from_blocks(&result.content, show_images);
    let lang = if is_error {
        None
    } else {
        raw_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .and_then(get_language_from_path)
    };
    let rendered_lines: Vec<String> = match lang {
        Some(l) => highlight_code(&replace_tabs(&output), Some(l), theme),
        None => output.split('\n').map(str::to_string).collect(),
    };
    let lines = trim_trailing_empty_lines(&rendered_lines);
    let max_lines = if options.expanded { lines.len() } else { 10 };
    let display_lines = &lines[..max_lines.min(lines.len())];
    let remaining = lines.len() as isize - max_lines as isize;

    let body = display_lines
        .iter()
        .map(|line| {
            if lang.is_some() {
                replace_tabs(line)
            } else {
                fg(theme, "toolOutput", &replace_tabs(line))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut text = format!("\n{body}");

    if remaining > 0 {
        text += &fg(theme, "muted", &format!("\n... ({remaining} more lines,"));
        text += " ";
        text += &tools_expand_hint(theme, "to expand");
        text += &fg(theme, "muted", ")");
    }

    let truncation = result.details.get("truncation");
    let truncated = truncation
        .and_then(|t| t.get("truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(truncation) = truncation {
        if truncated {
            let first_line_exceeds = truncation
                .get("firstLineExceedsLimit")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let max_bytes = detail_usize(truncation, "maxBytes").unwrap_or(DEFAULT_MAX_BYTES);
            if first_line_exceeds {
                let notice = format!("[First line exceeds {} limit]", format_size(max_bytes));
                text += &format!("\n{}", fg(theme, "warning", &notice));
            } else if truncation.get("truncatedBy").and_then(Value::as_str) == Some("lines") {
                let output_lines = detail_usize(truncation, "outputLines").unwrap_or(0);
                let total_lines = detail_usize(truncation, "totalLines").unwrap_or(0);
                let max_lines = detail_usize(truncation, "maxLines").unwrap_or(DEFAULT_MAX_LINES);
                let notice = format!(
                    "[Truncated: showing {output_lines} of {total_lines} lines ({max_lines} line limit)]"
                );
                text += &format!("\n{}", fg(theme, "warning", &notice));
            } else {
                let output_lines = detail_usize(truncation, "outputLines").unwrap_or(0);
                let notice = format!(
                    "[Truncated: {output_lines} lines shown ({} limit)]",
                    format_size(max_bytes)
                );
                text += &format!("\n{}", fg(theme, "warning", &notice));
            }
        }
    }

    text
}

/// Custom rendering for the read tool result (pi's `renderResult`,
/// `read.ts:339`). The read tool uses the default shell, so this returns the
/// result body `Text` that the shell composes into its box.
pub fn read_render_result(
    result: &AgentToolResult,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    let text = format_read_result(
        context.args,
        result,
        options,
        theme,
        context.show_images,
        context.is_error,
    );
    Box::new(Text::new(&text, 0, 0, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_content_that_fits() {
        let content = "Hello, world!\nLine 2\nLine 3";
        let out = format_text_read(content, "test.txt", None, None).unwrap();
        assert_eq!(out.text, content);
        assert!(!out.text.contains("Use offset="));
        assert!(out.details.is_none());
    }

    #[test]
    fn truncates_files_exceeding_line_limit() {
        let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "large.txt", None, None).unwrap();
        assert!(out.text.contains("Line 1"));
        assert!(out.text.contains("Line 2000"));
        assert!(!out.text.contains("Line 2001"));
        assert!(out
            .text
            .contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
    }

    #[test]
    fn truncates_when_byte_limit_exceeded() {
        // 500 lines, each ~207 bytes -> exceeds 50KB before 2000 lines.
        let lines: Vec<String> = (1..=500)
            .map(|i| format!("Line {i}: {}", "x".repeat(200)))
            .collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "large-bytes.txt", None, None).unwrap();
        assert!(out.text.contains("Line 1:"));
        // Matches the byte-limit notice shape.
        let re = regex::Regex::new(
            r"\[Showing lines 1-\d+ of 500 \(.* limit\)\. Use offset=\d+ to continue\.\]",
        )
        .unwrap();
        assert!(re.is_match(&out.text), "notice not found in: {}", out.text);
    }

    #[test]
    fn handles_offset() {
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "offset.txt", Some(51), None).unwrap();
        assert!(!out.text.contains("Line 50"));
        assert!(out.text.contains("Line 51"));
        assert!(out.text.contains("Line 100"));
        assert!(!out.text.contains("Use offset="));
    }

    #[test]
    fn handles_limit() {
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "limit.txt", None, Some(10)).unwrap();
        assert!(out.text.contains("Line 1"));
        assert!(out.text.contains("Line 10"));
        assert!(!out.text.contains("Line 11"));
        assert!(out
            .text
            .contains("[90 more lines in file. Use offset=11 to continue.]"));
    }

    #[test]
    fn handles_offset_plus_limit() {
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "ol.txt", Some(41), Some(20)).unwrap();
        assert!(!out.text.contains("Line 40"));
        assert!(out.text.contains("Line 41"));
        assert!(out.text.contains("Line 60"));
        assert!(!out.text.contains("Line 61"));
        assert!(out
            .text
            .contains("[40 more lines in file. Use offset=61 to continue.]"));
    }

    #[test]
    fn errors_when_offset_beyond_end() {
        let content = "Line 1\nLine 2\nLine 3";
        let err = format_text_read(content, "short.txt", Some(100), None).unwrap_err();
        assert_eq!(err, "Offset 100 is beyond end of file (3 lines total)");
    }

    #[test]
    fn includes_truncation_details_when_truncated() {
        let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
        let content = lines.join("\n");
        let out = format_text_read(&content, "large.txt", None, None).unwrap();
        let details = out.details.expect("details present");
        assert!(details.truncated);
        assert_eq!(details.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(details.total_lines, 2500);
        assert_eq!(details.output_lines, 2000);
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

    fn ctx<'a>(args: &'a Value, expanded: bool, is_error: bool) -> ToolRenderContext<'a> {
        ToolRenderContext {
            args,
            cwd: "/tmp/tool-cwd",
            execution_started: true,
            args_complete: true,
            is_partial: false,
            expanded,
            show_images: false,
            is_error,
        }
    }

    fn text_result(text: &str) -> AgentToolResult {
        AgentToolResult {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                text_signature: None,
            }],
            details: Value::Null,
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
    fn call_renders_read_header_with_path() {
        let theme = dark_theme();
        let args = json!({ "path": "notes.txt" });
        let out = read_render_call(&args, &theme, &ctx(&args, false, false)).render(80);
        let joined = out.join("\n");
        assert!(joined.contains("read"), "got: {joined:?}");
        assert!(joined.contains("notes.txt"), "got: {joined:?}");
    }

    #[test]
    fn call_line_range_uses_offset_and_limit() {
        let theme = dark_theme();
        // offset=5, limit=10 -> `:5-14`.
        let args = json!({ "path": "notes.txt", "offset": 5, "limit": 10 });
        let range = format_read_line_range(&args, &theme);
        assert!(range.contains(":5-14"), "got: {range:?}");
        // offset only -> `:5` (no end).
        let args = json!({ "path": "notes.txt", "offset": 5 });
        assert!(format_read_line_range(&args, &theme).contains(":5"));
        // neither -> empty.
        let args = json!({ "path": "notes.txt" });
        assert_eq!(format_read_line_range(&args, &theme), "");
    }

    #[test]
    fn result_is_empty_when_collapsed_and_not_error() {
        let theme = dark_theme();
        let args = json!({ "path": "notes.txt" });
        let result = text_result("line one\nline two");
        let body = format_read_result(&args, &result, &opts(false), &theme, false, false);
        assert_eq!(body, "");
    }

    #[test]
    fn result_shows_body_when_expanded() {
        let theme = dark_theme();
        let args = json!({ "path": "notes.txt" });
        let result = text_result("line one\nline two");
        let body = format_read_result(&args, &result, &opts(true), &theme, false, false);
        assert!(body.starts_with('\n'));
        assert!(body.contains("line one"));
        assert!(body.contains("line two"));
    }

    #[test]
    fn result_shows_body_on_error_even_when_collapsed() {
        let theme = dark_theme();
        let args = json!({ "path": "missing.txt" });
        let result = text_result("ENOENT: no such file");
        let body = format_read_result(&args, &result, &opts(false), &theme, false, true);
        assert!(body.contains("ENOENT: no such file"), "got: {body:?}");
    }

    #[test]
    fn skill_md_uses_compact_skill_classification() {
        let args = json!({ "path": "my-skill/SKILL.md" });
        let classification =
            get_compact_read_classification(&args, "/tmp/tool-cwd").expect("skill classified");
        assert_eq!(classification.kind, CompactReadKind::Skill);
        assert_eq!(classification.label, "my-skill");
    }

    #[test]
    fn invalid_path_arg_shows_invalid_marker() {
        let theme = dark_theme();
        let args = json!({ "path": 42 });
        let joined = read_render_call(&args, &theme, &ctx(&args, false, false))
            .render(80)
            .join("\n");
        assert!(joined.contains("[invalid arg]"), "got: {joined:?}");
    }
}
