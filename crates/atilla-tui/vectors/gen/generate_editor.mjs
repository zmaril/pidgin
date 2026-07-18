// straitjacket-allow-file:duplication — this generator's dump()/paths
// boilerplate intentionally mirrors generate_input_lists.mjs; each generator is
// a standalone script.
// straitjacket-allow-file:emoji — the CJK/emoji/umlaut string literals are
// UTF-8 width/segmentation test data fed to pi's own Editor, not decorative
// prose (mirrors generate_input_lists.mjs's emoji allow-file).
//
// Vector generator for the byte-exact Rust port of pi's TUI Editor. Runs pi's
// OWN Editor + exported wordWrapLine from
// vendor/pi/packages/tui/src/components/editor.ts (Node 22 strips TS types
// natively) and dumps input scripts -> {getText, getCursor, render, expanded,
// lines, isShowingAutocomplete, onSubmit} that the Rust test suite asserts
// byte-identical.
//
// Coverage of pi's test/editor.test.ts:
//   * editor_scenarios.json / editor_wordwrap.json (C6a) — every describe block
//     except the async Autocomplete block; the ~14 wordWrapLine cases call pi's
//     exported wordWrapLine directly.
//   * editor_autocomplete.json (C6b) — the Autocomplete block + the Undo block's
//     "undoes autocomplete" case, driven through the two-phase flush-seam machine
//     with providers recorded as (text, cursor, force) -> suggestions tables and
//     pi's own flushAutocomplete() called at every assert point. The one
//     wall-clock "aborts active @ autocomplete" case is a Rust behavioral unit
//     test (see tests/editor_vectors.rs) rather than a byte vector.
//
// Run from this directory:  node generate_editor.mjs
// Output is written to ../../tests/vectors/*.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a.

import { Editor, wordWrapLine } from "../../../../vendor/pi/packages/tui/src/components/editor.ts";
import { CombinedAutocompleteProvider } from "../../../../vendor/pi/packages/tui/src/autocomplete.ts";
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

// ===========================================================================
// Autocomplete (async): the two-phase flush-seam machine.
//
// Drives pi's OWN Editor through the Autocomplete describe block (and the
// Undo block's "undoes autocomplete" case), recording each provider's
// (text, cursor, force) -> suggestions and (text, cursor, item, prefix) ->
// applied responses AND calling pi's own flushAutocomplete() at every assert
// point. The Rust replay injects the recorded provider table and calls
// flush_autocomplete() at the same points, asserting byte-identical output.
//
// The abort-count case (a genuinely wall-clock test of superseding an
// in-flight request) is a Rust behavioral unit test, not a byte vector.
// ===========================================================================

// pi's test helper: drain the microtask + task queue so a settled autocomplete
// state is observable (mirrors test/editor.test.ts).
async function flushAutocomplete() {
	await Promise.resolve();
	await new Promise((resolve) => setImmediate(resolve));
}

// The standard applyCompletion used by every mock provider in the test file:
// replace the prefix before the cursor with item.value.
function applyCompletion(lines, cursorLine, cursorCol, item, prefix) {
	const line = lines[cursorLine] || "";
	const before = line.slice(0, cursorCol - prefix.length);
	const after = line.slice(cursorCol);
	const newLines = [...lines];
	newLines[cursorLine] = before + item.value + after;
	return { lines: newLines, cursorLine, cursorCol: cursorCol - prefix.length + item.value.length };
}

function normItem(it) {
	const o = { value: it.value, label: it.label };
	if (it.description !== undefined) o.description = it.description;
	return o;
}

// Wrap a provider so every getSuggestions / applyCompletion /
// shouldTriggerFileCompletion call is recorded as a lookup-table row.
function recordingProvider(base, rec) {
	const wrapped = {
		triggerCharacters: base.triggerCharacters,
		getSuggestions: async (lines, cl, cc, options) => {
			rec.calls += 1;
			const res = await base.getSuggestions(lines, cl, cc, options);
			rec.suggestions.push({
				text: lines.join("\n"),
				line: cl,
				col: cc,
				force: !!(options && options.force),
				result: res ? { items: res.items.map(normItem), prefix: res.prefix } : null,
			});
			return res;
		},
		applyCompletion: (lines, cl, cc, item, prefix) => {
			const res = base.applyCompletion(lines, cl, cc, item, prefix);
			rec.applies.push({
				text: lines.join("\n"),
				line: cl,
				col: cc,
				itemValue: item.value,
				prefix,
				result: { text: res.lines.join("\n"), line: res.cursorLine, col: res.cursorCol },
			});
			return res;
		},
	};
	if (base.shouldTriggerFileCompletion) {
		wrapped.shouldTriggerFileCompletion = (lines, cl, cc) => {
			const res = base.shouldTriggerFileCompletion(lines, cl, cc);
			rec.shouldTrigger.push({ text: lines.join("\n"), line: cl, col: cc, result: res });
			return res;
		};
	}
	return wrapped;
}

const autoFlush = (waitMs = 0) => ({ op: "flush", waitMs });

async function runAutoEditor(spec) {
	const editor = new Editor(makeTui(spec.rows ?? 24), editorTheme, spec.options ?? {});
	editor.disableSubmit = spec.disableSubmit ?? false;
	const submits = [];
	editor.onSubmit = (t) => submits.push(t);

	const rec = { suggestions: [], applies: [], shouldTrigger: [], calls: 0 };
	editor.setAutocompleteProvider(recordingProvider(spec.provider, rec));

	const trace = [];
	for (const step of spec.steps) {
		let renderLines = null;
		switch (step.op) {
			case "input":
				editor.handleInput(step.data);
				break;
			case "setText":
				editor.setText(step.text);
				break;
			case "flush":
				if (step.waitMs) await new Promise((resolve) => setTimeout(resolve, step.waitMs));
				await flushAutocomplete();
				break;
			case "render":
				renderLines = editor.render(step.width);
				break;
			default:
				throw new Error(`unknown auto op: ${step.op}`);
		}
		const cur = editor.getCursor();
		trace.push({
			...step,
			textAfter: editor.getText(),
			line: cur.line,
			col: cur.col,
			showing: editor.isShowingAutocomplete(),
			render: renderLines,
		});
	}

	return {
		name: spec.name,
		rows: spec.rows ?? 24,
		options: spec.options ?? {},
		disableSubmit: spec.disableSubmit ?? false,
		triggerCharacters: spec.provider.triggerCharacters ?? [],
		steps: trace,
		submits,
		suggestions: rec.suggestions,
		applies: rec.applies,
		shouldTrigger: rec.shouldTrigger,
		suggestionCallCount: rec.calls,
	};
}

const autoScenarios = [];
async function addAuto(name, provider, steps, extra = {}) {
	autoScenarios.push(await runAutoEditor({ name, provider, steps, ...extra }));
}

const typeChars = (s) => [...s].map(inp);
const cwd = process.cwd();

// --- Undo block: "undoes autocomplete" ---
await addAuto(
	"undo: undoes autocomplete",
	{
		getSuggestions: async (lines, _cl, cc) => {
			const prefix = (lines[0] || "").slice(0, cc);
			if (prefix === "di") return { items: [{ value: "dist/", label: "dist/" }], prefix: "di" };
			return null;
		},
		applyCompletion,
	},
	[inp("d"), inp("i"), inp("\t"), autoFlush(), inp(UNDO)],
);

// --- Autocomplete block ---
await addAuto(
	"auto: auto-applies single force-file suggestion without showing menu",
	{
		getSuggestions: async (lines, _cl, cc, options) => {
			if (!options.force) return null;
			const prefix = (lines[0] || "").slice(0, cc);
			if (prefix === "Work") return { items: [{ value: "Workspace/", label: "Workspace/" }], prefix: "Work" };
			return null;
		},
		applyCompletion,
	},
	[...typeChars("Work"), inp("\t"), autoFlush(), inp(UNDO)],
);

await addAuto(
	"auto: shows menu when force-file has multiple suggestions",
	{
		getSuggestions: async (lines, _cl, cc, options) => {
			if (!options.force) return null;
			const prefix = (lines[0] || "").slice(0, cc);
			if (prefix === "src")
				return {
					items: [
						{ value: "src/", label: "src/" },
						{ value: "src.txt", label: "src.txt" },
					],
					prefix: "src",
				};
			return null;
		},
		applyCompletion,
	},
	[...typeChars("src"), inp("\t"), autoFlush(), inp("\t")],
);

await addAuto(
	"auto: keeps suggestions open when typing in force mode (Tab-triggered)",
	(() => {
		const allFiles = [
			{ value: "readme.md", label: "readme.md" },
			{ value: "package.json", label: "package.json" },
			{ value: "src/", label: "src/" },
			{ value: "dist/", label: "dist/" },
		];
		return {
			getSuggestions: async (lines, _cl, cc, options) => {
				const prefix = (lines[0] || "").slice(0, cc);
				const shouldMatch = options.force || prefix.includes("/") || prefix.startsWith(".");
				if (!shouldMatch) return null;
				const filtered = allFiles.filter((f) => f.value.toLowerCase().startsWith(prefix.toLowerCase()));
				return filtered.length > 0 ? { items: filtered, prefix } : null;
			},
			applyCompletion,
		};
	})(),
	[inp("\t"), autoFlush(), inp("r"), autoFlush(), inp("e"), autoFlush(), inp("\t")],
);

await addAuto(
	"auto: debounces @ autocomplete while typing",
	{
		getSuggestions: async (lines, _cl, cc) => {
			const text = (lines[0] || "").slice(0, cc);
			return { items: [{ value: "@main.ts", label: "main.ts" }], prefix: text };
		},
		applyCompletion,
	},
	[inp("@"), inp("m"), inp("a"), inp("i"), autoFlush(50)],
);

await addAuto(
	"auto: re-queries the picker when cursor moves back into the command name",
	{
		getSuggestions: async (lines, _cl, cc) => {
			const before = (lines[0] || "").slice(0, cc);
			if (!before.startsWith("/")) return null;
			if (before.includes(" "))
				return {
					items: [
						{ value: "repo", label: "repo" },
						{ value: "message", label: "message" },
						{ value: "help", label: "help" },
					],
					prefix: before.slice(before.indexOf(" ") + 1),
				};
			return { items: [{ value: "cmd", label: "cmd" }], prefix: before };
		},
		applyCompletion,
	},
	[
		...[..."/cmd "].flatMap((ch) => [inp(ch), autoFlush()]),
		render(80),
		inp(LEFT),
		autoFlush(),
		render(80),
	],
);

await addAuto(
	"auto: debounces # autocomplete while typing",
	{
		getSuggestions: async (lines, _cl, cc) => {
			const text = (lines[0] || "").slice(0, cc);
			return { items: [{ value: "#2983", label: "#2983" }], prefix: text };
		},
		applyCompletion,
	},
	[inp("#"), inp("2"), inp("9"), inp("8"), autoFlush(50)],
);

await addAuto(
	"auto: debounces custom triggerCharacters autocomplete while typing",
	{
		triggerCharacters: ["$"],
		getSuggestions: async (lines, _cl, cc) => {
			const prefix = (lines[0] || "").slice(0, cc);
			return { items: [{ value: "$skill-name", label: "skill-name" }], prefix };
		},
		applyCompletion,
	},
	[inp("$"), inp("s"), inp("k"), autoFlush(50)],
);

// "resets custom triggerCharacters when provider changes": the first ($-trigger)
// provider is replaced before any query, so its only effect — installing $ as a
// trigger — is immediately overwritten by the default-trigger provider. With no
// query in between, the observable collapses to driving the default-trigger
// provider alone (typing "$s" triggers nothing → 0 calls, menu hidden).
await addAuto(
	"auto: resets custom triggerCharacters when provider changes",
	{
		getSuggestions: async () => ({ items: [{ value: "$skill-name", label: "skill-name" }], prefix: "$" }),
		applyCompletion,
	},
	[inp("$"), inp("s"), autoFlush(50)],
);

await addAuto(
	"auto: hides autocomplete when backspacing slash command to empty",
	{
		getSuggestions: async (lines, _cl, cc) => {
			const prefix = (lines[0] || "").slice(0, cc);
			if (prefix.startsWith("/")) {
				const commands = [
					{ value: "/model", label: "model", description: "Change model" },
					{ value: "/help", label: "help", description: "Show help" },
				];
				const query = prefix.slice(1);
				const filtered = commands.filter((c) => c.value.startsWith(query));
				if (filtered.length > 0) return { items: filtered, prefix };
			}
			return null;
		},
		applyCompletion,
	},
	[inp("/"), autoFlush(), inp(BS), autoFlush()],
);

// The four /argtest argument-selection cases share the structure "type, flush,
// Enter" and differ only in the provider's item list / filtering.
const argtestProvider = (allArguments, filterByPrefix) => ({
	getSuggestions: async (lines, _cl, cc) => {
		const beforeCursor = (lines[0] || "").slice(0, cc);
		const m = beforeCursor.match(/^\/argtest\s+(\S+)$/);
		if (m) {
			const argumentText = m[1];
			const items = filterByPrefix
				? allArguments.filter((arg) => arg.value.startsWith(argumentText))
				: allArguments;
			if (items.length > 0) return { items, prefix: argumentText };
		}
		return null;
	},
	applyCompletion,
});

await addAuto(
	"auto: applies exact typed slash-argument value on Enter",
	argtestProvider(
		[
			{ value: "one", label: "one" },
			{ value: "two", label: "two" },
			{ value: "three", label: "three" },
		],
		true,
	),
	[...typeChars("/argtest two"), autoFlush(), inp("\r")],
);

await addAuto(
	"auto: selects first prefix match on Enter when typed arg is not exact",
	argtestProvider(
		[
			{ value: "two", label: "two" },
			{ value: "three", label: "three" },
			{ value: "twelve", label: "twelve" },
		],
		true,
	),
	[...typeChars("/argtest t"), autoFlush(), inp("\r")],
);

await addAuto(
	"auto: highlights unique prefix match as user types",
	argtestProvider(
		[
			{ value: "one", label: "one" },
			{ value: "two", label: "two" },
			{ value: "three", label: "three" },
		],
		false,
	),
	[...typeChars("/argtest tw"), autoFlush(), inp("\r")],
);

await addAuto(
	"auto: selects first prefix match when multiple items match",
	argtestProvider(
		[
			{ value: "one", label: "one" },
			{ value: "two", label: "two" },
			{ value: "three", label: "three" },
		],
		false,
	),
	[...typeChars("/argtest t"), autoFlush(), inp("\r")],
);

await addAuto(
	"auto: built-in-style command argument completion path (model-like)",
	{
		getSuggestions: async (lines, _cl, cc) => {
			const beforeCursor = (lines[0] || "").slice(0, cc);
			const m = beforeCursor.match(/^\/model\s+(\S+)$/);
			if (m) {
				const modelText = m[1];
				const allModels = [
					{ value: "gpt-4o", label: "gpt-4o" },
					{ value: "gpt-4o-mini", label: "gpt-4o-mini" },
					{ value: "claude-sonnet", label: "claude-sonnet" },
				];
				const filtered = allModels.filter((mm) => mm.value.startsWith(modelText));
				if (filtered.length > 0) return { items: filtered, prefix: modelText };
			}
			return null;
		},
		applyCompletion,
	},
	[...typeChars("/model gpt-4o-mini"), autoFlush(), inp("\r")],
);

await addAuto(
	"auto: awaits async slash command argument completions",
	new CombinedAutocompleteProvider(
		[
			{
				name: "load-skills",
				description: "Load skills",
				getArgumentCompletions: async (prefix) => (prefix.startsWith("s") ? [{ value: "skill-a", label: "skill-a" }] : null),
			},
		],
		cwd,
	),
	[setText("/load-skills "), inp("s"), autoFlush(), inp("\t")],
);

await addAuto(
	"auto: ignores invalid slash command argument completion results",
	new CombinedAutocompleteProvider(
		[
			{
				name: "load-skills",
				description: "Load skills",
				getArgumentCompletions: () => "not-an-array",
			},
		],
		cwd,
	),
	[setText("/load-skills "), inp("s"), autoFlush()],
);

await addAuto(
	"auto: does not show argument completions when command has no argument completer",
	new CombinedAutocompleteProvider(
		[
			{ name: "help", description: "Show help" },
			{ name: "model", description: "Switch model", getArgumentCompletions: () => [{ value: "claude-opus", label: "claude-opus" }] },
		],
		cwd,
	),
	[inp("/"), inp("h"), inp("e"), autoFlush(), inp("\t")],
);

dump("editor_autocomplete", autoScenarios);

console.log(`\ntotal editor vectors: ${total}`);
