//! Replays renderer vectors extracted from pi's own `TUI`
//! (`crates/pidgin-tui/vectors/gen/generate_renderer.mjs`, driven through the
//! `@xterm/headless` harness) and asserts the Rust renderer emits a
//! byte-identical write stream plus identical viewport bookkeeping at every
//! step. pi is the source of truth; any disagreement is a bug in the port.
//!
//! Scope: PR-R1 core (resize/shrink/differential cases from `tui-render.test.ts`
//! and `tui-shrink.test.ts`). Kitty image lifecycle and overlay cases are
//! deferred to PR-R2 and are intentionally absent from these vectors.

use std::path::PathBuf;

use serde::Deserialize;

use pidgin_tui::{LoggingTerminal, SharedLines, Tui};

#[derive(Deserialize)]
struct SetEntry {
    #[serde(default)]
    c: usize,
    lines: Vec<String>,
}

#[derive(Deserialize)]
struct Step {
    op: String,
    #[serde(default)]
    set: Option<Vec<SetEntry>>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    columns: Option<usize>,
    #[serde(default)]
    rows: Option<usize>,
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
struct StepResult {
    op: String,
    writes: String,
    state: StateSnapshot,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    columns: usize,
    rows: usize,
    #[serde(rename = "showHardwareCursor")]
    show_hardware_cursor: bool,
    #[serde(rename = "clearOnShrink")]
    clear_on_shrink: bool,
    termux: bool,
    #[serde(rename = "imagesCapable")]
    images_capable: bool,
    components: usize,
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

fn replay(scenario: &Scenario, fails: &mut Vec<String>) {
    let terminal = LoggingTerminal::new(scenario.columns, scenario.rows);
    let mut tui = Tui::new(terminal, scenario.show_hardware_cursor);
    tui.set_clear_on_shrink(scenario.clear_on_shrink);
    tui.set_termux(scenario.termux);
    tui.set_images_capable(scenario.images_capable);

    // One SharedLines child per component; keep handles to mutate between steps.
    let handles: Vec<SharedLines> = (0..scenario.components)
        .map(|_| SharedLines::new())
        .collect();
    for h in &handles {
        tui.add_child(Box::new(h.clone()));
    }

    for (i, step) in scenario.steps.iter().enumerate() {
        if let Some(set) = &step.set {
            for entry in set {
                handles[entry.c].set(entry.lines.clone());
            }
        }
        tui.terminal_mut().clear_writes();
        match step.op.as_str() {
            "start" => {
                tui.start();
                tui.flush()
                    .expect("R1 vectors never trigger the crash contract");
            }
            "render" => {
                tui.request_render(step.force);
                tui.flush()
                    .expect("R1 vectors never trigger the crash contract");
            }
            "clear" => {
                tui.clear();
                tui.request_render(step.force);
                tui.flush()
                    .expect("R1 vectors never trigger the crash contract");
            }
            "resize" => {
                tui.terminal_mut()
                    .resize(step.columns.unwrap(), step.rows.unwrap());
                tui.request_render(false);
                tui.flush()
                    .expect("R1 vectors never trigger the crash contract");
            }
            "stop" => {
                tui.stop();
            }
            other => panic!("unknown op {other}"),
        }

        let expected = &scenario.results[i];
        let got_writes = tui.terminal_mut().get_writes();
        let where_ = format!("{}[step {i} {}]", scenario.name, step.op);
        if expected.op != step.op {
            fails.push(format!("{where_}: op mismatch vs vector ({})", expected.op));
        }
        if got_writes != expected.writes {
            fails.push(format!(
                "{where_}: WRITE STREAM MISMATCH\n  got : {:?}\n  want: {:?}",
                got_writes, expected.writes
            ));
        }
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
        let shown: Vec<_> = fails.iter().take(20).cloned().collect();
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
fn renderer_core_vectors() {
    run_suite("renderer_core", "renderer_core.json");
}

/// The 7 byte-exact "TUI Kitty image cleanup" cases from `tui-render.test.ts`:
/// reserved-row clear-before-draw, would-scroll full-redraw fallback,
/// reserved-row draw during full redraw, taller-than-viewport first-row
/// placement (#4461), delete-before-draw ordering for moved/changed images,
/// and previous-image deletion during full redraws.
#[test]
fn renderer_image_vectors() {
    run_suite("renderer_images", "renderer_images.json");
}
