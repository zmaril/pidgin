//! Replays the PR-R2 overlay-compositing and focus-restore vectors extracted
//! from pi's own `TUI` (`vectors/gen/generate_renderer_r2.mjs`, driven through
//! the `@xterm/headless` harness) and asserts the Rust renderer emits a
//! byte-identical write stream plus identical renderer state, focus snapshot,
//! and input-delivery log at every step. pi is the source of truth.
//!
//! `renderer_overlays.json` covers overlay compositing (resolveOverlayLayout,
//! compositeOverlays, compositeLineAt, anchors/margins/offsets/percentages,
//! maxHeight, stacking, z-order) — the byte-exact write-stream deliverable.
//! `renderer_focus.json` covers the overlay focus-restore state machine
//! (showOverlay/hideOverlay/setHidden/focus/unfocus, non-capturing, visibility
//! redirection, blocked/eligible transitions, input routing).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use serde::Deserialize;

use atilla_tui::renderer::{
    Component, MarginSpec, OverlayAnchor, OverlayMargin, OverlayOptions, ReactionAction, SizeValue,
};
use atilla_tui::{LoggingTerminal, Tui};

/// A component that renders a fixed set of lines (base content or an overlay).
struct VecLines {
    lines: Vec<String>,
}
impl Component for VecLines {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SizeJson {
    Num(f64),
    Str(String),
}

impl SizeJson {
    fn to_size(&self) -> SizeValue {
        match self {
            SizeJson::Num(n) => SizeValue::Abs(*n as i64),
            SizeJson::Str(s) => {
                let pct = s.trim_end_matches('%').parse::<f64>().expect("percent");
                SizeValue::Pct(pct)
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MarginJson {
    Num(i64),
    Obj(MarginObj),
}

#[derive(Deserialize)]
struct MarginObj {
    top: Option<i64>,
    right: Option<i64>,
    bottom: Option<i64>,
    left: Option<i64>,
}

#[derive(Deserialize)]
struct OptJson {
    width: Option<SizeJson>,
    #[serde(rename = "minWidth")]
    min_width: Option<i64>,
    #[serde(rename = "maxHeight")]
    max_height: Option<SizeJson>,
    anchor: Option<String>,
    #[serde(rename = "offsetX")]
    offset_x: Option<i64>,
    #[serde(rename = "offsetY")]
    offset_y: Option<i64>,
    row: Option<SizeJson>,
    col: Option<SizeJson>,
    margin: Option<MarginJson>,
    #[serde(rename = "nonCapturing")]
    non_capturing: Option<bool>,
    #[serde(rename = "visibleFlag")]
    visible_flag: Option<usize>,
}

fn anchor_of(s: &str) -> OverlayAnchor {
    match s {
        "center" => OverlayAnchor::Center,
        "top-left" => OverlayAnchor::TopLeft,
        "top-right" => OverlayAnchor::TopRight,
        "bottom-left" => OverlayAnchor::BottomLeft,
        "bottom-right" => OverlayAnchor::BottomRight,
        "top-center" => OverlayAnchor::TopCenter,
        "bottom-center" => OverlayAnchor::BottomCenter,
        "left-center" => OverlayAnchor::LeftCenter,
        "right-center" => OverlayAnchor::RightCenter,
        other => panic!("unknown anchor {other}"),
    }
}

fn build_options(opt: &OptJson, flags: &[Rc<Cell<bool>>]) -> OverlayOptions {
    OverlayOptions {
        width: opt.width.as_ref().map(SizeJson::to_size),
        min_width: opt.min_width,
        max_height: opt.max_height.as_ref().map(SizeJson::to_size),
        anchor: opt.anchor.as_deref().map(anchor_of),
        offset_x: opt.offset_x,
        offset_y: opt.offset_y,
        row: opt.row.as_ref().map(SizeJson::to_size),
        col: opt.col.as_ref().map(SizeJson::to_size),
        margin: opt.margin.as_ref().map(|m| match m {
            MarginJson::Num(n) => MarginSpec::All(*n),
            MarginJson::Obj(o) => MarginSpec::Sides(OverlayMargin {
                top: o.top,
                right: o.right,
                bottom: o.bottom,
                left: o.left,
            }),
        }),
        visible: opt.visible_flag.map(|f| flags[f].clone()),
        non_capturing: opt.non_capturing.unwrap_or(false),
    }
}

#[derive(Deserialize)]
struct ComponentSpec {
    lines: Vec<String>,
}

#[derive(Deserialize)]
struct ActionJson {
    op: String,
    #[serde(default)]
    target: Option<i64>,
    #[serde(default)]
    component: Option<usize>,
    #[serde(default)]
    handle: Option<usize>,
}

impl ActionJson {
    fn to_action(&self) -> ReactionAction {
        // Overlay handle ids are deterministic: the i-th showOverlay (0-based)
        // returns handle_id i+1, so a reaction's 0-based handle index maps to
        // handle index+1.
        match self.op.as_str() {
            "setFocus" => ReactionAction::SetFocus(self.target.map(|t| t as usize)),
            "clearBase" => ReactionAction::ClearBase,
            "mountBase" => ReactionAction::MountBase(self.component.unwrap()),
            "hideOverlay" => ReactionAction::HideOverlay,
            "closeOverlay" => ReactionAction::CloseOverlay(self.handle.unwrap() + 1),
            "unfocus" => ReactionAction::Unfocus(self.handle.unwrap() + 1),
            "unfocusTarget" => ReactionAction::UnfocusTarget(
                self.handle.unwrap() + 1,
                self.target.map(|t| t as usize),
            ),
            other => panic!("unknown reaction op {other}"),
        }
    }
}

#[derive(Deserialize)]
struct Reaction {
    component: usize,
    data: String,
    actions: Vec<ActionJson>,
}

#[derive(Deserialize)]
struct Step {
    op: String,
    #[serde(default)]
    target: Option<i64>,
    #[serde(default)]
    component: Option<usize>,
    #[serde(default)]
    options: Option<OptJson>,
    #[serde(default)]
    handle: Option<usize>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(rename = "hasOptions", default)]
    has_options: Option<bool>,
    #[serde(default)]
    flag: Option<usize>,
    #[serde(default)]
    value: Option<bool>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
struct StateSnapshot {
    #[serde(rename = "fullRedraws")]
    full_redraws: u64,
    #[serde(rename = "cursorRow")]
    cursor_row: i64,
    #[serde(rename = "hardwareCursorRow")]
    hardware_cursor_row: i64,
    #[serde(rename = "previousViewportTop")]
    previous_viewport_top: i64,
    #[serde(rename = "maxLinesRendered")]
    max_lines_rendered: i64,
}

#[derive(Deserialize)]
struct FocusSnapshot {
    focused: Option<i64>,
    status: String,
    overlay: Option<i64>,
    #[serde(rename = "blockedBy")]
    blocked_by: Option<i64>,
    resume: String,
    target: Option<i64>,
}

#[derive(Deserialize)]
struct StepResult {
    op: String,
    writes: String,
    state: StateSnapshot,
    focus: FocusSnapshot,
    deliveries: Vec<(usize, String)>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    columns: usize,
    rows: usize,
    components: Vec<ComponentSpec>,
    base: Vec<usize>,
    #[serde(default)]
    flags: Vec<bool>,
    #[serde(default)]
    reactions: Vec<Reaction>,
    steps: Vec<Step>,
    results: Vec<StepResult>,
}

fn load(file: &str) -> Vec<Scenario> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(file);
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

fn opt_usize(v: Option<i64>) -> Option<usize> {
    v.map(|x| x as usize)
}

fn replay(scenario: &Scenario, fails: &mut Vec<String>) {
    let terminal = LoggingTerminal::new(scenario.columns, scenario.rows);
    let mut tui = Tui::new(terminal, false);

    let flags: Vec<Rc<Cell<bool>>> = scenario
        .flags
        .iter()
        .map(|v| Rc::new(Cell::new(*v)))
        .collect();

    // Register every component (ids align with the vector indices).
    for c in &scenario.components {
        let comp: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(VecLines {
            lines: c.lines.clone(),
        }));
        tui.register_component(comp);
    }
    // Base render tree + mounted set.
    for &b in &scenario.base {
        tui.add_child(Box::new(VecLines {
            lines: scenario.components[b].lines.clone(),
        }));
        tui.mount_base(b);
    }
    // Scripted input reactions.
    for r in &scenario.reactions {
        let actions: Vec<ReactionAction> = r.actions.iter().map(ActionJson::to_action).collect();
        tui.set_input_reaction(r.component, &r.data, actions);
    }

    let mut handles: Vec<usize> = Vec::new();

    for (i, step) in scenario.steps.iter().enumerate() {
        tui.terminal_mut().clear_writes();
        match step.op.as_str() {
            "setFocus" => tui.set_focus(opt_usize(step.target)),
            "showOverlay" => {
                let opts = step
                    .options
                    .as_ref()
                    .map(|o| build_options(o, &flags))
                    .unwrap_or_default();
                let h = tui.show_overlay(step.component.unwrap(), opts);
                handles.push(h);
            }
            "hideOverlay" => tui.hide_overlay(),
            "overlayHide" => tui.overlay_hide(handles[step.handle.unwrap()]),
            "overlaySetHidden" => {
                tui.overlay_set_hidden(handles[step.handle.unwrap()], step.hidden.unwrap())
            }
            "overlayFocus" => tui.overlay_focus(handles[step.handle.unwrap()]),
            "overlayUnfocus" => {
                let options = if step.has_options.unwrap_or(false) {
                    Some(opt_usize(step.target))
                } else {
                    None
                };
                tui.overlay_unfocus(handles[step.handle.unwrap()], options);
            }
            "setFlag" => flags[step.flag.unwrap()].set(step.value.unwrap()),
            "mountBase" => tui.mount_base(step.component.unwrap()),
            "sendInput" => tui.handle_input(step.data.as_deref().unwrap()),
            "start" => {
                tui.start();
                tui.flush().expect("overlay vectors never crash");
            }
            "render" => {
                tui.request_render(step.force);
                tui.flush().expect("overlay vectors never crash");
            }
            "stop" => tui.stop(),
            other => panic!("unknown op {other}"),
        }

        let expected = &scenario.results[i];
        let where_ = format!("{}[step {i} {}]", scenario.name, step.op);
        if expected.op != step.op {
            fails.push(format!("{where_}: op mismatch vs vector ({})", expected.op));
        }
        let got = tui.terminal_mut().get_writes();
        if got != expected.writes {
            fails.push(format!(
                "{where_}: WRITE STREAM MISMATCH\n  got : {:?}\n  want: {:?}",
                got, expected.writes
            ));
        }
        // Renderer state.
        if tui.full_redraws() != expected.state.full_redraws {
            fails.push(format!(
                "{where_}: fullRedraws {} want {}",
                tui.full_redraws(),
                expected.state.full_redraws
            ));
        }
        if tui.cursor_row() != expected.state.cursor_row {
            fails.push(format!(
                "{where_}: cursorRow {} want {}",
                tui.cursor_row(),
                expected.state.cursor_row
            ));
        }
        if tui.hardware_cursor_row() != expected.state.hardware_cursor_row {
            fails.push(format!(
                "{where_}: hardwareCursorRow {} want {}",
                tui.hardware_cursor_row(),
                expected.state.hardware_cursor_row
            ));
        }
        if tui.previous_viewport_top() != expected.state.previous_viewport_top {
            fails.push(format!(
                "{where_}: previousViewportTop {} want {}",
                tui.previous_viewport_top(),
                expected.state.previous_viewport_top
            ));
        }
        if tui.max_lines_rendered() != expected.state.max_lines_rendered {
            fails.push(format!(
                "{where_}: maxLinesRendered {} want {}",
                tui.max_lines_rendered(),
                expected.state.max_lines_rendered
            ));
        }
        // Focus snapshot.
        let (focused, status, overlay, blocked_by, resume, target) = tui.focus_snapshot();
        let want = &expected.focus;
        let cmp = [
            (focused, opt_usize(want.focused), "focused"),
            (overlay, opt_usize(want.overlay), "restore.overlay"),
            (blocked_by, opt_usize(want.blocked_by), "restore.blockedBy"),
            (target, opt_usize(want.target), "restore.target"),
        ];
        for (got, exp, label) in cmp {
            if got != exp {
                fails.push(format!("{where_}: {label} {got:?} want {exp:?}"));
            }
        }
        if status != want.status {
            fails.push(format!(
                "{where_}: restore.status {status:?} want {:?}",
                want.status
            ));
        }
        if resume != want.resume {
            fails.push(format!(
                "{where_}: restore.resume {resume:?} want {:?}",
                want.resume
            ));
        }
        // Input deliveries (cumulative).
        let got_deliveries: Vec<(usize, String)> = tui
            .input_deliveries()
            .iter()
            .map(|(c, d)| (*c, d.clone()))
            .collect();
        if got_deliveries != expected.deliveries {
            fails.push(format!(
                "{where_}: deliveries {:?} want {:?}",
                got_deliveries, expected.deliveries
            ));
        }
    }
}

fn run_suite(label: &str, file: &str) {
    let scenarios = load(file);
    assert!(!scenarios.is_empty(), "no {label} vectors loaded");
    let mut fails = Vec::new();
    let step_count: usize = scenarios.iter().map(|s| s.results.len()).sum();
    for scenario in &scenarios {
        replay(scenario, &mut fails);
    }
    if !fails.is_empty() {
        let shown: Vec<_> = fails.iter().take(30).cloned().collect();
        panic!(
            "{label}: {} disagreements across {} scenarios / {step_count} steps\n{}",
            fails.len(),
            scenarios.len(),
            shown.join("\n")
        );
    }
    eprintln!(
        "{label}: {} scenarios / {step_count} steps byte-identical to pi",
        scenarios.len()
    );
}

#[test]
fn renderer_overlay_compositing_vectors() {
    run_suite("renderer_overlays", "renderer_overlays.json");
}

#[test]
fn renderer_focus_restore_vectors() {
    run_suite("renderer_focus", "renderer_focus.json");
}
