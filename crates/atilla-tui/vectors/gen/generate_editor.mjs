// straitjacket-allow-file:duplication — this generator's dump()/paths
// boilerplate intentionally mirrors generate_input_lists.mjs; each generator is
// a standalone script.
// straitjacket-allow-file:emoji — the CJK/emoji/umlaut string literals are
// UTF-8 width/segmentation test data fed to pi's own Editor, not decorative
// prose (mirrors generate_input_lists.mjs's emoji allow-file).
//
// Vector generator for the byte-exact Rust port of pi's TUI Editor CORE (PR
// C6a: buffer/render/word-wrap/vertical-move/paste-marker/history/kill-ring/
// undo/jump). Runs pi's OWN Editor + exported wordWrapLine from
// vendor/pi/packages/tui/src/components/editor.ts (Node 22 strips TS types
// natively) and dumps input scripts -> {getText, getCursor, render, expanded,
// lines, isShowingAutocomplete, onSubmit} that the Rust test suite asserts
// byte-identical.
//
// The editor scenarios replay the exact cases from pi's test/editor.test.ts for
// every describe block EXCEPT the async Autocomplete block (deferred to C6b),
// and the single "undoes autocomplete" case in the Undo block (also C6b, it
// needs a provider + flushAutocomplete seam). The abort-count case lives inside
// the Autocomplete block and is deferred with it (a Rust behavioral unit test
// in C6b). The ~14 wordWrapLine cases call pi's exported wordWrapLine directly.
//
// Run from this directory:  node generate_editor.mjs
// Output is written to ../../tests/vectors/*.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a.

import { Editor, wordWrapLine } from "../../../../vendor/pi/packages/tui/src/components/editor.ts";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "..", "tests", "vectors");
mkdirSync(outDir, { recursive: true });

let total = 0;
function dump(name, vectors) {
	const path = join(outDir, `${name}.json`);
	writeFileSync(path, `${JSON.stringify(vectors, null, "\t")}\n`);
	total += vectors.length;
	console.log(`  ${name}.json: ${vectors.length}`);
}

// Editor theme matching the tests' defaultEditorTheme: chalk.dim borders
// (level 3 -> \x1b[2m … \x1b[22m). The Rust replay uses the identical closure.
const editorTheme = {
	borderColor: (t) => `\x1b[2m${t}\x1b[22m`,
	selectList: {
		selectedPrefix: (t) => t,
		selectedText: (t) => t,
		description: (t) => t,
		scrollInfo: (t) => t,
		noMatch: (t) => t,
	},
};

// Minimal duck-typed TUI: the editor core only reads terminal.rows and calls
// requestRender() (a no-op here). rows is the terminal-rows seam.
function makeTui(rows = 24, cols = 80) {
	return { terminal: { rows, columns: cols }, requestRender: () => {} };
}

// Step builders.
const inp = (data) => ({ op: "input", data });
const setText = (text) => ({ op: "setText", text });
const insTxt = (text) => ({ op: "insertTextAtCursor", text });
const hist = (text) => ({ op: "addToHistory", text });
const focus = (focused) => ({ op: "focus", focused });
const render = (width) => ({ op: "render", width });
const expanded = () => ({ op: "expandedText" });
const getLinesOp = () => ({ op: "lines" });
const repeat = (n, step) => Array.from({ length: n }, () => step);

const UP = "\x1b[A";
const DOWN = "\x1b[B";
const LEFT = "\x1b[D";
const RIGHT = "\x1b[C";
const CTRL_A = "\x01";
const CTRL_E = "\x05";
const CTRL_W = "\x17";
const CTRL_Y = "\x19";
const CTRL_K = "\x0b";
const CTRL_U = "\x15";
const ALT_Y = "\x1by";
const ALT_D = "\x1bd";
const UNDO = "\x1b[45;5u";
const BS = "\x7f";
const DEL = "\x1b[3~";
const CTRL_LEFT = "\x1b[1;5D";
const CTRL_RIGHT = "\x1b[1;5C";
const CTRL_RB = "\x1d"; // Ctrl+]
const CTRL_ALT_RB = "\x1b\x1d"; // Ctrl+Alt+]
const ESC = "\x1b";

// positionCursor helper from the Sticky column tests, expanded inline.
function positionCursor(line, col) {
	return [...repeat(20, inp(UP)), ...repeat(line, inp(DOWN)), inp(CTRL_A), ...repeat(col, inp(RIGHT))];
}

// A large paste that becomes a "+N lines" marker (test helper pasteWithMarker).
function pasteLines(n) {
	const big = "line\n".repeat(n).replace(/\n$/, "");
	return inp(`\x1b[200~${big}\x1b[201~`);
}

function runEditor(spec) {
	const editor = new Editor(makeTui(spec.rows ?? 24), editorTheme, spec.options ?? {});
	editor.disableSubmit = spec.disableSubmit ?? false;
	const submits = [];
	editor.onSubmit = (t) => submits.push(t);

	const trace = [];
	for (const step of spec.steps) {
		let renderLines = null;
		let expandedText = null;
		let lines = null;
		switch (step.op) {
			case "input":
				editor.handleInput(step.data);
				break;
			case "setText":
				editor.setText(step.text);
				break;
			case "insertTextAtCursor":
				editor.insertTextAtCursor(step.text);
				break;
			case "addToHistory":
				editor.addToHistory(step.text);
				break;
			case "focus":
				editor.focused = step.focused;
				break;
			case "render":
				renderLines = editor.render(step.width);
				break;
			case "expandedText":
				expandedText = editor.getExpandedText();
				break;
			case "lines":
				lines = editor.getLines();
				break;
			default:
				throw new Error(`unknown editor op: ${step.op}`);
		}
		const cur = editor.getCursor();
		trace.push({
			...step,
			textAfter: editor.getText(),
			line: cur.line,
			col: cur.col,
			showing: editor.isShowingAutocomplete(),
			render: renderLines,
			expanded: expandedText,
			lines,
		});
	}
	return {
		name: spec.name,
		rows: spec.rows ?? 24,
		options: spec.options ?? {},
		disableSubmit: spec.disableSubmit ?? false,
		steps: trace,
		submits,
	};
}

const scenarios = [];
const add = (name, steps, extra = {}) => scenarios.push(runEditor({ name, steps, ...extra }));

// ===========================================================================
// Prompt history navigation
// ===========================================================================
add("history: does nothing on Up when empty", [inp(UP)]);
add("history: shows most recent on Up when empty", [hist("first prompt"), hist("second prompt"), inp(UP)]);
add("history: cycles through entries on repeated Up", [
	hist("first"), hist("second"), hist("third"),
	inp(UP), inp(UP), inp(UP), inp(UP),
]);
add("history: jumps to start before entering history from non-empty draft", [
	hist("prompt"), setText("draft"), inp(LEFT), inp(LEFT), inp(UP), inp(UP), inp(DOWN),
]);
add("history: navigates forward with Down", [
	hist("first"), hist("second"), hist("third"), setText("draft"),
	inp(UP), inp(UP), inp(UP), inp(UP), inp(DOWN), inp(DOWN), inp(DOWN),
]);
add("history: exits history mode when typing a character", [hist("old prompt"), inp(UP), inp("x")]);
add("history: exits history mode on setText", [hist("first"), hist("second"), inp(UP), setText(""), inp(UP)]);
add("history: does not add empty strings", [hist(""), hist("   "), hist("valid"), inp(UP), inp(UP)]);
add("history: does not add consecutive duplicates", [hist("same"), hist("same"), hist("same"), inp(UP), inp(UP)]);
add("history: allows non-consecutive duplicates", [
	hist("first"), hist("second"), hist("first"), inp(UP), inp(UP), inp(UP),
]);
add("history: uses cursor movement when editor has content", [
	hist("history item"), setText("line1\nline2"), inp(UP), inp("X"),
]);
add("history: limits to 100 entries", [
	...Array.from({ length: 105 }, (_, i) => hist(`prompt ${i}`)),
	...repeat(100, inp(UP)),
	inp(UP),
]);
add("history: places cursor at start after browsing upward", [
	hist("older entry"), hist("line1\nline2\nline3"), inp(UP), inp(UP),
]);
add("history: places cursor at end after browsing downward", [
	hist("older entry"), hist("line1\nline2\nline3"), hist("newer entry"),
	inp(UP), inp(UP), inp(UP), inp(DOWN), inp(DOWN),
]);
add("history: opposite-direction cursor movement within multi-line entry", [
	hist("line1\nline2\nline3"), inp(UP), inp(DOWN), inp(UP),
]);

// ===========================================================================
// public state accessors
// ===========================================================================
add("accessors: returns cursor position", [inp("a"), inp("b"), inp("c"), inp(LEFT)]);
add("accessors: returns lines as a defensive copy", [setText("a\nb"), getLinesOp()]);

// ===========================================================================
// Backslash+Enter newline workaround
// ===========================================================================
add("backslash: inserts backslash immediately", [inp("\\")]);
add("backslash: converts standalone backslash to newline on Enter", [inp("\\"), inp("\r")]);
add("backslash: inserts backslash normally when followed by other characters", [inp("\\"), inp("x")]);
add("backslash: does not trigger newline when backslash not before cursor", [inp("\\"), inp("x"), inp("\r")]);
add("backslash: only removes one backslash when multiple present", [inp("\\"), inp("\\"), inp("\\"), inp("\r")]);

// ===========================================================================
// Kitty CSI-u handling
// ===========================================================================
add("kitty: ignores printable CSI-u with unsupported modifiers", [inp("\x1b[99;9u")]);
add("kitty: inserts shifted CSI-u letters as text", [inp("\x1b[69;2u")]);
add("kitty: inserts shifted xterm modifyOtherKeys letters as text", [inp("\x1b[27;2;69~")]);

// ===========================================================================
// Unicode text editing behavior
// ===========================================================================
add("unicode: inserts mixed ASCII umlauts emojis", [
	...[..."Hello "].map(inp), inp("ä"), inp("ö"), inp("ü"), inp(" "), inp("😀"),
]);
add("unicode: deletes umlauts with Backspace", [inp("ä"), inp("ö"), inp("ü"), inp(BS)]);
add("unicode: deletes multi-code-unit emoji with single Backspace", [inp("😀"), inp("👍"), inp(BS)]);
add("unicode: inserts at correct position after cursor movement over umlauts", [
	inp("ä"), inp("ö"), inp("ü"), inp(LEFT), inp(LEFT), inp("x"),
]);
add("unicode: moves cursor across emojis with single arrow", [
	inp("😀"), inp("👍"), inp("🎉"), inp(LEFT), inp(LEFT), inp("x"),
]);
add("unicode: preserves umlauts across line breaks", [
	inp("ä"), inp("ö"), inp("ü"), inp("\n"), inp("Ä"), inp("Ö"), inp("Ü"),
]);
add("unicode: replaces document via setText", [setText("Hällö Wörld! 😀 äöüÄÖÜß")]);
add("unicode: Ctrl+A then insert at beginning", [inp("a"), inp("b"), inp(CTRL_A), inp("x")]);
add("unicode: deletes words with Ctrl+W and Alt+Backspace", [
	setText("foo bar baz"), inp(CTRL_W),
	setText("foo bar   "), inp(CTRL_W),
	setText("foo bar..."), inp(CTRL_W),
	setText("foo.bar"), inp(CTRL_W),
	setText("foo:bar"), inp(CTRL_W),
	setText("line one\nline two"), inp(CTRL_W),
	setText("line one\n"), inp(CTRL_W),
	setText("foo 😀😀 bar"), inp(CTRL_W), inp(CTRL_W),
	setText("foo bar"), inp("\x1b\x7f"),
]);
add("unicode: navigates words with Ctrl+Left/Right", [
	setText("foo bar... baz"),
	inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_RIGHT), inp(CTRL_RIGHT), inp(CTRL_RIGHT),
	setText("   foo bar"), inp(CTRL_A), inp(CTRL_RIGHT),
	setText("foo.bar baz"), inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_LEFT),
	inp(CTRL_A), inp(CTRL_RIGHT), inp(CTRL_RIGHT), inp(CTRL_RIGHT),
]);
add("unicode: stops at fullwidth Chinese punctuation", [
	setText("你好，世界"),
	inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_RIGHT), inp(CTRL_RIGHT), inp(CTRL_RIGHT),
]);
add("unicode: mixed CJK and ASCII word movement", [
	setText("hello你好，world世界"),
	inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_LEFT), inp(CTRL_LEFT),
	inp(CTRL_RIGHT), inp(CTRL_RIGHT), inp(CTRL_RIGHT), inp(CTRL_RIGHT), inp(CTRL_RIGHT),
]);

// ===========================================================================
// Grapheme-aware text wrapping (render-based)
// ===========================================================================
add("gwrap: wraps wide emojis", [setText("Hello ✅ World"), render(20)]);
add("gwrap: wraps long text with emojis", [setText("✅✅✅✅✅✅"), render(10)]);
add("gwrap: isolated Thai AM cluster", [setText("ำabc"), render(8)]);
add("gwrap: isolated Lao AM cluster", [setText("ຳabc"), render(8)]);
add("gwrap: wraps CJK each 2 cols", [setText("日本語テスト"), render(11)]);
add("gwrap: mixed ASCII and wide", [setText("Test ✅ OK 日本"), render(16)]);
add("gwrap: cursor on wide characters", [setText("A✅B"), render(20)]);
add("gwrap: emoji at wrap boundary", [setText("0123456789✅"), render(11)]);
add("gwrap: cursor at end before wrap paddingX=0", [...repeat(9, inp("a")), render(10), inp("a"), render(10)]);
add("gwrap: cursor at end before wrap paddingX=1", [...repeat(9, inp("a")), render(11), inp("a"), render(11)], {
	options: { paddingX: 1 },
});

// ===========================================================================
// Word wrapping (render-based subset; direct wordWrapLine below)
// ===========================================================================
add("wwrap: wraps at word boundaries", [
	setText("Hello world this is a test of word wrapping functionality"), render(40),
]);
add("wwrap: no leading whitespace after wrap", [setText("Word1 Word2 Word3 Word4 Word5 Word6"), render(20)]);
add("wwrap: breaks long URLs at character level", [
	setText("Check https://example.com/very/long/path/that/exceeds/width here"), render(30),
]);
add("wwrap: preserves multiple spaces within words", [setText("Word1   Word2    Word3"), render(50)]);
add("wwrap: handles empty string", [setText(""), render(40)]);
add("wwrap: single word that fits exactly", [setText("1234567890"), render(11)]);

// ===========================================================================
// Kill ring
// ===========================================================================
add("kill: Ctrl+W saves and Ctrl+Y yanks", [setText("foo bar baz"), inp(CTRL_W), inp(CTRL_A), inp(CTRL_Y)]);
add("kill: Ctrl+U saves", [
	setText("hello world"), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(CTRL_U), inp(CTRL_Y),
]);
add("kill: Ctrl+K saves", [setText("hello world"), inp(CTRL_A), inp(CTRL_K), inp(CTRL_Y)]);
add("kill: Ctrl+Y does nothing when empty", [setText("test"), inp(CTRL_Y)]);
add("kill: Alt+Y cycles after Ctrl+Y", [
	setText("first"), inp(CTRL_W), setText("second"), inp(CTRL_W), setText("third"), inp(CTRL_W),
	inp(CTRL_Y), inp(ALT_Y), inp(ALT_Y), inp(ALT_Y),
]);
add("kill: Alt+Y does nothing if not preceded by yank", [
	setText("test"), inp(CTRL_W), setText("other"), inp("x"), inp(ALT_Y),
]);
add("kill: Alt+Y does nothing if <=1 entry", [setText("only"), inp(CTRL_W), inp(CTRL_Y), inp(ALT_Y)]);
add("kill: consecutive Ctrl+W accumulates", [
	setText("one two three"), inp(CTRL_W), inp(CTRL_W), inp(CTRL_W), inp(CTRL_Y),
]);
add("kill: Ctrl+U accumulates multiline including newlines", [
	setText("line1\nline2\nline3"), inp(CTRL_U), inp(CTRL_U), inp(CTRL_U), inp(CTRL_U), inp(CTRL_U), inp(CTRL_Y),
]);
add("kill: backward prepend forward append during accumulation", [
	setText("prefix|suffix"), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(CTRL_K), inp(CTRL_K), inp(CTRL_Y),
]);
add("kill: non-delete actions break accumulation", [
	setText("foo bar baz"), inp(CTRL_W), inp("x"), inp(CTRL_W), inp(CTRL_Y), inp(ALT_Y),
]);
add("kill: non-yank actions break Alt+Y chain", [
	setText("first"), inp(CTRL_W), setText("second"), inp(CTRL_W), setText(""),
	inp(CTRL_Y), inp("x"), inp(ALT_Y),
]);
add("kill: rotation persists after cycling", [
	setText("first"), inp(CTRL_W), setText("second"), inp(CTRL_W), setText("third"), inp(CTRL_W), setText(""),
	inp(CTRL_Y), inp(ALT_Y), inp("x"), setText(""), inp(CTRL_Y),
]);
add("kill: consecutive deletions across lines coalesce", [
	setText("1\n2\n3"), inp(CTRL_W), inp(CTRL_W), inp(CTRL_W), inp(CTRL_W), inp(CTRL_W), inp(CTRL_Y),
]);
add("kill: Ctrl+K at line end deletes newline and coalesces", [
	setText(""), inp("a"), inp("b"), inp("\n"), inp("c"), inp("d"), inp(UP), inp(CTRL_E),
	inp(CTRL_K), inp(CTRL_K), inp(CTRL_Y),
]);
add("kill: handles yank in middle of text", [
	setText("word"), inp(CTRL_W), setText("hello world"), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(CTRL_Y),
]);
add("kill: handles yank-pop in middle of text", [
	setText("FIRST"), inp(CTRL_W), setText("SECOND"), inp(CTRL_W),
	setText("hello world"), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(CTRL_Y), inp(ALT_Y),
]);
add("kill: multiline yank and yank-pop in middle", [
	setText("SINGLE"), inp(CTRL_W), setText("A\nB"), inp(CTRL_U), inp(CTRL_U), inp(CTRL_U),
	setText("hello world"), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(CTRL_Y), inp(ALT_Y),
]);
add("kill: Alt+D deletes word forward and saves", [
	setText("hello world test"), inp(CTRL_A), inp(ALT_D), inp(ALT_D), inp(CTRL_Y),
]);
add("kill: Alt+D at end of line deletes newline", [
	setText("line1\nline2"), inp(UP), inp(CTRL_E), inp(ALT_D), inp(CTRL_Y),
]);

// ===========================================================================
// Undo (Autocomplete-dependent "undoes autocomplete" case deferred to C6b)
// ===========================================================================
add("undo: does nothing when stack empty", [inp(UNDO)]);
add("undo: coalesces consecutive word characters", [...[..."hello world"].map(inp), inp(UNDO), inp(UNDO)]);
add("undo: undoes spaces one at a time", [
	...[..."hello"].map(inp), inp(" "), inp(" "), inp(UNDO), inp(UNDO), inp(UNDO),
]);
add("undo: undoes newlines and signals next word capture", [
	...[..."hello"].map(inp), inp("\n"), ...[..."world"].map(inp), inp(UNDO), inp(UNDO), inp(UNDO),
]);
add("undo: undoes backspace", [...[..."hello"].map(inp), inp(BS), inp(UNDO)]);
add("undo: undoes forward delete", [...[..."hello"].map(inp), inp(CTRL_A), inp(RIGHT), inp(DEL), inp(UNDO)]);
add("undo: undoes Ctrl+W", [...[..."hello world"].map(inp), inp(CTRL_W), inp(UNDO)]);
add("undo: undoes Ctrl+K", [
	...[..."hello world"].map(inp), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(CTRL_K), inp(UNDO), inp("|"),
]);
add("undo: undoes Ctrl+U", [
	...[..."hello world"].map(inp), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(CTRL_U), inp(UNDO),
]);
add("undo: undoes yank", [...[..."hello"].map(inp), inp(" "), inp(CTRL_W), inp(CTRL_Y), inp(UNDO)]);
add("undo: undoes single-line paste atomically", [
	setText("hello world"), inp(CTRL_A), ...repeat(5, inp(RIGHT)),
	inp("\x1b[200~beep boop\x1b[201~"), inp(UNDO), inp("|"),
]);
// Core paste behavior; the provider suggestion-count assertion is C6b.
add("undo: single-line paste does not leak (no autocomplete)", [
	inp("\x1b[200~look at @node_modules/react/index.js please\x1b[201~"),
]);
add("undo: decodes CSI-u Ctrl+letter inside bracketed paste", [
	inp("\x1b[200~line1\x1b[106;5uline2\x1b[106;5uline3\x1b[201~"),
]);
add("undo: undoes multi-line paste atomically", [
	setText("hello world"), inp(CTRL_A), ...repeat(5, inp(RIGHT)),
	inp("\x1b[200~line1\nline2\nline3\x1b[201~"), inp(UNDO), inp("|"),
]);
add("undo: undoes insertTextAtCursor atomically", [
	setText("hello world"), inp(CTRL_A), ...repeat(5, inp(RIGHT)), insTxt("/tmp/image.png"), inp(UNDO), inp("|"),
]);
add("undo: insertTextAtCursor handles multiline text", [
	setText("hello world"), inp(CTRL_A), ...repeat(5, inp(RIGHT)), insTxt("line1\nline2\nline3"), inp(UNDO),
]);
add("undo: insertTextAtCursor normalizes CRLF and CR", [
	setText(""), insTxt("a\r\nb\r\nc"), inp(UNDO), insTxt("x\ry\rz"),
]);
add("undo: undoes setText to empty string", [...[..."hello world"].map(inp), setText(""), inp(UNDO)]);
add("undo: clears undo stack on submit", [...[..."hello"].map(inp), inp("\r"), inp(UNDO)]);
add("undo: exits history browsing mode on undo", [
	hist("hello"), ...[..."world"].map(inp), inp(CTRL_W), inp(UP), inp(UNDO), inp(UNDO),
]);
add("undo: restores pre-history state after multiple navigations", [
	hist("first"), hist("second"), hist("third"), ...[..."current"].map(inp), inp(CTRL_W),
	inp(UP), inp(UP), inp(UP), inp(UNDO), inp(UNDO),
]);
add("undo: cursor movement starts new undo unit", [
	...[..."hello world"].map(inp), ...repeat(5, inp(LEFT)), inp("l"), inp("o"), inp("l"), inp(UNDO), inp("|"),
]);
add("undo: no-op delete operations do not push snapshots", [
	...[..."hello"].map(inp), inp(CTRL_W), inp(CTRL_W), inp(CTRL_W), inp(UNDO),
]);

// ===========================================================================
// Character jump (Ctrl+])
// ===========================================================================
add("jump: forward to first occurrence on same line", [setText("hello world"), inp(CTRL_A), inp(CTRL_RB), inp("o")]);
add("jump: forward to next occurrence after cursor", [
	setText("hello world"), inp(CTRL_A), ...repeat(4, inp(RIGHT)), inp(CTRL_RB), inp("o"),
]);
add("jump: forward across multiple lines", [
	setText("abc\ndef\nghi"), inp(UP), inp(UP), inp(CTRL_A), inp(CTRL_RB), inp("g"),
]);
add("jump: backward to first occurrence before cursor", [setText("hello world"), inp(CTRL_ALT_RB), inp("o")]);
add("jump: backward across multiple lines", [setText("abc\ndef\nghi"), inp(CTRL_ALT_RB), inp("a")]);
add("jump: does nothing when not found forward", [setText("hello world"), inp(CTRL_A), inp(CTRL_RB), inp("z")]);
add("jump: does nothing when not found backward", [setText("hello world"), inp(CTRL_ALT_RB), inp("z")]);
add("jump: is case-sensitive", [
	setText("Hello World"), inp(CTRL_A), inp(CTRL_RB), inp("h"), inp(CTRL_RB), inp("W"),
]);
add("jump: cancels jump mode when Ctrl+] pressed again", [
	setText("hello world"), inp(CTRL_A), inp(CTRL_RB), inp(CTRL_RB), inp("o"),
]);
add("jump: cancels jump mode on Escape and processes it", [
	setText("hello world"), inp(CTRL_A), inp(CTRL_RB), inp(ESC), inp("o"),
]);
add("jump: cancels backward jump mode when Ctrl+Alt+] pressed again", [
	setText("hello world"), inp(CTRL_ALT_RB), inp(CTRL_ALT_RB), inp("o"),
]);
add("jump: searches for special characters", [
	setText("foo(bar) = baz;"), inp(CTRL_A), inp(CTRL_RB), inp("("), inp(CTRL_RB), inp("="),
]);
add("jump: handles empty text gracefully", [setText(""), inp(CTRL_RB), inp("x")]);
add("jump: resets lastAction when jumping", [
	setText("hello world"), inp(CTRL_A), inp("x"), inp(CTRL_RB), inp("o"), inp("Y"), inp(UNDO),
]);

// ===========================================================================
// Sticky column
// ===========================================================================
add("sticky: preserves target column moving up through shorter line", [
	setText("2222222222x222\n\n1111111111_111111111111"),
	inp(CTRL_A), ...repeat(10, inp(RIGHT)), inp(UP), inp(UP),
]);
add("sticky: preserves target column moving down through shorter line", [
	setText("1111111111_111\n\n2222222222x222222222222"),
	inp(UP), inp(UP), inp(CTRL_A), ...repeat(10, inp(RIGHT)), inp(DOWN), inp(DOWN),
]);
add("sticky: resets on horizontal movement (left)", [
	setText("1234567890\n\n1234567890"), inp(CTRL_A), ...repeat(5, inp(RIGHT)),
	inp(UP), inp(UP), inp(LEFT), inp(DOWN), inp(DOWN),
]);
add("sticky: resets on horizontal movement (right)", [
	setText("1234567890\n\n1234567890"), inp(UP), inp(UP), inp(CTRL_A), ...repeat(5, inp(RIGHT)),
	inp(DOWN), inp(DOWN), inp(RIGHT), inp(UP), inp(UP),
]);
add("sticky: resets on typing", [
	setText("1234567890\n\n1234567890"), inp(CTRL_A), ...repeat(8, inp(RIGHT)),
	inp(UP), inp(UP), inp("X"), inp(DOWN), inp(DOWN),
]);
add("sticky: resets on backspace", [
	setText("1234567890\n\n1234567890"), inp(CTRL_A), ...repeat(8, inp(RIGHT)),
	inp(UP), inp(UP), inp(BS), inp(DOWN), inp(DOWN),
]);
add("sticky: resets on Ctrl+A", [
	setText("1234567890\n\n1234567890"), inp(CTRL_A), ...repeat(8, inp(RIGHT)), inp(UP), inp(CTRL_A), inp(UP),
]);
add("sticky: resets on Ctrl+E", [
	setText("12345\n\n1234567890"), inp(CTRL_A), ...repeat(3, inp(RIGHT)),
	inp(UP), inp(UP), inp(CTRL_E), inp(DOWN), inp(DOWN),
]);
add("sticky: resets on word movement (Ctrl+Left)", [
	setText("hello world\n\nhello world"), inp(UP), inp(UP), inp(CTRL_LEFT), inp(DOWN), inp(DOWN),
]);
add("sticky: resets on word movement (Ctrl+Right)", [
	setText("hello world\n\nhello world"), inp(UP), inp(UP), inp(CTRL_A),
	inp(DOWN), inp(DOWN), inp(CTRL_RIGHT), inp(UP), inp(UP),
]);
add("sticky: resets on undo", [
	setText("1234567890\n\n1234567890"), inp(UP), inp(UP), inp(CTRL_A), ...repeat(8, inp(RIGHT)),
	inp(DOWN), inp(DOWN), inp("X"), inp(UP), inp(UP), inp(UNDO), inp(UP), inp(UP),
]);
add("sticky: multiple consecutive up/down movements", [
	setText("1234567890\nab\ncd\nef\n1234567890"), inp(CTRL_A), ...repeat(7, inp(RIGHT)),
	inp(UP), inp(UP), inp(UP), inp(UP), inp(DOWN), inp(DOWN), inp(DOWN), inp(DOWN),
]);
add("sticky: moves through wrapped visual lines without getting stuck", [
	setText("short\n123456789012345678901234567890"), render(15), inp(UP), inp(UP), inp(UP),
], { rows: 24 });
add("sticky: setText resets sticky column", [
	setText("1234567890\n\n1234567890"), inp(CTRL_A), ...repeat(8, inp(RIGHT)), inp(UP),
	setText("abcdefghij\n\nabcdefghij"), inp(UP), inp(UP),
]);
add("sticky: sets preferredVisualCol pressing right at end of prompt", [
	setText("111111111x1111111111\n\n333333333_"), inp(UP), inp(UP), inp(CTRL_E),
	inp(DOWN), inp(DOWN), inp(RIGHT), inp(UP), inp(UP),
]);
add("sticky: resizes when preferredVisualCol on same line", [
	setText("12345678901234567890\n\n12345678901234567890"), inp(CTRL_A), ...repeat(15, inp(RIGHT)),
	inp(UP), inp(UP), render(12), inp(DOWN), inp(DOWN),
]);
add("sticky: resizes when preferredVisualCol on different line", [
	setText("short\n12345678901234567890"), inp(CTRL_A), ...repeat(15, inp(RIGHT)),
	inp(UP), render(10), inp(DOWN), inp(UP), render(80), inp(DOWN),
]);
add("sticky: rewrapped lines target fits current visual column", [
	setText("abcdefghijklmnopqr\n123456789012345678"),
	...positionCursor(0, 18), render(10), inp(DOWN), render(80), inp(UP), inp(DOWN),
]);
add("sticky: rewrapped lines target shorter than current visual column", [
	setText("abcdefghijklmnopqr\n123456789012345678\nab"),
	...positionCursor(0, 18), render(10), inp(DOWN), render(80), inp(DOWN), inp(UP),
]);

// ===========================================================================
// Paste marker atomic behavior
// ===========================================================================
add("paste: creates a paste marker for large pastes", [pasteLines(20)]);
add("paste: single unit for right arrow", [
	inp("A"), pasteLines(20), inp("B"), inp(CTRL_A), inp(RIGHT), inp(RIGHT), inp(RIGHT),
]);
add("paste: single unit for left arrow", [
	inp("A"), pasteLines(20), inp("B"), inp(LEFT), inp(LEFT), inp(LEFT),
]);
add("paste: single unit for backspace", [
	inp("A"), pasteLines(20), inp("B"), inp(CTRL_A), inp(RIGHT), inp(RIGHT), inp(BS),
]);
add("paste: single unit for forward delete", [
	inp("A"), pasteLines(20), inp("B"), inp(CTRL_A), inp(RIGHT), inp(DEL),
]);
add("paste: single unit for word movement", [
	inp("X"), inp(" "), pasteLines(20), inp(" "), inp("Y"), inp(CTRL_A), inp(CTRL_RIGHT), inp(CTRL_RIGHT),
]);
add("paste: undo restores marker after backspace deletion", [
	inp("A"), pasteLines(20), inp("B"), inp(CTRL_A), inp(RIGHT), inp(RIGHT), inp(BS), inp(UNDO),
]);
add("paste: multiple markers in same line", [
	pasteLines(20), inp(" "), pasteLines(20), inp(CTRL_A), inp(RIGHT), inp(RIGHT), inp(RIGHT),
]);
add("paste: manually typed marker-like text not atomic", [
	...[..."[paste #99 +5 lines]"].map(inp), inp(CTRL_A), inp(RIGHT),
]);
add("paste: does not crash when marker wider than terminal width", [
	inp(`\x1b[200~${"line\n".repeat(47).replace(/\n$/, "")}\x1b[201~`), render(8),
]);
add("paste: does not crash when text + marker exceeds width with cursor on marker", [
	...repeat(35, inp("b")), inp(`\x1b[200~${"line\n".repeat(27).replace(/\n$/, "")}\x1b[201~`),
	...repeat(4, inp("b")), inp(LEFT), inp(LEFT), inp(LEFT), inp(LEFT), inp(LEFT), render(54),
]);
add("paste: wordWrapLine re-checks overflow after backtracking", [
	inp(" "), ...repeat(35, inp("b")), inp(`\x1b[200~${"line\n".repeat(27).replace(/\n$/, "")}\x1b[201~`),
	...repeat(4, inp("b")), render(54),
]);
add("paste: expands large pasted content literally in getExpandedText", [
	inp(`\x1b[200~${[
		"line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8", "line 9", "line 10",
		"tokens $1 $2 $& $$ $` $' end",
	].join("\n")}\x1b[201~`),
	expanded(),
]);
add("paste: snaps to marker start navigating down into it", [
	setText("12345678901234567890\n\nhello "),
	inp(`\x1b[200~${"x".repeat(2000)}\x1b[201~`), render(80),
	inp(UP), inp(UP), inp(CTRL_A), ...repeat(10, inp(RIGHT)), inp(DOWN), inp(DOWN),
]);
add("paste: preserves sticky column through marker line", [
	...[..."1234567890123456"].map(inp), inp("\n"), inp("\n"),
	inp(`\x1b[200~${"x".repeat(2000)}\x1b[201~`), inp("\n"), inp("\n"), ...[..."abcdefghijklmnop"].map(inp),
	render(30), ...repeat(4, inp(UP)), inp(CTRL_A), ...repeat(10, inp(RIGHT)),
	inp(DOWN), inp(DOWN), inp(DOWN), inp(DOWN),
], { rows: 24 });
add("paste: does not get stuck moving down from multi-visual-line marker", [
	...[..."abcdefgh"].map(inp), inp(`\x1b[200~${"line\n".repeat(100).replace(/\n$/, "")}\x1b[201~`),
	...[..."ijklmnopqr"].map(inp), inp("\n"), ...[..."123456789012345678"].map(inp), render(20),
	inp(UP), inp(CTRL_A), ...repeat(6, inp(RIGHT)), inp(DOWN), inp(DOWN), inp(UP), inp(UP),
], { rows: 24 });
add("paste: skips marker continuation VLs when preferred col in marker tail", [
	...[..."abcdefgh"].map(inp), inp(`\x1b[200~${"line\n".repeat(100).replace(/\n$/, "")}\x1b[201~`),
	...[..."ijklmnopqr"].map(inp), inp("\n"), ...[..."123456789012345678"].map(inp), render(20),
	inp(UP), inp(CTRL_A), ...repeat(3, inp(RIGHT)), inp(DOWN), inp(DOWN), inp(UP), inp(UP),
], { rows: 24 });
add("paste: submits large pasted content literally", [
	inp(`\x1b[200~${[
		"line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8", "line 9", "line 10",
		"tokens $1 $2 $& $$ $` $' end",
	].join("\n")}\x1b[201~`),
	inp("\r"),
]);

dump("editor_scenarios", scenarios);

// ===========================================================================
// wordWrapLine direct (pure function) vectors
// ===========================================================================
const wrapVectors = [];
function addWrap(name, line, maxWidth, segments = null) {
	const chunks = wordWrapLine(line, maxWidth, segments ?? undefined);
	wrapVectors.push({
		name,
		line,
		maxWidth,
		segments,
		chunks: chunks.map((c) => ({ text: c.text, startIndex: c.startIndex, endIndex: c.endIndex })),
	});
}

addWrap("word ends exactly at width", "hello world test", 11);
addWrap("keeps whitespace at width boundary", "hello world test", 12);
addWrap("unbreakable word filling width then space", "aaaaaaaaaaaa aaaa", 12);
addWrap("word fits width but not remaining space", "      aaaaaaaaaaaa", 12);
addWrap("multi-space + following word together when fit", "Lorem ipsum dolor sit amet,    consectetur", 30);
addWrap("multi-space + following word fill width exactly", "Lorem ipsum dolor sit amet,              consectetur", 30);
addWrap("word + multi-space + word exceeds width", "Lorem ipsum dolor sit amet,               consectetur", 30);
addWrap("breaks long whitespace at line boundary", "Lorem ipsum dolor sit amet,                         consectetur", 30);
addWrap("breaks long whitespace at line boundary 2", "Lorem ipsum dolor sit amet,                          consectetur", 30);
addWrap("breaks whitespace spanning full lines", "Lorem ipsum dolor sit amet,                                     consectetur", 30);
addWrap("force-break wide char after word boundary wrap", ` ${"a".repeat(186)}你`, 187);

{
	const marker = "[paste #1 +20 lines]";
	const line = `A${marker}B`;
	addWrap("oversized atomic segment across chunks", line, 10, [
		{ segment: "A", index: 0, input: line },
		{ segment: marker, index: 1, input: line },
		{ segment: "B", index: 1 + marker.length, input: line },
	]);
}
{
	const marker = "[paste #1 +20 lines]";
	const line = `${marker}B`;
	addWrap("oversized atomic segment at start of line", line, 10, [
		{ segment: marker, index: 0, input: line },
		{ segment: "B", index: marker.length, input: line },
	]);
}
{
	const marker = "[paste #1 +20 lines]";
	const line = `A${marker}`;
	addWrap("oversized atomic segment at end of line", line, 10, [
		{ segment: "A", index: 0, input: line },
		{ segment: marker, index: 1, input: line },
	]);
}
{
	const m1 = "[paste #1 +20 lines]";
	const m2 = "[paste #2 +30 lines]";
	const line = `${m1}${m2}`;
	addWrap("consecutive oversized atomic segments", line, 10, [
		{ segment: m1, index: 0, input: line },
		{ segment: m2, index: m1.length, input: line },
	]);
}
{
	const marker = "[paste #1 +20 lines]";
	const line = `${marker} hello world`;
	const segs = [{ segment: marker, index: 0, input: line }];
	const tail = " hello world";
	for (let i = 0; i < tail.length; i++) {
		segs.push({ segment: tail[i], index: marker.length + i, input: line });
	}
	addWrap("wraps normally after oversized atomic segment", line, 10, segs);
}

dump("editor_wordwrap", wrapVectors);

console.log(`\ntotal editor vectors: ${total}`);
