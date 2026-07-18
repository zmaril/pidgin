// straitjacket-allow-file:duplication — this generator's dump()/paths boilerplate
// intentionally mirrors generate_keys.mjs; each generator is a standalone script.
// straitjacket-allow-file:emoji — the emoji literal is UTF-8 segmentation/width
// test data fed to pi's own functions, not decorative prose (mirrors
// generate_keys.mjs's emoji allow-file).
//
// Vector generator for the bit-exact Rust port of pi's TUI component support
// layer (PR C1). Runs pi's OWN exported functions from
// vendor/pi/packages/tui/src/{fuzzy,word-navigation,keybindings,kill-ring,
// undo-stack}.ts (Node 22 strips TS types natively) plus the word segmenter and
// applyBackgroundToLine helpers from utils.ts, and dumps input -> expected
// JSON that the Rust test suite asserts byte-identical.
//
// Run from this directory:  node generate_components.mjs
// Output is written to ../../tests/vectors/*.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).

import { fuzzyFilter, fuzzyMatch } from "../../../../vendor/pi/packages/tui/src/fuzzy.ts";
import { findWordBackward, findWordForward } from "../../../../vendor/pi/packages/tui/src/word-navigation.ts";
import { KeybindingsManager, TUI_KEYBINDINGS } from "../../../../vendor/pi/packages/tui/src/keybindings.ts";
import { KillRing } from "../../../../vendor/pi/packages/tui/src/kill-ring.ts";
import { UndoStack } from "../../../../vendor/pi/packages/tui/src/undo-stack.ts";
import {
	applyBackgroundToLine,
	getWordSegmenter,
	isPunctuationChar,
	isWhitespaceChar,
} from "../../../../vendor/pi/packages/tui/src/utils.ts";
import { writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

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

// ---------------------------------------------------------------------------
// fuzzy
// ---------------------------------------------------------------------------

// (query, text) pairs: every fuzzy.test.ts case plus a broad sweep that
// exercises consecutive/gap/word-boundary/exact/swapped-alnum scoring paths.
const fuzzyPairs = [
	// straight from fuzzy.test.ts
	["", "anything"],
	["longquery", "short"],
	["test", "test"],
	["abc", "aXbXc"],
	["abc", "cba"],
	["ABC", "abc"],
	["abc", "ABC"],
	["foo", "foobar"],
	["foo", "f_o_o_bar"],
	["fb", "foo-bar"],
	["fb", "afbx"],
	["codex52", "gpt-5.2-codex"],
	// fuzzyFilter cases (per-token)
	["an", "apple"],
	["an", "banana"],
	["an", "cherry"],
	["app", "a_p_p"],
	["app", "app"],
	["app", "application"],
	["cl", "clone"],
	["cl", "cl"],
	["foo", "foo"],
	["foo", "bar"],
	["foo", "foobar"],
	// scoring-path sweep
	["a", "abc"],
	["c", "abc"],
	["ab", "a.b"],
	["ab", "a-b"],
	["ab", "a_b"],
	["ab", "a/b"],
	["ab", "a:b"],
	["ab", "axb"],
	["ac", "abc"],
	["xyz", "xyz"],
	["xyz", "x-y-z"],
	["gpt5", "gpt-5"],
	["5gpt", "gpt-5"],
	["52codex", "gpt-5.2-codex"],
	["abc123", "abc-123"],
	["123abc", "abc-123"],
	["o1", "gpt-4o"],
	["k", "key"],
	["key", "keybinding"],
	["kb", "keybinding"],
	["hello", "hello world"],
	["world", "hello world"],
	["hw", "hello world"],
	["中文", "中文测试"],
	["ab", "ＡＢ"],
];
// Emit the exact IEEE-754 bit pattern of the score alongside the readable
// value: JS serializes floats to the shortest round-tripping decimal, but that
// decimal can parse back (in a different engine's float reader) to an adjacent
// f64. Comparing raw bits makes the score assertion truly byte-exact.
function f64Bits(x) {
	const buf = new ArrayBuffer(8);
	new DataView(buf).setFloat64(0, x);
	return new DataView(buf).getBigUint64(0).toString();
}
const fuzzyMatchVectors = fuzzyPairs.map(([query, text]) => {
	const r = fuzzyMatch(query, text);
	return { query, text, matches: r.matches, score: r.score, bits: f64Bits(r.score) };
});
dump("fuzzy_match", fuzzyMatchVectors);

// fuzzyFilter: (items, query) -> ordered result. Every fuzzy.test.ts filter
// case (string items) plus extra ordering sweeps.
const fuzzyFilterCases = [
	{ items: ["apple", "banana", "cherry"], query: "" },
	{ items: ["apple", "banana", "cherry"], query: "an" },
	{ items: ["a_p_p", "app", "application"], query: "app" },
	{ items: ["clone", "cl"], query: "cl" },
	{ items: ["foo", "bar", "foobar"], query: "foo" },
	{ items: ["gpt-5.5 openai-codex"], query: "openai-codex/gpt-5.5" },
	{ items: ["apple", "banana", "cherry"], query: "   " },
	{ items: ["one", "two", "three"], query: "e" },
	{ items: ["cat", "car", "cart", "card"], query: "ca" },
	{ items: ["gpt-4o", "gpt-4.1", "gpt-5", "gpt-5-codex", "o1", "o3"], query: "gpt5" },
	{ items: ["file.ts", "file.test.ts", "index.ts"], query: "test" },
	{ items: ["a b c", "abc", "a-b-c"], query: "abc" },
	{ items: ["read file", "write file", "read line"], query: "read file" },
	{ items: ["provider/model", "other/thing"], query: "provider model" },
];
const fuzzyFilterVectors = fuzzyFilterCases.map(({ items, query }) => ({
	items,
	query,
	result: fuzzyFilter(items, query, (x) => x),
}));
dump("fuzzy_filter", fuzzyFilterVectors);

// ---------------------------------------------------------------------------
// word segmentation (Intl.Segmenter word granularity + isWordLike)
// ---------------------------------------------------------------------------

const wordSegmenter = getWordSegmenter();
const segCorpus = [
	"hello world",
	"foo.bar",
	"foo:bar",
	"foo;bar",
	"path/to/file",
	"path/to",
	"/to/file",
	"你好世界 test",
	"你好世界",
	"  hello  ",
	"foo...bar",
	"hello",
	"[paste #1 +5 lines]",
	"gpt-5.2-codex",
	"a_p_p",
	"openai-codex",
	"foo-bar",
	"a:b:c",
	"ab:cd:ef",
	"你:好",
	"1:2",
	"12:30",
	"a::b",
	"C:\\Users",
	"http://x",
	"snake_case",
	"kebab-case",
	"CamelCase",
	"file.tar.gz",
	"user@host",
	"don't",
	"café",
	"naïve",
	"2024-01-02",
	"v1.2.3",
	"emoji 🙂 x",
	"",
	" ",
	"   ",
	"a b\tc",
	"foo!bar",
	"foo?bar",
	"word:",
	":word",
	"αβ:γδ",
];
const wordSegVectors = segCorpus.map((text) => ({
	text,
	segments: [...wordSegmenter.segment(text)].map((s) => ({ segment: s.segment, isWordLike: s.isWordLike })),
}));
dump("word_segmentation", wordSegVectors);

// classification helpers (isWhitespaceChar / isPunctuationChar)
const classifyCorpus = [
	" ",
	"  ",
	"\t",
	"\n",
	"a",
	"ab",
	" a",
	"a ",
	".",
	":",
	"foo",
	"foo.",
	"!",
	"",
	"你",
	"1",
	"_",
	"-",
	"/",
	" ",
	"　",
	"﻿",
	"a.b",
	"   x   ",
];
const classifyVectors = classifyCorpus.map((s) => ({
	text: s,
	isWhitespace: isWhitespaceChar(s),
	isPunctuation: isPunctuationChar(s),
}));
dump("char_classification", classifyVectors);

// ---------------------------------------------------------------------------
// word navigation (default segmenter). Drive findWordBackward/Forward at every
// valid UTF-16 boundary for each corpus string, covering every describe case.
// ---------------------------------------------------------------------------

const navCorpus = [
	"hello world",
	"foo.bar",
	"foo:bar",
	"path/to/file",
	"你好世界 test",
	"  hello  ",
	"foo...bar",
	"hello",
	"a:b:c",
	"ab:cd:ef",
	"你:好",
	"foo;bar",
	"snake_case",
	"kebab-case",
	"a_p_p world",
	"one two three",
	"  leading",
	"trailing  ",
	"gpt-5.2-codex model",
	"user@host.com",
	"a.b.c.d",
	"  ",
	"",
	"café résumé",
];

// A cursor index is "valid" when slicing there does not split a UTF-16
// surrogate pair (pi's slice would produce a lone surrogate otherwise; the
// nav corpus is BMP-only, so every index is valid, but we guard anyway).
function validCursors(text) {
	const units = [];
	for (let i = 0; i < text.length; i++) units.push(text.charCodeAt(i));
	const out = [];
	for (let c = 0; c <= text.length; c++) {
		// invalid if the unit just before c is a high surrogate (c splits a pair)
		if (c > 0 && c < text.length) {
			const prev = units[c - 1];
			if (prev >= 0xd800 && prev <= 0xdbff) continue;
		}
		out.push(c);
	}
	return out;
}

const navVectors = [];
for (const text of navCorpus) {
	for (const cursor of validCursors(text)) {
		navVectors.push({
			text,
			cursor,
			backward: findWordBackward(text, cursor),
			forward: findWordForward(text, cursor),
		});
	}
}
dump("word_navigation", navVectors);

// ---------------------------------------------------------------------------
// kill ring
// ---------------------------------------------------------------------------

// Each scenario is a sequence of ops applied to a fresh KillRing; we record the
// observable state (peek + length) after every op.
const killRingScenarios = [
	{
		name: "basic push + peek",
		ops: [
			{ op: "push", text: "hello", prepend: false, accumulate: false },
			{ op: "push", text: "world", prepend: false, accumulate: false },
			{ op: "peek" },
		],
	},
	{
		name: "empty text ignored",
		ops: [
			{ op: "push", text: "", prepend: false, accumulate: false },
			{ op: "push", text: "x", prepend: false, accumulate: false },
			{ op: "push", text: "", prepend: true, accumulate: true },
		],
	},
	{
		name: "accumulate append (forward delete)",
		ops: [
			{ op: "push", text: "foo", prepend: false, accumulate: false },
			{ op: "push", text: "bar", prepend: false, accumulate: true },
			{ op: "push", text: "baz", prepend: false, accumulate: true },
		],
	},
	{
		name: "accumulate prepend (backward delete)",
		ops: [
			{ op: "push", text: "foo", prepend: false, accumulate: false },
			{ op: "push", text: "bar", prepend: true, accumulate: true },
			{ op: "push", text: "baz", prepend: true, accumulate: true },
		],
	},
	{
		name: "accumulate on empty ring creates entry",
		ops: [{ op: "push", text: "solo", prepend: true, accumulate: true }],
	},
	{
		name: "rotate cycles entries",
		ops: [
			{ op: "push", text: "a", prepend: false, accumulate: false },
			{ op: "push", text: "b", prepend: false, accumulate: false },
			{ op: "push", text: "c", prepend: false, accumulate: false },
			{ op: "rotate" },
			{ op: "peek" },
			{ op: "rotate" },
			{ op: "peek" },
		],
	},
	{
		name: "rotate single entry no-op",
		ops: [
			{ op: "push", text: "only", prepend: false, accumulate: false },
			{ op: "rotate" },
			{ op: "peek" },
		],
	},
	{
		name: "rotate empty no-op",
		ops: [{ op: "rotate" }, { op: "peek" }],
	},
	{
		name: "no accumulate makes new entries",
		ops: [
			{ op: "push", text: "x", prepend: false, accumulate: false },
			{ op: "push", text: "y", prepend: true, accumulate: false },
		],
	},
];
const killRingVectors = killRingScenarios.map(({ name, ops }) => {
	const ring = new KillRing();
	const states = [];
	for (const op of ops) {
		if (op.op === "push") {
			ring.push(op.text, { prepend: op.prepend, accumulate: op.accumulate });
		} else if (op.op === "rotate") {
			ring.rotate();
		}
		const peek = ring.peek();
		states.push({ peek: peek === undefined ? null : peek, length: ring.length });
	}
	return { name, ops, states };
});
dump("kill_ring", killRingVectors);

// ---------------------------------------------------------------------------
// undo stack (string snapshots — the observable clone-on-push / pop semantics)
// ---------------------------------------------------------------------------

const undoScenarios = [
	{
		name: "push/pop LIFO",
		ops: [
			{ op: "push", value: "a" },
			{ op: "push", value: "b" },
			{ op: "push", value: "c" },
			{ op: "pop" },
			{ op: "pop" },
			{ op: "pop" },
			{ op: "pop" },
		],
	},
	{
		name: "clear empties",
		ops: [
			{ op: "push", value: "x" },
			{ op: "push", value: "y" },
			{ op: "clear" },
			{ op: "pop" },
		],
	},
	{
		name: "pop empty returns undefined",
		ops: [{ op: "pop" }],
	},
	{
		name: "interleaved",
		ops: [
			{ op: "push", value: "1" },
			{ op: "pop" },
			{ op: "push", value: "2" },
			{ op: "push", value: "3" },
			{ op: "pop" },
			{ op: "push", value: "4" },
		],
	},
];
const undoVectors = undoScenarios.map(({ name, ops }) => {
	const stack = new UndoStack();
	const states = [];
	for (const op of ops) {
		let popped = undefined;
		if (op.op === "push") stack.push(op.value);
		else if (op.op === "pop") popped = stack.pop();
		else if (op.op === "clear") stack.clear();
		states.push({
			op: op.op,
			popped: popped === undefined ? null : popped,
			length: stack.length,
		});
	}
	return { name, ops, states };
});
dump("undo_stack", undoVectors);

// ---------------------------------------------------------------------------
// applyBackgroundToLine (deterministic bg fn: wrap in SGR 41/49)
// ---------------------------------------------------------------------------

const bgFn = (t) => `\x1b[41m${t}\x1b[49m`;
const bgCases = [
	{ line: "", width: 0 },
	{ line: "", width: 5 },
	{ line: "abc", width: 5 },
	{ line: "abc", width: 3 },
	{ line: "abc", width: 2 },
	{ line: "hello", width: 10 },
	{ line: "\x1b[1mbold\x1b[0m", width: 8 },
	{ line: "你好", width: 6 },
	{ line: "tab\there", width: 12 },
	{ line: "🙂x", width: 6 },
];
const bgVectors = bgCases.map(({ line, width }) => ({
	line,
	width,
	result: applyBackgroundToLine(line, width, bgFn),
}));
dump("apply_background", bgVectors);

// ---------------------------------------------------------------------------
// keybindings
// ---------------------------------------------------------------------------

const bindingIds = Object.keys(TUI_KEYBINDINGS);

// Data corpus for matches(): representative sequences hitting many bindings.
const kbDataCorpus = [
	"\n", // linefeed
	"\r", // enter
	"\t", // tab
	"\x1b[106;5u", // ctrl+j (kitty)
	"\x03", // ctrl+c
	"\x1b", // escape
	"\x1b[A", // up
	"\x1b[B", // down
	"\x1b[C", // right
	"\x1b[D", // left
	"\x7f", // backspace
	"\x04", // ctrl+d
	"\x15", // ctrl+u
	"\x0b", // ctrl+k
	"\x19", // ctrl+y
	"\x17", // ctrl+w
	"\x01", // ctrl+a
	"\x05", // ctrl+e
	"\x02", // ctrl+b
	"\x06", // ctrl+f
	"\x1b[5~", // pageUp
	"\x1b[6~", // pageDown
	"\x1b[3~", // delete
	"\x1bb", // alt+b
	"\x1bf", // alt+f
	"\x1bd", // alt+d
	"\x1by", // alt+y
	"x", // plain char
];

function scenarioFor(name, userBindings) {
	const mgr =
		userBindings === undefined
			? new KeybindingsManager(TUI_KEYBINDINGS)
			: new KeybindingsManager(TUI_KEYBINDINGS, userBindings);
	const getKeys = {};
	for (const id of bindingIds) getKeys[id] = mgr.getKeys(id);
	const resolved = mgr.getResolvedBindings();
	const conflicts = mgr.getConflicts();
	const matches = [];
	for (const data of kbDataCorpus) {
		for (const id of bindingIds) {
			matches.push({ data, binding: id, expected: mgr.matches(data, id) });
		}
	}
	// Serialize userBindings as an ordered array of [id, keysOrNull].
	const userBindingsArr =
		userBindings === undefined
			? null
			: Object.entries(userBindings).map(([id, keys]) => [id, keys === undefined ? null : keys]);
	return { name, userBindings: userBindingsArr, getKeys, resolved, conflicts, matches };
}

const kbScenarios = [
	scenarioFor("default", undefined),
	scenarioFor("submit-rebound", { "tui.input.submit": ["enter", "ctrl+enter"] }),
	scenarioFor("select-up-rebound", { "tui.select.up": ["up", "ctrl+p"] }),
	scenarioFor("direct-conflict", {
		"tui.input.submit": "ctrl+x",
		"tui.select.confirm": "ctrl+x",
	}),
	scenarioFor("unknown-binding-ignored", {
		"tui.does.not.exist": "ctrl+z",
		"tui.editor.undo": ["ctrl+-", "ctrl+-"],
	}),
];
dump("keybindings", kbScenarios);

console.log(`\nTOTAL COMPONENT VECTORS: ${total}`);
