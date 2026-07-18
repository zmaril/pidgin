// straitjacket-allow-file:duplication — generator scaffolding; the per-scenario
// step builders are intentionally uniform data, not shared logic.
// PR-R2 vector generator for the byte-exact Rust port of pi's TUI renderer
// (`vendor/pi/packages/tui/src/tui.ts`, class TUI): the Kitty image lifecycle,
// overlay compositing, and the overlay focus-restore state machine.
//
// It imports pi's own `TUI` plus the `@xterm/headless`-backed test harness
// (`test/virtual-terminal.ts`) and drives each scenario from
// `test/tui-render.test.ts` ("TUI Kitty image cleanup"),
// `test/overlay-options.test.ts`, `test/tui-overlay-style-leak.test.ts`, and
// `test/overlay-non-capturing.test.ts`, dumping per-step {raw ANSI write
// stream, viewport, cursor/viewport bookkeeping, fullRedraws, focus state} as
// JSON. The Rust renderer test replays the same script and asserts the emitted
// write stream + state are byte-identical.
//
// Determinism: image scenarios pass a FIXED imageId to Image/encodeKitty so the
// `Math.random()` in allocateImageId is never reached and re-runs are stable.
//
// Run from this directory:  node generate_renderer_r2.mjs
// Outputs are written to ../../tests/vectors/renderer_images.json and
// ../../tests/vectors/renderer_overlays.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Image } from "../../../../vendor/pi/packages/tui/src/components/image.ts";
import {
	encodeKitty,
	resetCapabilitiesCache,
	setCapabilities,
	setCellDimensions,
} from "../../../../vendor/pi/packages/tui/src/terminal-image.ts";
import { Container, TUI } from "../../../../vendor/pi/packages/tui/src/tui.ts";
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
	};
}

// ---------------------------------------------------------------------------
// Kitty image scenarios (byte-exact write stream). Reuses the renderer_core
// schema so tests/renderer_vectors.rs replays them with the same driver.
// ---------------------------------------------------------------------------

// Build the exact image lines pi's Image component would render, with a FIXED
// imageId so the vector is deterministic (kills Math.random in allocateImageId).
function imageLines({ base64, mime, maxWidthCells, dims, imageId }) {
	const image = new Image(base64, mime, { fallbackColor: (v) => v }, { maxWidthCells, imageId }, dims);
	return image.render(40);
}

// Each image scenario runs with kitty capabilities + fixed cell dimensions set
// around the run (mirroring the test's setCapabilities/setCellDimensions).
function buildImageScenarios() {
	const scenarios = [];

	// 1. clears reserved Kitty image rows before drawing appended placements
	scenarios.push({
		name: "kitty_clear_reserved_rows_before_draw",
		columns: 40,
		rows: 10,
		caps: "kitty",
		cellPx: { widthPx: 10, heightPx: 10 },
		imagesCapable: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["before"] }] },
			{
				op: "render",
				set: [
					{
						c: 0,
						lines: ["before", ...imageLines({ base64: "AAAA", mime: "image/png", maxWidthCells: 2, dims: { widthPx: 20, heightPx: 20 }, imageId: 4242 }), "after"],
					},
				],
			},
			{ op: "stop" },
		],
	});

	// 2. falls back to full redraw when Kitty image pre-clear would scroll
	scenarios.push({
		name: "kitty_pre_clear_would_scroll_full_redraw",
		columns: 40,
		rows: 2,
		caps: "kitty",
		cellPx: { widthPx: 10, heightPx: 10 },
		imagesCapable: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["before"] }] },
			{
				op: "render",
				set: [
					{
						c: 0,
						lines: ["before", ...imageLines({ base64: "AAAA", mime: "image/png", maxWidthCells: 3, dims: { widthPx: 30, heightPx: 30 }, imageId: 4243 }), "after"],
					},
				],
			},
			{ op: "stop" },
		],
	});

	// 3. reserves Kitty image rows before drawing during full redraw fallbacks
	scenarios.push({
		name: "kitty_reserve_rows_during_full_redraw",
		columns: 40,
		rows: 5,
		caps: "kitty",
		cellPx: { widthPx: 10, heightPx: 10 },
		imagesCapable: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["l0", "l1", "l2", "l3", "l4"] }] },
			{
				op: "render",
				set: [
					{
						c: 0,
						lines: ["l0", "l1", "l2", "l3", "l4", ...imageLines({ base64: "AAAA", mime: "image/png", maxWidthCells: 3, dims: { widthPx: 30, heightPx: 30 }, imageId: 4244 }), "after"],
					},
				],
			},
			{ op: "stop" },
		],
	});

	// 4. does not use cursor-up placement for images taller than the viewport
	scenarios.push({
		name: "kitty_taller_than_viewport_first_row_placement",
		columns: 40,
		rows: 5,
		caps: "kitty",
		cellPx: { widthPx: 10, heightPx: 10 },
		imagesCapable: true,
		steps: [
			{ op: "start", set: [{ c: 0, lines: ["before"] }] },
			{
				op: "render",
				force: true,
				set: [
					{
						c: 0,
						lines: ["before", ...imageLines({ base64: "AAAA", mime: "image/png", maxWidthCells: 6, dims: { widthPx: 60, heightPx: 60 }, imageId: 4245 }), "after"],
					},
				],
			},
			{ op: "stop" },
		],
	});

	// 5. deletes changed image ids before drawing moved placements
	{
		const oldImage = encodeKitty("AAAA", { columns: 2, rows: 2, imageId: 42, moveCursor: false });
		const newImage = encodeKitty("BBBB", { columns: 2, rows: 1, imageId: 42, moveCursor: false });
		scenarios.push({
			name: "kitty_delete_changed_before_moved_draw",
			columns: 40,
			rows: 10,
			caps: null,
			imagesCapable: false,
			steps: [
				{ op: "start", set: [{ c: 0, lines: ["top", oldImage] }] },
				{ op: "render", set: [{ c: 0, lines: [newImage, ""] }] },
				{ op: "stop" },
			],
		});
	}

	// 6. redraws image lines when an earlier reserved image row changes
	{
		const image = encodeKitty("AAAA", { columns: 2, rows: 2, imageId: 88, moveCursor: false });
		scenarios.push({
			name: "kitty_redraw_when_reserved_row_changes",
			columns: 40,
			rows: 10,
			caps: null,
			imagesCapable: false,
			steps: [
				{ op: "start", set: [{ c: 0, lines: ["", image] }] },
				{ op: "render", set: [{ c: 0, lines: ["covered", image] }] },
				{ op: "stop" },
			],
		});
	}

	// 7. deletes previously rendered image ids during full redraws
	{
		const image = encodeKitty("AAAA", { columns: 2, rows: 2, imageId: 77, moveCursor: false });
		scenarios.push({
			name: "kitty_delete_previous_ids_during_full_redraw",
			columns: 40,
			rows: 10,
			caps: null,
			imagesCapable: false,
			steps: [
				{ op: "start", set: [{ c: 0, lines: [image] }] },
				{ op: "render", force: true, set: [{ c: 0, lines: ["plain text"] }] },
				{ op: "stop" },
			],
		});
	}

	return scenarios;
}

async function runImageScenario(scenario) {
	const terminal = new LoggingVirtualTerminal(scenario.columns, scenario.rows);
	const tui = new TUI(terminal, false);
	const component = new LinesComponent([]);
	tui.addChild(component);

	const outSteps = [];
	for (const step of scenario.steps) {
		if (step.set) {
			for (const s of step.set) component.lines = s.lines;
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
		showHardwareCursor: false,
		clearOnShrink: false,
		termux: false,
		imagesCapable: scenario.imagesCapable ?? false,
		components: 1,
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

async function generateImages() {
	const out = [];
	let stepCount = 0;
	for (const scenario of buildImageScenarios()) {
		if (scenario.caps === "kitty") {
			setCapabilities({ images: "kitty", trueColor: true, hyperlinks: true });
		} else {
			// Ensure a deterministic non-image capability (the test env has no
			// kitty markers, so detectCapabilities returns images: null).
			setCapabilities({ images: null, trueColor: true, hyperlinks: false });
		}
		if (scenario.cellPx) setCellDimensions(scenario.cellPx);
		try {
			const result = await runImageScenario(scenario);
			out.push(result);
			stepCount += result.results.length;
		} finally {
			resetCapabilitiesCache();
			setCellDimensions({ widthPx: 9, heightPx: 18 });
		}
	}
	const outPath = join(outDir, "renderer_images.json");
	writeFileSync(outPath, `${JSON.stringify(out, null, "\t")}\n`);
	console.log(`  renderer_images.json: ${out.length} scenarios, ${stepCount} steps`);
	return out.length;
}

const imageCount = await generateImages();

// ---------------------------------------------------------------------------
// Overlay compositing + focus-restore scenarios. Components are addressed by
// index; the driver reproduces pi's overlay API driven by a step list, and
// captures the write stream, viewport, renderer state, focus snapshot, and
// input-delivery log. The Rust replay (tests/renderer_overlay_vectors.rs)
// reproduces each step and asserts byte-identity + focus/delivery parity.
// ---------------------------------------------------------------------------

// A focusable, input-recording component. `id` is its index; handleInput
// records the delivery, records to its own inputs, and runs any scripted
// reaction (mirroring the ad-hoc handleInput closures in pi's overlay tests).
class FocusComponent {
	constructor(id, lines, ctx) {
		this.id = id;
		this.lines = lines;
		this.focused = false;
		this.inputs = [];
		this.ctx = ctx;
	}
	render() {
		return this.lines;
	}
	handleInput(data) {
		this.ctx.deliveries.push([this.id, data]);
		this.inputs.push(data);
		const actions = this.ctx.reactions.get(`${this.id} ${data}`);
		if (actions) this.ctx.applyReaction(actions);
	}
	invalidate() {}
}

function focusSnapshot(tui, compIndex) {
	const idOf = (component) => {
		if (component === null || component === undefined) return null;
		return compIndex.has(component) ? compIndex.get(component) : null;
	};
	const restore = tui.overlayFocusRestore;
	const snap = { focused: idOf(tui.focusedComponent), status: restore.status };
	if (restore.status === "eligible" || restore.status === "blocked") {
		snap.overlay = idOf(restore.overlay.component);
	} else {
		snap.overlay = null;
	}
	if (restore.status === "blocked") {
		snap.blockedBy = idOf(restore.blockedBy);
		snap.resume = restore.resume.status;
		snap.target = restore.resume.status === "focus-target" ? idOf(restore.resume.target) : null;
	} else {
		snap.blockedBy = null;
		snap.resume = "none";
		snap.target = null;
	}
	return snap;
}

// Build a pi OverlayOptions object from the vector's normalized options,
// wiring `visible` to a shared flag array so the tests' toggle pattern works.
function buildOptions(opt, flags) {
	if (!opt) return undefined;
	const out = {};
	if (opt.width !== undefined && opt.width !== null) out.width = opt.width;
	if (opt.minWidth !== undefined && opt.minWidth !== null) out.minWidth = opt.minWidth;
	if (opt.maxHeight !== undefined && opt.maxHeight !== null) out.maxHeight = opt.maxHeight;
	if (opt.anchor) out.anchor = opt.anchor;
	if (opt.offsetX !== undefined && opt.offsetX !== null) out.offsetX = opt.offsetX;
	if (opt.offsetY !== undefined && opt.offsetY !== null) out.offsetY = opt.offsetY;
	if (opt.row !== undefined && opt.row !== null) out.row = opt.row;
	if (opt.col !== undefined && opt.col !== null) out.col = opt.col;
	if (opt.margin !== undefined && opt.margin !== null) out.margin = opt.margin;
	if (opt.nonCapturing) out.nonCapturing = true;
	if (opt.visibleFlag !== undefined && opt.visibleFlag !== null) {
		const flagId = opt.visibleFlag;
		out.visible = () => flags[flagId];
	}
	return out;
}

async function runOverlayScenario(scenario) {
	const terminal = new LoggingVirtualTerminal(scenario.columns, scenario.rows);
	const tui = new TUI(terminal, false);

	const compIndex = new Map();
	const ctx = { deliveries: [], reactions: new Map(), applyReaction: null };
	const components = (scenario.components ?? []).map((c, i) => {
		const comp = new FocusComponent(i, c.lines ?? [], ctx);
		compIndex.set(comp, i);
		return comp;
	});
	const flags = (scenario.flags ?? []).map((v) => v);
	const handles = [];

	// Base render tree (rendered as the underlying content), a real pi Container
	// so `isComponentMounted`/`containsComponent` behave exactly as in pi. Base
	// children are referenced by component index.
	const baseContainer = new Container();
	for (const idx of scenario.base ?? []) baseContainer.addChild(components[idx]);
	tui.addChild(baseContainer);

	ctx.applyReaction = (actions) => {
		for (const a of actions) {
			switch (a.op) {
				case "setFocus":
					tui.setFocus(a.target === null || a.target === undefined ? null : components[a.target]);
					break;
				case "clearBase":
					baseContainer.clear();
					break;
				case "mountBase":
					baseContainer.addChild(components[a.component]);
					break;
				case "hideOverlay":
					tui.hideOverlay();
					break;
				case "closeOverlay":
					handles[a.handle].hide();
					break;
				case "unfocus":
					handles[a.handle].unfocus();
					break;
				case "unfocusTarget":
					handles[a.handle].unfocus({ target: a.target === null || a.target === undefined ? null : components[a.target] });
					break;
				default:
					throw new Error(`unknown reaction op ${a.op}`);
			}
		}
	};

	for (const r of scenario.reactions ?? []) {
		ctx.reactions.set(`${r.component} ${r.data}`, r.actions);
	}

	const outSteps = [];
	for (const step of scenario.steps) {
		terminal.clearWrites();
		switch (step.op) {
			case "setFocus":
				tui.setFocus(step.target === null || step.target === undefined ? null : components[step.target]);
				break;
			case "showOverlay": {
				const handle = tui.showOverlay(components[step.component], buildOptions(step.options, flags));
				handles.push(handle);
				break;
			}
			case "hideOverlay":
				tui.hideOverlay();
				break;
			case "overlayHide":
				handles[step.handle].hide();
				break;
			case "overlaySetHidden":
				handles[step.handle].setHidden(step.hidden);
				break;
			case "overlayFocus":
				handles[step.handle].focus();
				break;
			case "overlayUnfocus":
				if (step.hasOptions) {
					handles[step.handle].unfocus({ target: step.target === null || step.target === undefined ? null : components[step.target] });
				} else {
					handles[step.handle].unfocus();
				}
				break;
			case "setFlag":
				flags[step.flag] = step.value;
				break;
			case "mountBase":
				baseContainer.addChild(components[step.component]);
				break;
			case "sendInput":
				tui.handleInput(step.data);
				break;
			case "start":
				tui.start();
				await terminal.waitForRender();
				break;
			case "render":
				tui.requestRender(step.force ?? false);
				await terminal.waitForRender();
				break;
			case "stop":
				tui.stop();
				break;
			default:
				throw new Error(`unknown op ${step.op}`);
		}
		const flushed = step.op === "start" || step.op === "render";
		outSteps.push({
			op: step.op,
			writes: terminal.getWrites(),
			viewport: flushed ? terminal.getViewport() : [],
			state: snapshot(tui),
			focus: focusSnapshot(tui, compIndex),
			deliveries: ctx.deliveries.map((d) => [d[0], d[1]]),
		});
	}

	return {
		name: scenario.name,
		columns: scenario.columns,
		rows: scenario.rows,
		components: (scenario.components ?? []).map((c) => ({ lines: c.lines ?? [] })),
		base: scenario.base ?? [],
		flags: scenario.flags ?? [],
		reactions: scenario.reactions ?? [],
		steps: scenario.steps,
		results: outSteps,
	};
}

async function generateOverlays(scenarios, fileName, label) {
	const out = [];
	let stepCount = 0;
	for (const scenario of scenarios) {
		const result = await runOverlayScenario(scenario);
		out.push(result);
		stepCount += result.results.length;
	}
	const outPath = join(outDir, fileName);
	writeFileSync(outPath, `${JSON.stringify(out, null, "\t")}\n`);
	console.log(`  ${fileName}: ${out.length} scenarios, ${stepCount} steps`);
	return out.length;
}

const { compositingScenarios, focusScenarios } = await import("./generate_renderer_r2_scenarios.mjs");
const overlayCount = await generateOverlays(compositingScenarios, "renderer_overlays.json", "overlays");
const focusCount = await generateOverlays(focusScenarios, "renderer_focus.json", "focus");

console.log(`\nTOTAL: ${imageCount} image, ${overlayCount} overlay, ${focusCount} focus scenarios`);
