// straitjacket-allow-file:duplication — the `load()` vector-reading helper and
// the per-scenario replay loop intentionally mirror the two-line boilerplate in
// input_list_vectors.rs / widget_vectors.rs; each integration-test binary is
// standalone and cannot share a private helper without a common module.
//! Drives the Rust port of pi's Editor CORE against vectors extracted from pi
//! itself (`crates/pidgin-tui/vectors/gen/generate_editor.mjs`). Every assertion
//! is byte-identical: pi's `render(width)`, `getText`, `getCursor`,
//! `getExpandedText`, `getLines`, `isShowingAutocomplete`, and `onSubmit` are
//! the source of truth. The scenarios replay every `test/editor.test.ts` block,
//! including the async Autocomplete block (via the recorded-provider table + the
//! `flush_autocomplete()` seam — see `editor_autocomplete_vectors`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use serde::Deserialize;

use pidgin_tui::editor::{word_wrap_line, Segment};
use pidgin_tui::{
    AppliedCompletion, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, Editor,
    EditorOptions, EditorTheme, SelectListTheme, SuggestionOutcome,
};

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

// ===========================================================================
// Autocomplete integration vectors (the flush-seam two-phase machine).
//
// The provider is a RECORDED TABLE: pi's own Editor was driven through the
// Autocomplete describe block (+ the Undo block's "undoes autocomplete" case),
// recording each `(text, cursorLine, cursorCol, force) -> suggestions` and
// `(text, cursorLine, cursorCol, item, prefix) -> applied` response, and pi's
// `flushAutocomplete()` was called at every assert point. This replay injects
// that table and calls `flush_autocomplete()` at the same points, asserting
// byte-identical getText / getCursor / isShowingAutocomplete / render / onSubmit
// and the provider call count (proving "N keystrokes = 1 call" coalescing).
// The one wall-clock case (abort count) is the unit test below.
// ===========================================================================

#[derive(Deserialize, Clone)]
struct JsonAcItem {
    value: String,
    label: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, Clone)]
struct JsonAcSugg {
    items: Vec<JsonAcItem>,
    prefix: String,
}

#[derive(Deserialize)]
struct JsonSuggRow {
    text: String,
    line: usize,
    col: usize,
    force: bool,
    result: Option<JsonAcSugg>,
}

#[derive(Deserialize)]
struct JsonApplyResult {
    text: String,
    line: usize,
    col: usize,
}

#[derive(Deserialize)]
struct JsonApplyRow {
    text: String,
    line: usize,
    col: usize,
    #[serde(rename = "itemValue")]
    item_value: String,
    prefix: String,
    result: JsonApplyResult,
}

#[derive(Deserialize)]
struct JsonShouldRow {
    text: String,
    line: usize,
    col: usize,
    result: bool,
}

#[derive(Deserialize)]
struct AutoStep {
    op: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    width: Option<usize>,
    #[serde(rename = "textAfter")]
    text_after: String,
    line: usize,
    col: usize,
    showing: bool,
    #[serde(default)]
    render: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct AutoScenario {
    name: String,
    rows: usize,
    #[serde(default)]
    options: Option<JsonOptions>,
    #[serde(rename = "disableSubmit")]
    disable_submit: bool,
    #[serde(rename = "triggerCharacters")]
    trigger_characters: Vec<String>,
    steps: Vec<AutoStep>,
    submits: Vec<String>,
    suggestions: Vec<JsonSuggRow>,
    applies: Vec<JsonApplyRow>,
    #[serde(rename = "shouldTrigger")]
    should_trigger: Vec<JsonShouldRow>,
    #[serde(rename = "suggestionCallCount")]
    suggestion_call_count: usize,
}

fn to_suggestions(s: &JsonAcSugg) -> AutocompleteSuggestions {
    AutocompleteSuggestions {
        items: s
            .items
            .iter()
            .map(|it| AutocompleteItem {
                value: it.value.clone(),
                label: it.label.clone(),
                description: it.description.clone(),
            })
            .collect(),
        prefix: s.prefix.clone(),
    }
}

/// A provider backed entirely by recorded lookup tables — no fd/timers/network.
struct RecordedProvider {
    trigger_characters: Vec<String>,
    suggestions: HashMap<(String, usize, usize, bool), Option<AutocompleteSuggestions>>,
    applies: HashMap<(String, usize, usize, String, String), AppliedCompletion>,
    should_trigger: HashMap<(String, usize, usize), bool>,
    calls: Rc<Cell<usize>>,
}

impl AutocompleteProvider for RecordedProvider {
    fn trigger_characters(&self) -> Vec<String> {
        self.trigger_characters.clone()
    }

    fn get_suggestions(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> SuggestionOutcome {
        self.calls.set(self.calls.get() + 1);
        let key = (lines.join("\n"), cursor_line, cursor_col, force);
        let res = self
            .suggestions
            .get(&key)
            .unwrap_or_else(|| panic!("no recorded getSuggestions for {key:?}"))
            .clone();
        SuggestionOutcome::Ready(res)
    }

    fn apply_completion(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> AppliedCompletion {
        let key = (
            lines.join("\n"),
            cursor_line,
            cursor_col,
            item.value.clone(),
            prefix.to_string(),
        );
        self.applies
            .get(&key)
            .unwrap_or_else(|| panic!("no recorded applyCompletion for {key:?}"))
            .clone()
    }

    fn should_trigger_file_completion(
        &mut self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> Option<bool> {
        self.should_trigger
            .get(&(lines.join("\n"), cursor_line, cursor_col))
            .copied()
    }
}

#[test]
fn editor_autocomplete_vectors() {
    let scenarios: Vec<AutoScenario> = load("editor_autocomplete");
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

        let suggestions: HashMap<_, _> = sc
            .suggestions
            .iter()
            .map(|r| {
                (
                    (r.text.clone(), r.line, r.col, r.force),
                    r.result.as_ref().map(to_suggestions),
                )
            })
            .collect();
        let applies: HashMap<_, _> = sc
            .applies
            .iter()
            .map(|r| {
                (
                    (
                        r.text.clone(),
                        r.line,
                        r.col,
                        r.item_value.clone(),
                        r.prefix.clone(),
                    ),
                    AppliedCompletion {
                        lines: r.result.text.split('\n').map(str::to_string).collect(),
                        cursor_line: r.result.line,
                        cursor_col: r.result.col,
                    },
                )
            })
            .collect();
        let should_trigger: HashMap<_, _> = sc
            .should_trigger
            .iter()
            .map(|r| ((r.text.clone(), r.line, r.col), r.result))
            .collect();

        let calls = Rc::new(Cell::new(0usize));
        editor.set_autocomplete_provider(Box::new(RecordedProvider {
            trigger_characters: sc.trigger_characters.clone(),
            suggestions,
            applies,
            should_trigger,
            calls: Rc::clone(&calls),
        }));

        for (i, step) in sc.steps.iter().enumerate() {
            let mut got_render: Option<Vec<String>> = None;
            match step.op.as_str() {
                "input" => editor.handle_input_str(step.data.as_deref().expect("input data")),
                "setText" => editor.set_text(step.text.as_deref().expect("setText text")),
                "flush" => editor.flush_autocomplete(),
                "render" => {
                    got_render = Some(editor.render_lines(step.width.expect("render width")))
                }
                other => panic!("unknown auto op: {other}"),
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
                "scenario {:?} step {i} ({}): showing mismatch",
                sc.name,
                step.op
            );
            if let Some(expected) = &step.render {
                assert_eq!(
                    got_render.as_ref().expect("render captured"),
                    expected,
                    "scenario {:?} step {i}: render mismatch",
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
        assert_eq!(
            calls.get(),
            sc.suggestion_call_count,
            "scenario {:?}: getSuggestions call count mismatch (coalescing)",
            sc.name
        );
    }
}

// The one wall-clock case ("aborts active @ autocomplete when typing continues"):
// a provider whose promise never resolves except on abort. Superseding an
// in-flight request must abort it exactly once. Modeled natively: flush starts
// the request (provider returns Pending -> stays in-flight); the next keystroke
// supersedes it, incrementing the abort counter.
struct PendingProvider;

impl AutocompleteProvider for PendingProvider {
    fn get_suggestions(
        &mut self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
        _force: bool,
    ) -> SuggestionOutcome {
        SuggestionOutcome::Pending
    }

    fn apply_completion(
        &mut self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
        _item: &AutocompleteItem,
        _prefix: &str,
    ) -> AppliedCompletion {
        unreachable!("PendingProvider never applies")
    }
}

#[test]
fn autocomplete_aborts_in_flight_on_superseding_keystroke() {
    let mut editor = Editor::new(editor_theme(), EditorOptions::default());
    editor.set_autocomplete_provider(Box::new(PendingProvider));

    // Type "@main": each keystroke enqueues a superseding pending request.
    editor.handle_input_str("@");
    editor.handle_input_str("m");
    editor.handle_input_str("a");
    editor.handle_input_str("i");

    // Start the surviving request: the provider never resolves, so it stays
    // in-flight (no abort yet).
    editor.flush_autocomplete();
    assert_eq!(editor.autocomplete_abort_count(), 0);

    // Typing continues: this supersedes the in-flight request, aborting it once.
    editor.handle_input_str("n");
    assert_eq!(editor.autocomplete_abort_count(), 1);
}
