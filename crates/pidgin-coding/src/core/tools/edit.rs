//! Argument coercion, validation, and the pure edit pipeline for the edit tool.
//!
//! Ported from pi's `core/tools/edit.ts`. [`prepare_edit_arguments`] handles the
//! legacy `oldText`/`newText` input shape and edits-as-JSON-string coercion that
//! some models emit; [`validate_edit_input`] enforces a non-empty edits array.
//! [`compute_edit_result`] composes the pure edit-diff core (BOM strip, line
//! ending detect/normalize/restore, edit application, diff + patch generation)
//! over already-read file content.
//!
//! Deferred: the filesystem read/write, the per-file mutation queue, and the
//! abort plumbing. Those need an execution environment, so the tool's `execute`
//! shell is not ported here (it lives in `definitions.rs`).
//!
//! The TUI render hooks ([`edit_render_call`]/[`edit_render_result`]) are ported
//! here as **stateless** functions: pi's renderers thread a mutable
//! `EditCallRenderComponent` (async file-read preview, `previewPending`,
//! `settledError`) through `context.state`/`context.lastComponent`. That state
//! is intentionally omitted (see [`ToolRenderContext`]), so the call render is
//! the pending header (no async diff preview) and the result render shows the
//! result's own `details.diff`.

use serde_json::{Map, Value};

use pidgin_agent::types::AgentToolResult;
use pidgin_ai::ContentBlock;
use pidgin_tui::renderer::{Component, Container};
use pidgin_tui::widgets::box_widget::BoxWidget;
use pidgin_tui::widgets::text::BgFn;
use pidgin_tui::{Spacer, Text};

use crate::core::extensions::types::{ToolRenderContext, ToolRenderResultOptions};
use crate::modes::interactive::theme::runtime::Theme;

use super::diff_render::render_diff;
use super::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom, Edit,
};
use super::render_utils::{render_tool_path, str_json};

/// Coerce raw tool input into the canonical `{ path, edits }` shape.
///
/// - `edits` supplied as a JSON string is parsed into an array when valid.
/// - Legacy top-level `oldText`/`newText` (both strings) are folded into a
///   trailing `edits[]` entry and removed from the object.
/// - Non-object input and already-valid input pass through unchanged.
pub fn prepare_edit_arguments(input: Value) -> Value {
    let mut args = match input {
        Value::Object(m) => m,
        other => return other,
    };

    // Some models send edits as a JSON string instead of an array.
    if let Some(Value::String(s)) = args.get("edits").cloned() {
        if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
            if parsed.is_array() {
                args.insert("edits".to_string(), parsed);
            }
        }
    }

    let old_is_str = matches!(args.get("oldText"), Some(Value::String(_)));
    let new_is_str = matches!(args.get("newText"), Some(Value::String(_)));
    if !(old_is_str && new_is_str) {
        return Value::Object(args);
    }

    let old_text = args.get("oldText").cloned().unwrap();
    let new_text = args.get("newText").cloned().unwrap();
    let mut edits = match args.get("edits") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let mut edit_obj = Map::new();
    edit_obj.insert("oldText".to_string(), old_text);
    edit_obj.insert("newText".to_string(), new_text);
    edits.push(Value::Object(edit_obj));

    args.remove("oldText");
    args.remove("newText");
    args.insert("edits".to_string(), Value::Array(edits));
    Value::Object(args)
}

/// The validated `{ path, edits }` extracted from prepared input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEditInput {
    /// Target file path.
    pub path: String,
    /// The replacements to apply.
    pub edits: Vec<Edit>,
}

/// Validate prepared input: `edits` must be a non-empty array of
/// `{ oldText, newText }` objects.
pub fn validate_edit_input(input: &Value) -> Result<ValidatedEditInput, String> {
    let edits_val = input.get("edits");
    let edits_arr = match edits_val {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => {
            return Err(
                "Edit tool input is invalid. edits must contain at least one replacement."
                    .to_string(),
            );
        }
    };

    let mut edits = Vec::with_capacity(edits_arr.len());
    for e in edits_arr {
        let old_text = e.get("oldText").and_then(Value::as_str).ok_or_else(|| {
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string()
        })?;
        let new_text = e.get("newText").and_then(Value::as_str).ok_or_else(|| {
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string()
        })?;
        edits.push(Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        });
    }

    let path = input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(ValidatedEditInput { path, edits })
}

/// The pure result of applying edits to file content (no filesystem write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditComputation {
    /// The full new file content, with BOM and original line endings restored.
    pub final_content: String,
    /// Display-oriented diff.
    pub diff: String,
    /// 1-based first changed line in the new file.
    pub first_changed_line: Option<usize>,
    /// jsdiff-compatible unified patch.
    pub patch: String,
    /// The success message the tool returns.
    pub message: String,
}

/// Compute the edit result for `raw_content` (the file's contents as read).
///
/// Mirrors pi's `execute` minus the filesystem: strip BOM, detect the original
/// line ending, LF-normalize, apply edits, then restore endings and BOM.
pub fn compute_edit_result(
    raw_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<EditComputation, String> {
    let stripped = strip_bom(raw_content);
    let original_ending = detect_line_ending(&stripped.text);
    let normalized = normalize_to_lf(&stripped.text);
    let applied = apply_edits_to_normalized_content(&normalized, edits, path)?;
    let final_content = format!(
        "{}{}",
        stripped.bom,
        restore_line_endings(&applied.new_content, original_ending)
    );
    let diff = generate_diff_string(&applied.base_content, &applied.new_content, 4);
    let patch = generate_unified_patch(path, &applied.base_content, &applied.new_content, 4);
    Ok(EditComputation {
        final_content,
        diff: diff.diff,
        first_changed_line: diff.first_changed_line,
        patch,
        message: format!(
            "Successfully replaced {} block(s) in {}.",
            edits.len(),
            path
        ),
    })
}

// ---------------------------------------------------------------------------
// TUI render hooks (pi's `renderCall` / `renderResult`, `edit.ts:363` / `:377`)
// ---------------------------------------------------------------------------

/// Local `theme.fg` wrapper falling back to unstyled text on an unknown color
/// key (pi's `theme.fg` is infallible; the ported [`Theme::fg`] returns a
/// `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// A `theme.bg(color, …)` background function over an owned ANSI escape, so the
/// resulting [`BgFn`] is `'static` (pi's `(text) => theme.bg(color, text)`).
fn bg_fn(theme: &Theme, color: &str) -> BgFn {
    let ansi = theme.get_bg_ansi(color).unwrap_or_default();
    Box::new(move |text: &str| format!("{ansi}{text}\x1b[49m"))
}

/// The path argument for display: `file_path` unless nullish, else `path`,
/// coerced through pi's `str` (mirrors `str(args?.file_path ?? args?.path)`).
fn edit_path_arg(args: &Value) -> Option<String> {
    let raw = match args.get("file_path") {
        Some(v) if !v.is_null() => Some(v),
        _ => args.get("path"),
    };
    str_json(raw)
}

/// Format the edit call header line (pi's `formatEditCall`).
fn format_edit_call(args: &Value, theme: &Theme, cwd: &str) -> String {
    let path_display = render_tool_path(edit_path_arg(args).as_deref(), theme, cwd, None);
    format!(
        "{} {}",
        fg(theme, "toolTitle", &theme.bold("edit")),
        path_display
    )
}

/// Custom rendering for the edit tool call (pi's `renderCall`, `edit.ts:363`).
///
/// Stateless port: pi computes an async diff preview into a stateful call
/// component; without that state this renders only the pending header
/// (`toolPendingBg`), which is the synchronous portion of pi's output.
pub fn edit_render_call(
    args: &Value,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    let mut boxed = BoxWidget::new(1, 1, Some(bg_fn(theme, "toolPendingBg")));
    boxed.add_child(Box::new(Text::new(
        &format_edit_call(args, theme, context.cwd),
        0,
        0,
        None,
    )));
    Box::new(boxed)
}

/// The displayable text of a tool result's text blocks, joined by newlines
/// (pi's `result.content.filter(text).map(c.text || "").join("\n")`).
fn result_text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format the edit result body (pi's `formatEditResult`), stateless: the call
/// component's async preview is unavailable, so `preview` is treated as absent.
/// Returns `None` when there is nothing to render (pi's `undefined`).
fn format_edit_result(result: &AgentToolResult, theme: &Theme, is_error: bool) -> Option<String> {
    if is_error {
        let error_text = result_text(result);
        if error_text.is_empty() {
            return None;
        }
        return Some(fg(theme, "error", &error_text));
    }

    let result_diff = result.details.get("diff").and_then(Value::as_str);
    match result_diff {
        Some(diff) if !diff.is_empty() => Some(render_diff(diff, theme)),
        _ => None,
    }
}

/// Custom rendering for the edit tool result (pi's `renderResult`,
/// `edit.ts:377`).
///
/// Stateless port: pi also reconciles the call component's preview/error state
/// as a side effect; that state is omitted here, leaving the returned
/// container — a spacer plus the formatted body (diff panel or error text), or
/// empty when there is nothing to show.
pub fn edit_render_result(
    result: &AgentToolResult,
    _options: &ToolRenderResultOptions,
    theme: &Theme,
    context: &ToolRenderContext,
) -> Box<dyn Component> {
    let mut component = Container::new();
    let Some(output) = format_edit_result(result, theme, context.is_error) else {
        return Box::new(component);
    };
    component.add_child(Box::new(Spacer::new(1)));
    component.add_child(Box::new(Text::new(&output, 1, 0, None)));
    Box::new(component)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(old: &str, new: &str) -> Edit {
        Edit {
            old_text: old.to_string(),
            new_text: new.to_string(),
        }
    }

    // --- prepare_edit_arguments (pi edit-legacy-input.test.ts) ---

    #[test]
    fn folds_top_level_old_new_into_edits() {
        let prepared = prepare_edit_arguments(json!({
            "path": "file.txt",
            "oldText": "before",
            "newText": "after",
        }));
        assert_eq!(
            prepared,
            json!({ "path": "file.txt", "edits": [{ "oldText": "before", "newText": "after" }] })
        );
    }

    #[test]
    fn appends_legacy_replacement_to_existing_edits() {
        let prepared = prepare_edit_arguments(json!({
            "path": "file.txt",
            "edits": [{ "oldText": "a", "newText": "b" }],
            "oldText": "c",
            "newText": "d",
        }));
        assert_eq!(
            prepared,
            json!({
                "path": "file.txt",
                "edits": [
                    { "oldText": "a", "newText": "b" },
                    { "oldText": "c", "newText": "d" }
                ]
            })
        );
    }

    #[test]
    fn passes_through_valid_input_unchanged() {
        let input = json!({ "path": "file.txt", "edits": [{ "oldText": "a", "newText": "b" }] });
        assert_eq!(prepare_edit_arguments(input.clone()), input);
    }

    #[test]
    fn passes_through_non_object_input_unchanged() {
        assert_eq!(prepare_edit_arguments(Value::Null), Value::Null);
        assert_eq!(prepare_edit_arguments(json!("garbage")), json!("garbage"));
    }

    #[test]
    fn parses_edits_from_json_string() {
        let prepared = prepare_edit_arguments(json!({
            "path": "file.txt",
            "edits": "[{\"oldText\": \"a\", \"newText\": \"b\"}]",
        }));
        assert_eq!(
            prepared,
            json!({ "path": "file.txt", "edits": [{ "oldText": "a", "newText": "b" }] })
        );
    }

    #[test]
    fn leaves_edits_alone_when_not_valid_json() {
        let prepared = prepare_edit_arguments(json!({ "path": "file.txt", "edits": "not json" }));
        assert_eq!(prepared, json!({ "path": "file.txt", "edits": "not json" }));
    }

    // --- validate_edit_input ---

    #[test]
    fn validate_rejects_empty_edits() {
        let err = validate_edit_input(&json!({ "path": "f.txt", "edits": [] })).unwrap_err();
        assert!(err.contains("edits must contain at least one replacement"));
    }

    #[test]
    fn validate_extracts_path_and_edits() {
        let v = validate_edit_input(&json!({
            "path": "f.txt",
            "edits": [{ "oldText": "a", "newText": "b" }]
        }))
        .unwrap();
        assert_eq!(v.path, "f.txt");
        assert_eq!(v.edits, vec![edit("a", "b")]);
    }

    #[test]
    fn prepared_args_validate_and_compute() {
        let prepared = prepare_edit_arguments(json!({
            "path": "legacy.txt",
            "oldText": "before",
            "newText": "after",
        }));
        let validated = validate_edit_input(&prepared).unwrap();
        let result = compute_edit_result("before\n", &validated.edits, &validated.path).unwrap();
        assert_eq!(
            result.message,
            "Successfully replaced 1 block(s) in legacy.txt."
        );
        assert_eq!(result.final_content, "after\n");
    }

    // --- compute_edit_result: CRLF / BOM handling (pi tools.test.ts) ---

    #[test]
    fn matches_lf_old_text_against_crlf_file() {
        let result = compute_edit_result(
            "line one\r\nline two\r\nline three\r\n",
            &[edit("line two\n", "replaced line\n")],
            "f.txt",
        )
        .unwrap();
        assert!(result.message.contains("Successfully replaced"));
    }

    #[test]
    fn preserves_crlf_after_edit() {
        let result = compute_edit_result(
            "first\r\nsecond\r\nthird\r\n",
            &[edit("second\n", "REPLACED\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.final_content, "first\r\nREPLACED\r\nthird\r\n");
    }

    #[test]
    fn preserves_lf_for_lf_files() {
        let result = compute_edit_result(
            "first\nsecond\nthird\n",
            &[edit("second\n", "REPLACED\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.final_content, "first\nREPLACED\nthird\n");
    }

    #[test]
    fn detects_duplicates_across_crlf_lf_variants() {
        let err = compute_edit_result(
            "hello\r\nworld\r\n---\r\nhello\nworld\n",
            &[edit("hello\nworld\n", "replaced\n")],
            "f.txt",
        )
        .unwrap_err();
        assert!(err.contains("Found 2 occurrences"));
    }

    #[test]
    fn preserves_utf8_bom_after_edit() {
        let result = compute_edit_result(
            "\u{FEFF}first\r\nsecond\r\nthird\r\n",
            &[edit("second\n", "REPLACED\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(
            result.final_content,
            "\u{FEFF}first\r\nREPLACED\r\nthird\r\n"
        );
    }

    #[test]
    fn preserves_crlf_and_bom_in_multi_edit() {
        let result = compute_edit_result(
            "\u{FEFF}first\r\nsecond\r\nthird\r\nfourth\r\n",
            &[edit("second\n", "SECOND\n"), edit("fourth\n", "FOURTH\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(
            result.final_content,
            "\u{FEFF}first\r\nSECOND\r\nthird\r\nFOURTH\r\n"
        );
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode};
    use serde_json::json;
    use std::path::PathBuf;

    /// The 256-color dark theme, loaded the same way the interactive vector
    /// tests build it.
    fn dark_theme() -> Theme {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/modes/interactive/theme/dark.json");
        let content = std::fs::read_to_string(&path).expect("read dark.json");
        let json = parse_theme_json(&content).expect("parse dark.json");
        create_theme(&json, Some(ColorMode::Color256), None).expect("create dark theme")
    }

    fn sample_args() -> Value {
        json!({ "path": "src/main.rs", "edits": [{ "oldText": "a", "newText": "b" }] })
    }

    fn ctx<'a>(args: &'a Value, is_error: bool) -> ToolRenderContext<'a> {
        ToolRenderContext {
            args,
            cwd: "/home/zack/proj",
            execution_started: true,
            args_complete: true,
            is_partial: false,
            expanded: false,
            show_images: false,
            is_error,
        }
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

    #[test]
    fn render_call_pending_header_byte_exact() {
        let theme = dark_theme();
        let args = sample_args();
        let call = edit_render_call(&args, &theme, &ctx(&args, false));

        assert_eq!(
            call.render(40),
            vec![
                "\u{1b}[48;5;17m                                        \u{1b}[49m",
                "\u{1b}[48;5;17m \u{1b}[38;5;188m\u{1b}[1medit\u{1b}[22m\u{1b}[39m \u{1b}[38;5;109msrc/main.rs\u{1b}[39m                       \u{1b}[49m",
                "\u{1b}[48;5;17m                                        \u{1b}[49m",
            ]
        );
        assert_eq!(
            call.render(80),
            vec![
                "\u{1b}[48;5;17m                                                                                \u{1b}[49m",
                "\u{1b}[48;5;17m \u{1b}[38;5;188m\u{1b}[1medit\u{1b}[22m\u{1b}[39m \u{1b}[38;5;109msrc/main.rs\u{1b}[39m                                                               \u{1b}[49m",
                "\u{1b}[48;5;17m                                                                                \u{1b}[49m",
            ]
        );
    }

    #[test]
    fn render_result_diff_panel_byte_exact() {
        let theme = dark_theme();
        let args = sample_args();
        let opts = ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        };
        let result = text_result(
            "Successfully replaced 1 block(s) in src/main.rs.",
            json!({ "diff": " 1 unchanged\n-2 old line\n+2 new line" }),
        );
        let comp = edit_render_result(&result, &opts, &theme, &ctx(&args, false));

        assert_eq!(
            comp.render(40),
            vec![
                "".to_string(),
                " \u{1b}[38;5;244m 1 unchanged\u{1b}[39m                           ".to_string(),
                " \u{1b}[38;5;167m-2 old line\u{1b}[39m                            ".to_string(),
                " \u{1b}[38;5;143m+2 new line\u{1b}[39m                            ".to_string(),
            ]
        );
        assert_eq!(
            comp.render(80),
            vec![
                "".to_string(),
                " \u{1b}[38;5;244m 1 unchanged\u{1b}[39m                                                                   ".to_string(),
                " \u{1b}[38;5;167m-2 old line\u{1b}[39m                                                                    ".to_string(),
                " \u{1b}[38;5;143m+2 new line\u{1b}[39m                                                                    ".to_string(),
            ]
        );
    }

    #[test]
    fn render_result_error_text_byte_exact() {
        let theme = dark_theme();
        let args = sample_args();
        let opts = ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        };
        let err = text_result("Could not edit file: x.", json!({}));
        let comp = edit_render_result(&err, &opts, &theme, &ctx(&args, true));

        assert_eq!(
            comp.render(80),
            vec![
                "".to_string(),
                " \u{1b}[38;5;167mCould not edit file: x.\u{1b}[39m                                                        ".to_string(),
            ]
        );
    }

    #[test]
    fn render_result_no_diff_no_error_is_empty() {
        let theme = dark_theme();
        let args = sample_args();
        let opts = ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        };
        // Success with no `details.diff` → nothing to render (empty container).
        let result = text_result("done", Value::Null);
        let comp = edit_render_result(&result, &opts, &theme, &ctx(&args, false));
        assert!(comp.render(80).is_empty());
    }

    #[test]
    fn render_call_invalid_path_arg_shows_invalid_marker() {
        let theme = dark_theme();
        // A numeric `path` is a wrong-type arg → pi's `str` returns null →
        // `[invalid arg]`.
        let args = json!({ "path": 42 });
        let call = edit_render_call(&args, &theme, &ctx(&args, false));
        let joined = call.render(80).join("\n");
        assert!(joined.contains("[invalid arg]"), "got: {joined:?}");
    }
}
