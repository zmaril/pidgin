// straitjacket-allow-file:duplication — the `load()` vector-reading helper and
// the per-scenario replay loop intentionally mirror the two-line boilerplate in
// input_list_vectors.rs / widget_vectors.rs; each integration-test binary is
// standalone and cannot share a private helper without a common module.
//! Drives the Rust port of pi's Editor CORE against vectors extracted from pi
//! itself (`crates/atilla-tui/vectors/gen/generate_editor.mjs`). Every assertion
//! is byte-identical: pi's `render(width)`, `getText`, `getCursor`,
//! `getExpandedText`, `getLines`, `isShowingAutocomplete`, and `onSubmit` are
//! the source of truth. The scenarios replay every `test/editor.test.ts` block
//! except the async Autocomplete block (deferred to C6b).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use serde::Deserialize;

use atilla_tui::editor::{word_wrap_line, Segment};
use atilla_tui::{Editor, EditorOptions, EditorTheme, SelectListTheme};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

// Editor theme matching the generator: chalk.dim borders (\x1b[2m … \x1b[22m).
fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Box::new(|t: &str| format!("\x1b[2m{t}\x1b[22m")),
        select_list: SelectListTheme {
            selected_prefix: Box::new(|t: &str| t.to_string()),
            selected_text: Box::new(|t: &str| t.to_string()),
            description: Box::new(|t: &str| t.to_string()),
            scroll_info: Box::new(|t: &str| t.to_string()),
            no_match: Box::new(|t: &str| t.to_string()),
        },
    }
}

#[derive(Deserialize)]
struct JsonOptions {
    #[serde(default, rename = "paddingX")]
    padding_x: Option<i64>,
    #[serde(default, rename = "autocompleteMaxVisible")]
    autocomplete_max_visible: Option<i64>,
}

#[derive(Deserialize)]
struct Step {
    op: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    focused: Option<bool>,
    #[serde(default)]
    width: Option<usize>,
    // Recorded outputs.
    #[serde(rename = "textAfter")]
    text_after: String,
    line: usize,
    col: usize,
    showing: bool,
    #[serde(default)]
    render: Option<Vec<String>>,
    #[serde(default)]
    expanded: Option<String>,
    #[serde(default)]
    lines: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct EditorScenario {
    name: String,
    rows: usize,
    #[serde(default)]
    options: Option<JsonOptions>,
    #[serde(rename = "disableSubmit")]
    disable_submit: bool,
    steps: Vec<Step>,
    submits: Vec<String>,
}

#[test]
fn editor_scenario_vectors() {
    let scenarios: Vec<EditorScenario> = load("editor_scenarios");
    assert!(!scenarios.is_empty());

    for sc in &scenarios {
        let submits: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

        let opts = EditorOptions {
            padding_x: sc.options.as_ref().and_then(|o| o.padding_x),
            autocomplete_max_visible: sc.options.as_ref().and_then(|o| o.autocomplete_max_visible),
        };
        let mut editor = Editor::new(editor_theme(), opts);
        editor.set_terminal_rows(sc.rows);
        editor.disable_submit = sc.disable_submit;
        {
            let submits = Rc::clone(&submits);
            editor.on_submit = Some(Box::new(move |t: String| submits.borrow_mut().push(t)));
        }

        for (i, step) in sc.steps.iter().enumerate() {
            let mut got_render: Option<Vec<String>> = None;
            let mut got_expanded: Option<String> = None;
            let mut got_lines: Option<Vec<String>> = None;
            match step.op.as_str() {
                "input" => editor.handle_input_str(step.data.as_deref().expect("input data")),
                "setText" => editor.set_text(step.text.as_deref().expect("setText text")),
                "insertTextAtCursor" => {
                    editor.insert_text_at_cursor(step.text.as_deref().expect("insert text"))
                }
                "addToHistory" => {
                    editor.add_to_history(step.text.as_deref().expect("history text"))
                }
                "focus" => editor.focused = step.focused.expect("focused arg"),
                "render" => {
                    got_render = Some(editor.render_lines(step.width.expect("render width")))
                }
                "expandedText" => got_expanded = Some(editor.get_expanded_text()),
                "lines" => got_lines = Some(editor.get_lines()),
                other => panic!("unknown editor op: {other}"),
            }

            let cursor = editor.get_cursor();
            assert_eq!(
                editor.get_text(),
                step.text_after,
                "scenario {:?} step {i} ({}): getText mismatch",
                sc.name,
                step.op
            );
            assert_eq!(
                (cursor.line, cursor.col),
                (step.line, step.col),
                "scenario {:?} step {i} ({}): cursor mismatch",
                sc.name,
                step.op
            );
            assert_eq!(
                editor.is_showing_autocomplete(),
                step.showing,
                "scenario {:?} step {i}: showing mismatch",
                sc.name
            );
            if let Some(expected) = &step.render {
                assert_eq!(
                    got_render.as_ref().expect("render captured"),
                    expected,
                    "scenario {:?} step {i}: render mismatch",
                    sc.name
                );
            }
            if let Some(expected) = &step.expanded {
                assert_eq!(
                    got_expanded.as_ref().expect("expanded captured"),
                    expected,
                    "scenario {:?} step {i}: expandedText mismatch",
                    sc.name
                );
            }
            if let Some(expected) = &step.lines {
                assert_eq!(
                    got_lines.as_ref().expect("lines captured"),
                    expected,
                    "scenario {:?} step {i}: getLines mismatch",
                    sc.name
                );
            }
        }

        assert_eq!(
            *submits.borrow(),
            sc.submits,
            "scenario {:?}: onSubmit sequence mismatch",
            sc.name
        );
    }
}

// ===========================================================================
// wordWrapLine direct (pure function) vectors
// ===========================================================================

#[derive(Deserialize)]
struct JsonSegment {
    segment: String,
    index: usize,
}

#[derive(Deserialize)]
struct JsonChunk {
    text: String,
    #[serde(rename = "startIndex")]
    start_index: usize,
    #[serde(rename = "endIndex")]
    end_index: usize,
}

#[derive(Deserialize)]
struct WrapVector {
    name: String,
    line: String,
    #[serde(rename = "maxWidth")]
    max_width: i64,
    segments: Option<Vec<JsonSegment>>,
    chunks: Vec<JsonChunk>,
}

#[test]
fn editor_wordwrap_vectors() {
    let vectors: Vec<WrapVector> = load("editor_wordwrap");
    assert!(!vectors.is_empty());

    for v in &vectors {
        let segments: Option<Vec<Segment>> = v.segments.as_ref().map(|segs| {
            segs.iter()
                .map(|s| Segment {
                    segment: s.segment.clone(),
                    index: s.index,
                })
                .collect()
        });
        let chunks = word_wrap_line(&v.line, v.max_width, segments.as_deref());

        assert_eq!(
            chunks.len(),
            v.chunks.len(),
            "wordWrapLine {:?}: chunk count mismatch",
            v.name
        );
        for (j, (got, expected)) in chunks.iter().zip(v.chunks.iter()).enumerate() {
            assert_eq!(
                got.text, expected.text,
                "wordWrapLine {:?} chunk {j}: text mismatch",
                v.name
            );
            assert_eq!(
                (got.start_index, got.end_index),
                (expected.start_index, expected.end_index),
                "wordWrapLine {:?} chunk {j}: index mismatch",
                v.name
            );
        }
    }
}
