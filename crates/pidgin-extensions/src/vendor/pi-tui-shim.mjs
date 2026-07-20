// straitjacket-allow-file:duplication -- the marker classes faithfully mirror
// pi's upstream pi-tui components, which repeat the same container surface
// (addChild/removeChild/clear) across Box (components/box.ts) and Container
// (tui.ts). The parallel between the Box and Container markers is intentional and
// faithful to upstream, not incidental duplication.
// Render-stub shim of @earendil-works/pi-tui: component classes are display-time
// markers (renderResult/renderCall are never invoked on pidgin's headless plane);
// pure utils copied verbatim from pi. MIT (c) 2025 Mario Zechner.
//
// Why a STUB is faithful here: pidgin's deno plane is HEADLESS. A tool/command
// extension's display hooks (`renderResult` / `renderCall`) — the ONLY places the
// pi-tui component classes are touched — are never called: invoke runs the tool's
// `execute` only, and inventory records metadata alone. So resolving
// `@earendil-works/pi-tui` to marker classes that STORE their constructor args and
// return a `{ __piTuiStub }` marker from `render()` is behavior-faithful for
// load / register / invoke; only display output changes, and the headless plane
// produces none. Upstream `Text`/`Box`/… (pi packages/tui/src/components/*.ts,
// tui.ts) are pure descriptor classes (constructor stores args; `render(width)` is
// a pure string -> string[] transform), so a marker faithfully stands in wherever
// the plane exercises them. This deliberately does NOT reproduce live-terminal
// rendering.
//
// Scope of exports:
//   * Marker component classes — Text, Box, Container, Spacer, Markdown — the UI
//     components render-only examples import (e.g. structured-output.ts imports
//     `Text`, used ONLY inside `renderResult`).
//   * `Key` — the pure, self-contained key-identifier const from
//     packages/tui/src/keys.ts, ported to JS (its TypeScript generic parameters
//     are erased, leaving an identical runtime body). Included as the one trivial
//     pure export; no bundled example needs it, but it is cheap and faithful.
//
// Intentionally NOT included (documented follow-ups, neither needed by any bundled
// example, both display/input-time code never reached on the headless plane):
//   * `truncateToWidth` (packages/tui/src/utils.ts) — cannot be copied verbatim:
//     its module imports the external `get-east-asian-width` npm package at top
//     level (for `visibleWidth`/`graphemeWidth`), which is not vendored, so
//     including it would break module loading. Vendoring that package is out of
//     scope for this render-stub slice.
//   * `matchesKey` (packages/tui/src/keys.ts) — not trivially faithful: it pulls
//     in ~40 private helpers/tables and references Node's `process.env` (via
//     `isWindowsTerminalSession`). It is input-time terminal-parsing code, never
//     invoked on the headless plane, and no bundled example imports it.

/**
 * Text component marker. Upstream (packages/tui/src/components/text.ts) wraps and
 * pads text at `render(width)`; here the constructor stores the args and
 * `render()` returns a marker. `setText`/`setCustomBgFn`/`invalidate` mirror the
 * upstream surface as no-ops so construction-time calls do not throw.
 */
export class Text {
	constructor(text = "", paddingX = 1, paddingY = 1, customBgFn) {
		this.text = text;
		this.paddingX = paddingX;
		this.paddingY = paddingY;
		this.customBgFn = customBgFn;
	}
	setText(text) {
		this.text = text;
	}
	setCustomBgFn(customBgFn) {
		this.customBgFn = customBgFn;
	}
	invalidate() {}
	render(_width) {
		return { __piTuiStub: "Text" };
	}
}

/**
 * Box container marker. Upstream (components/box.ts) is a padded/background
 * container; here `addChild`/`removeChild`/`clear` just track children so
 * construction-time composition works, and `render()` returns a marker.
 */
export class Box {
	constructor(paddingX = 1, paddingY = 1, bgFn) {
		this.children = [];
		this.paddingX = paddingX;
		this.paddingY = paddingY;
		this.bgFn = bgFn;
	}
	addChild(component) {
		this.children.push(component);
	}
	removeChild(component) {
		const index = this.children.indexOf(component);
		if (index !== -1) {
			this.children.splice(index, 1);
		}
	}
	clear() {
		this.children = [];
	}
	setBgFn(bgFn) {
		this.bgFn = bgFn;
	}
	invalidate() {}
	render(_width) {
		return { __piTuiStub: "Box" };
	}
}

/**
 * Container marker. Upstream (tui.ts) stacks child renders; here children are
 * tracked and `render()` returns a marker.
 */
export class Container {
	constructor() {
		this.children = [];
	}
	addChild(component) {
		this.children.push(component);
	}
	removeChild(component) {
		const index = this.children.indexOf(component);
		if (index !== -1) {
			this.children.splice(index, 1);
		}
	}
	clear() {
		this.children = [];
	}
	invalidate() {}
	render(_width) {
		return { __piTuiStub: "Container" };
	}
}

/**
 * Spacer marker. Upstream (components/spacer.ts) renders N empty lines; here the
 * line count is stored and `render()` returns a marker.
 */
export class Spacer {
	constructor(lines = 1) {
		this.lines = lines;
	}
	setLines(lines) {
		this.lines = lines;
	}
	invalidate() {}
	render(_width) {
		return { __piTuiStub: "Spacer" };
	}
}

/**
 * Markdown marker. Upstream (components/markdown.ts) renders parsed markdown; the
 * constructor mirrors its positional args (text, paddingX, paddingY, theme,
 * defaultTextStyle, options) and `render()` returns a marker.
 */
export class Markdown {
	constructor(text = "", paddingX = 1, paddingY = 1, theme, defaultTextStyle, options) {
		this.text = text;
		this.paddingX = paddingX;
		this.paddingY = paddingY;
		this.theme = theme;
		this.defaultTextStyle = defaultTextStyle;
		this.options = options ? { ...options } : {};
	}
	setText(text) {
		this.text = text;
	}
	invalidate() {}
	render(_width) {
		return { __piTuiStub: "Markdown" };
	}
}

// `Key` copied from pi's packages/tui/src/keys.ts (the `export const Key` object,
// keys.ts:163). It is a pure, dependency-free helper for building typed key-id
// strings; the upstream generic type parameters (e.g. `<K extends BaseKey>`) are
// TypeScript-only and erase at transpile, so this JS port has an identical runtime
// body.
export const Key = {
	// Special keys
	escape: "escape",
	esc: "esc",
	enter: "enter",
	return: "return",
	tab: "tab",
	space: "space",
	backspace: "backspace",
	delete: "delete",
	insert: "insert",
	clear: "clear",
	home: "home",
	end: "end",
	pageUp: "pageUp",
	pageDown: "pageDown",
	up: "up",
	down: "down",
	left: "left",
	right: "right",
	f1: "f1",
	f2: "f2",
	f3: "f3",
	f4: "f4",
	f5: "f5",
	f6: "f6",
	f7: "f7",
	f8: "f8",
	f9: "f9",
	f10: "f10",
	f11: "f11",
	f12: "f12",

	// Symbol keys
	backtick: "`",
	hyphen: "-",
	equals: "=",
	leftbracket: "[",
	rightbracket: "]",
	backslash: "\\",
	semicolon: ";",
	quote: "'",
	comma: ",",
	period: ".",
	slash: "/",
	exclamation: "!",
	at: "@",
	hash: "#",
	dollar: "$",
	percent: "%",
	caret: "^",
	ampersand: "&",
	asterisk: "*",
	leftparen: "(",
	rightparen: ")",
	underscore: "_",
	plus: "+",
	pipe: "|",
	tilde: "~",
	leftbrace: "{",
	rightbrace: "}",
	colon: ":",
	lessthan: "<",
	greaterthan: ">",
	question: "?",

	// Single modifiers
	ctrl: (key) => `ctrl+${key}`,
	shift: (key) => `shift+${key}`,
	alt: (key) => `alt+${key}`,
	super: (key) => `super+${key}`,

	// Combined modifiers
	ctrlShift: (key) => `ctrl+shift+${key}`,
	shiftCtrl: (key) => `shift+ctrl+${key}`,
	ctrlAlt: (key) => `ctrl+alt+${key}`,
	altCtrl: (key) => `alt+ctrl+${key}`,
	shiftAlt: (key) => `shift+alt+${key}`,
	altShift: (key) => `alt+shift+${key}`,
	ctrlSuper: (key) => `ctrl+super+${key}`,
	superCtrl: (key) => `super+ctrl+${key}`,
	shiftSuper: (key) => `shift+super+${key}`,
	superShift: (key) => `super+shift+${key}`,
	altSuper: (key) => `alt+super+${key}`,
	superAlt: (key) => `super+alt+${key}`,

	// Triple modifiers
	ctrlShiftAlt: (key) => `ctrl+shift+alt+${key}`,
	ctrlShiftSuper: (key) => `ctrl+shift+super+${key}`,
};
