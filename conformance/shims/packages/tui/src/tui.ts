// Native shim for packages/tui/src/tui.ts, backed by the pidgin Rust addon
// (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `tui.__pi_original__.ts` and this shim takes its place,
// so pi's tests import `../src/tui.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's differential RENDER PATH (`TUI::doRender`),
// ported bit-exactly in `crates/pidgin-tui` (the renderer, validated against 342
// byte-exact vectors) and exposed as `TuiCore`. The Rust renderer consumes
// PRE-RENDERED LINES: pi's TS components still render themselves to `string[]`
// (unchanged), and this shim feeds those lines into Rust via `setBaseLines`,
// drives one render with `tick`, drains the write stream with `takeWrites`, and
// forwards it to pi's `VirtualTerminal` (an xterm emulator) so
// `getViewport`/cell-attribute assertions replay JS-side.
//
// Everything else stays pi's TS, unchanged: this shim SUBCLASSES pi's original
// `TUI` and overrides ONLY the render seam (`doRender`, plus `requestRender` to
// capture the force flag). Overlays, focus, input routing, OSC handling, and
// `compositeLineAt` are inherited verbatim from the original, so the ~9 other
// tui test files that construct a `TUI` (overlay-*, editor, tab-width, ...) keep
// pi's exact behavior. When an overlay is active, the render is delegated to the
// original `doRender` (pi composites overlays in TS); the base render path — the
// only path the `tui-render` / `tui-shrink` suites exercise — routes through
// Rust. `Container`, `Component`, `CURSOR_MARKER`, overlay types, `isFocusable`,
// and all other exports are re-exported from the original unchanged.

export * from "./tui.__pi_original__.ts";

import { TuiCore } from "pidgin-napi";
import { TUI as OriginalTUI } from "./tui.__pi_original__.ts";
import type { Terminal } from "./terminal.ts";

// The subset of pi's `TUI` private state this shim reads/writes to keep the
// original engine coherent and its public getters (`fullRedraws`) correct. These
// fields exist on the instance at runtime (set by pi's constructor); the cast
// only satisfies the type layer, which Node's type-stripping erases anyway.
interface TuiInternals {
	stopped: boolean;
	overlayStack: unknown[];
	fullRedrawCount: number;
	cursorRow: number;
	hardwareCursorRow: number;
	previousViewportTop: number;
	maxLinesRendered: number;
	previousLines: string[];
	render(width: number): string[];
	getClearOnShrink(): boolean;
	getShowHardwareCursor(): boolean;
	terminal: Terminal;
}

/**
 * Native `TUI`: pi's original renderer with the base render path routed through
 * the Rust `TuiCore`. See the file header for the seam. Only `doRender` and
 * `requestRender` are overridden; all other behavior is inherited.
 */
export class TUI extends OriginalTUI {
	private core?: TuiCore;
	private forceRender = false;

	private internals(): TuiInternals {
		return this as unknown as TuiInternals;
	}

	/** Lazily build the Rust renderer at first render, matching the terminal's
	 * current dimensions and the `PI_HARDWARE_CURSOR` opt-in. */
	private getCore(cols: number, rows: number): TuiCore {
		if (this.core === undefined) {
			this.core = new TuiCore(cols, rows, this.internals().getShowHardwareCursor());
		}
		return this.core;
	}

	// Capture the force flag (pi coalesces requests; any force before the next
	// render forces it), then defer to pi's scheduler + field resets unchanged.
	override requestRender(force = false): void {
		if (force) {
			this.forceRender = true;
		}
		super.requestRender(force);
	}

	// The render seam. When an overlay is active, pi composites overlays in TS —
	// delegate to the original so overlay/focus tests keep exact behavior. The
	// base render path routes through Rust: render pi's components to lines TS-side,
	// feed them to `TuiCore`, drive one render, and forward the write stream to the
	// JS terminal (an xterm emulator) so viewport/cell assertions replay.
	private doRender(): void {
		const self = this.internals();
		if (self.stopped) {
			this.forceRender = false;
			return;
		}
		if (self.overlayStack.length > 0) {
			// Overlay path stays in pi's TS renderer.
			(OriginalTUI.prototype as unknown as { doRender(): void }).doRender.call(this);
			this.forceRender = false;
			return;
		}

		const terminal = self.terminal;
		const cols = terminal.columns;
		const rows = terminal.rows;
		const width = cols;
		const lines = self.render(width);

		const core = this.getCore(cols, rows);
		core.setSize(cols, rows);
		core.setTermux(Boolean(process.env.TERMUX_VERSION));
		core.setClearOnShrink(self.getClearOnShrink());
		core.setBaseLines(lines);
		core.tick(this.forceRender);
		this.forceRender = false;

		const writes = core.takeWrites();
		if (writes.length > 0) {
			terminal.write(writes);
		}

		// Mirror the renderer's state back onto pi's fields so the inherited
		// `fullRedraws` getter and `stop()` (which reads previousLines/hardware
		// cursor) stay correct, and a later overlay render can take over coherently.
		self.fullRedrawCount = core.fullRedraws();
		self.cursorRow = core.cursorRow();
		self.hardwareCursorRow = core.hardwareCursorRow();
		self.previousViewportTop = core.previousViewportTop();
		self.maxLinesRendered = core.maxLinesRendered();
		self.previousLines = lines;
	}
}
