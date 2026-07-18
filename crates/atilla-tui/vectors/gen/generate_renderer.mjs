// straitjacket-allow-file:duplication — generator scaffolding; the per-scenario
// step builders are intentionally uniform data, not shared logic.
// Vector generator for the byte-exact Rust port of pi's TUI line-diff renderer
// (`vendor/pi/packages/tui/src/tui.ts`, class TUI).
//
// It imports pi's own `TUI` plus the `@xterm/headless`-backed test harness
// (`test/virtual-terminal.ts`) and drives each core (non-image, non-overlay)
// scenario from `test/tui-render.test.ts` + `test/tui-shrink.test.ts`, dumping
// per-step {raw ANSI write stream, viewport, cursor/viewport bookkeeping,
// fullRedraws} as JSON. The Rust renderer test (tests/renderer_vectors.rs)
// replays the same script and asserts the emitted write stream + state are
// byte-identical.
//
// Only `terminal.write()` calls are captured (this mirrors pi's LoggingVirtual-
// Terminal.getWrites(): hideCursor/showCursor/bracketed-paste go straight to
// xterm and are intentionally not part of the diff stream).
//
// Determinism: no images are used in R1, so `Math.random()` in allocateImageId
// is never reached. Kitty image lifecycle + overlays are deferred to PR-R2.
//
// Run from this directory:  node generate_renderer.mjs
// Output is written to ../../tests/vectors/renderer_core.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { TUI } from "../../../../vendor/pi/packages/tui/src/tui.ts";
import { VirtualTerminal } from "../../../../vendor/pi/packages/tui/test/virtual-terminal.ts";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "..", "tests", "vectors");
mkdirSync(outDir, { recursive: true });

// Logging terminal: mirrors the test-suite LoggingVirtualTerminal. Only
// terminal.write() is recorded (hideCursor/showCursor/start/stop bypass this
// path via xterm.write and are excluded from the diff stream, exactly as in
// pi's own getWrites()).
class LoggingVirtualTerminal extends VirtualTerminal {
	constructor(columns, rows) {
		super(columns, rows);
		this.writes = [];
	}
	write(data) {
		this.writes.push(data);
		super.write(data);
	}
	getWrites() {
		return this.writes.join("");
	}
	clearWrites() {
		this.writes = [];
	}
}

class LinesComponent {
	constructor(lines = []) {
		this.lines = lines;
	}
	render() {
		return this.lines;
	}
	invalidate() {}
}

function snapshot(tui) {
	// TypeScript `private` is compile-time only; the fields are plain runtime
	// properties, so this reads pi's internal renderer bookkeeping directly.
	return {
		fullRedraws: tui.fullRedraws,
		cursorRow: tui.cursorRow,
		hardwareCursorRow: tui.hardwareCursorRow,
		previousViewportTop: tui.previousViewportTop,
		maxLinesRendered: tui.maxLinesRendered,
		previousWidth: tui.previousWidth,
		previousHeight: tui.previousHeight,
		previousLines: tui.previousLines.slice(),
	};
}

// Each scenario declares terminal size, optional flags, the component set, and
// a list of steps. A step mutates component lines / resizes / stops, drives one
// coalesced render via waitForRender(), and we capture the writes + state it
// produced.
async function runScenario(scenario) {
	const terminal = new LoggingVirtualTerminal(scenario.columns, scenario.rows);
	// showHardwareCursor defaults false (PI_HARDWARE_CURSOR !== "1"); pass the
	// second constructor arg so the port does not depend on process env.
	const tui = new TUI(terminal, scenario.showHardwareCursor ?? false);
	if (scenario.clearOnShrink !== undefined) tui.setClearOnShrink(scenario.clearOnShrink);
	const components = (scenario.components ?? [{}]).map((c) => new LinesComponent(c.lines ?? []));
	for (const c of components) tui.addChild(c);

	const outSteps = [];
	for (const step of scenario.steps) {
		if (step.set) {
			for (const s of step.set) components[s.c ?? 0].lines = s.lines;
		}
		terminal.clearWrites();
		switch (step.op) {
			case "start":
				tui.start();
				await terminal.waitForRender();
				break;
			case "render":
				tui.requestRender(step.force ?? false);
				await terminal.waitForRender();
				break;
			case "clear":
				tui.clear();
				tui.requestRender(step.force ?? false);
				await terminal.waitForRender();
				break;
			case "resize":
				terminal.resize(step.columns, step.rows);
				await terminal.waitForRender();
				break;
			case "stop":
				tui.stop();
				break;
			default:
				throw new Error(`unknown op ${step.op}`);
		}
		outSteps.push({
			op: step.op,
			writes: terminal.getWrites(),
			viewport: step.op === "stop" ? [] : terminal.getViewport(),
			state: snapshot(tui),
		});
	}

	return {
		name: scenario.name,
		columns: scenario.columns,
		rows: scenario.rows,
		showHardwareCursor: scenario.showHardwareCursor ?? false,
		clearOnShrink: scenario.clearOnShrink ?? false,
		termux: scenario.termux ?? false,
		imagesCapable: false,
		components: components.length,
		steps: scenario.steps.map((s) => ({
			op: s.op,
			set: s.set ?? null,
			force: s.force ?? false,
			columns: s.columns ?? null,
			rows: s.rows ?? null,
		})),
		results: outSteps,
	};
}

const seq = (n, label = "Line") => Array.from({ length: n }, (_, i) => `${label} ${i}`);

// --- Scenarios: the core (non-image, non-overlay) cases from tui-render.test.ts
// and tui-shrink.test.ts. termux flag maps to isTermuxSession(); the generator
// sets TERMUX_VERSION around the run since pi reads it live in doRender.
const scenarios = [
	{
		name: "resize_height_full_redraw",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["Line 0", "Line 1", "Line 2"] }] },
			{ op: "resize", columns: 40, rows: 15 },
			{ op: "stop" },
		],
	},
	{
		name: "resize_height_termux_no_redraw",
		columns: 40,
		rows: 10,
		termux: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(20) }] },
			{ op: "resize", columns: 40, rows: 15 },
			{ op: "resize", columns: 40, rows: 8 },
			{ op: "resize", columns: 40, rows: 14 },
			{ op: "resize", columns: 40, rows: 11 },
			{ op: "stop" },
		],
	},
	{
		name: "resize_width_full_redraw",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["Line 0", "Line 1", "Line 2"] }] },
			{ op: "resize", columns: 60, rows: 10 },
			{ op: "stop" },
		],
	},
	{
		name: "shrink_significant_clearonshrink",
		columns: 40,
		rows: 10,
		clearOnShrink: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(6) }] },
			{ op: "render", set: [{ c: 0, lines: ["Line 0", "Line 1"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "shrink_to_single_line",
		columns: 40,
		rows: 10,
		clearOnShrink: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(4) }] },
			{ op: "render", set: [{ c: 0, lines: ["Only line"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "shrink_to_empty",
		columns: 40,
		rows: 10,
		clearOnShrink: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(3) }] },
			{ op: "render", set: [{ c: 0, lines: [] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_shrink_unchanged_then_change",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(5) }] },
			{ op: "render", set: [{ c: 0, lines: ["Line 0", "Line 1", "Line 2"] }] },
			{ op: "render", set: [{ c: 0, lines: ["Line 0", "CHANGED", "Line 2"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_spinner_middle_line",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["Header", "Working...", "Footer"] }] },
			{ op: "render", set: [{ c: 0, lines: ["Header", "Working |", "Footer"] }] },
			{ op: "render", set: [{ c: 0, lines: ["Header", "Working /", "Footer"] }] },
			{ op: "render", set: [{ c: 0, lines: ["Header", "Working -", "Footer"] }] },
			{ op: "render", set: [{ c: 0, lines: ["Header", "Working \\", "Footer"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_style_reset_per_line",
		columns: 20,
		rows: 6,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["\x1b[3mItalic", "Plain"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_first_line_changes",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(4) }] },
			{ op: "render", set: [{ c: 0, lines: ["CHANGED", "Line 1", "Line 2", "Line 3"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_last_line_changes",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(4) }] },
			{ op: "render", set: [{ c: 0, lines: ["Line 0", "Line 1", "Line 2", "CHANGED"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_multiple_nonadjacent",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(5) }] },
			{ op: "render", set: [{ c: 0, lines: ["Line 0", "CHANGED 1", "Line 2", "CHANGED 3", "Line 4"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_content_empty_content",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["Line 0", "Line 1", "Line 2"] }] },
			{ op: "render", set: [{ c: 0, lines: [] }] },
			{ op: "render", set: [{ c: 0, lines: ["New Line 0", "New Line 1"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_deleted_lines_move_viewport_up",
		columns: 20,
		rows: 5,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(12) }] },
			{ op: "render", set: [{ c: 0, lines: seq(7) }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_append_after_shrink",
		columns: 20,
		rows: 5,
		steps: [
			{ op: "start", set: [{ c: 0, lines: seq(8) }] },
			{ op: "render", set: [{ c: 0, lines: ["Line 0", "Line 1"] }] },
			{ op: "render", set: [{ c: 0, lines: ["Line 0", "Line 1", "Line 2"] }] },
			{ op: "stop" },
		],
	},
	{
		name: "diff_maxlines_inflated_transient",
		columns: 40,
		rows: 10,
		components: [{ lines: [] }, { lines: [] }],
		steps: [
			{
				op: "start",
				set: [
					{ c: 0, lines: seq(15, "Chat") },
					{ c: 1, lines: ["Editor 0", "Editor 1", "Editor 2"] },
				],
			},
			{ op: "render", set: [{ c: 1, lines: seq(8, "Selector") }] },
			{ op: "render", set: [{ c: 1, lines: ["Editor 0", "Editor 1", "Editor 2"] }] },
			{ op: "render", set: [{ c: 0, lines: seq(12, "Chat") }] },
			{ op: "stop" },
		],
	},
	{
		name: "shrink_clear_to_zero",
		columns: 40,
		rows: 10,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["first", "second", "third"] }] },
			{ op: "clear" },
			{ op: "stop" },
		],
	},
];

const out = [];
let stepCount = 0;
for (const scenario of scenarios) {
	const prevTermux = process.env.TERMUX_VERSION;
	if (scenario.termux) process.env.TERMUX_VERSION = "1";
	else delete process.env.TERMUX_VERSION;
	try {
		const result = await runScenario(scenario);
		out.push(result);
		stepCount += result.results.length;
	} finally {
		if (prevTermux === undefined) delete process.env.TERMUX_VERSION;
		else process.env.TERMUX_VERSION = prevTermux;
	}
}

const outPath = join(outDir, "renderer_core.json");
writeFileSync(outPath, `${JSON.stringify(out, null, "\t")}\n`);
console.log(`  renderer_core.json: ${out.length} scenarios, ${stepCount} steps`);
console.log(`\nTOTAL SCENARIOS: ${out.length}`);
