// straitjacket-allow-file:duplication — this generator's dump()/paths boilerplate
// intentionally mirrors generate_widgets.mjs / generate_components.mjs; each
// generator is a standalone script.
// straitjacket-allow-file:emoji — the CJK/emoji string literals are UTF-8
// width/segmentation test data fed to pi's own components, not decorative prose
// (mirrors generate_widgets.mjs's emoji allow-file).
//
// Vector generator for the byte-exact Rust port of pi's TUI interactive input
// widgets (PR C3: Input, SelectList, SettingsList). Runs pi's OWN component
// classes from vendor/pi/packages/tui/src/components/*.ts (Node 22 strips TS
// types natively) and dumps input scripts / props -> {value, cursor-derived
// state, rendered lines} that the Rust test suite asserts byte-identical.
//
// The Input and SelectList scenarios replay the exact cases from pi's
// test/input.test.ts and test/select-list.test.ts, so the Rust vector tests
// prove those two suites can be flipped to the native port. SettingsList has no
// pi test file, so a representative render/handle-input corpus is generated
// (its submenu delegation is covered by Rust unit tests, since it is pure
// delegation to already-vectored components).
//
// Run from this directory:  node generate_input_lists.mjs
// Output is written to ../../tests/vectors/*.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).

import { Input } from "../../../../vendor/pi/packages/tui/src/components/input.ts";
import { SelectList } from "../../../../vendor/pi/packages/tui/src/components/select-list.ts";
import { SettingsList } from "../../../../vendor/pi/packages/tui/src/components/settings-list.ts";
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

// ===========================================================================
// Input
// ===========================================================================
//
// Each scenario is a list of steps. A step is one of:
//   { op: "input", data }        -> input.handleInput(data)
//   { op: "setValue", value }    -> input.setValue(value)
//   { op: "focus", value }       -> input.focused = value
//   { op: "render", width }      -> input.render(width) (also captured)
// After each step the resulting getValue() is recorded; render steps also
// record the rendered lines. The value passed to the last onSubmit call and the
// onEscape call count are recorded per scenario.

// Steps use distinct arg field names (data / setValue / focused / width) so the
// recorded outcome (valueAfter / render) never collides with a step argument.
function runInput(steps) {
	const input = new Input();
	let submitted = null;
	let escaped = 0;
	input.onSubmit = (v) => {
		submitted = v;
	};
	input.onEscape = () => {
		escaped++;
	};
	const trace = [];
	for (const step of steps) {
		let render = null;
		switch (step.op) {
			case "input":
				input.handleInput(step.data);
				break;
			case "setValue":
				input.setValue(step.setValue);
				break;
			case "focus":
				input.focused = step.focused;
				break;
			case "render":
				render = input.render(step.width);
				break;
			default:
				throw new Error(`unknown input op: ${step.op}`);
		}
		trace.push({ ...step, valueAfter: input.getValue(), render });
	}
	return { steps: trace, submitted, escaped };
}

// Convenience builders.
const inp = (data) => ({ op: "input", data });
const setVal = (value) => ({ op: "setValue", setValue: value });
const focus = (value) => ({ op: "focus", focused: value });
const render = (width) => ({ op: "render", width });
const typeChars = (s) => [...s].map(inp);

function moveRight(n) {
	return Array.from({ length: n }, () => inp("\x1b[C"));
}

const CTRL_A = "\x01";
const CTRL_E = "\x05";
const CTRL_W = "\x17";
const CTRL_Y = "\x19";
const CTRL_K = "\x0b";
const CTRL_U = "\x15";
const ALT_Y = "\x1by";
const ALT_D = "\x1bd";
const UNDO = "\x1b[45;5u";
const BACKSPACE = "\x7f";
const DELETE = "\x1b[3~";

{
	const scenarios = [];
	const add = (name, steps) => scenarios.push({ name, ...runInput(steps) });

	// --- submits / backslash ---
	add("submits value including backslash on Enter", [
		...typeChars("hello"),
		inp("\\"),
		inp("\r"),
	]);
	add("inserts backslash as regular character", [inp("\\"), inp("x")]);

	// --- render: CJK / fullwidth overflow (width 93) ---
	const cjkCases = [
		"가나다라마바사아자차카타파하 한글 텍스트가 터미널 너비를 초과하면 크래시가 발생합니다 이것은 재현용 테스트입니다",
		"これはテスト文章です。日本語のテキストが正しく表示されるかどうかを確認するためのサンプルテキストです。あいうえお",
		"这是一段测试文本，用于验证中文字符在终端中的显示宽度是否被正确计算，如果不正确就会导致用户界面崩溃的问题",
		"ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ０１２３４５６７８９ａｂｃｄｅｆｇｈｉｊｋｌｍ",
	];
	const cursorPositions = [
		{ label: "start", move: [] },
		{ label: "middle", move: moveRight(10) },
		{ label: "end", move: [inp(CTRL_E)] },
	];
	for (let c = 0; c < cjkCases.length; c++) {
		for (const { label, move } of cursorPositions) {
			add(`render CJK case ${c} at ${label}`, [
				setVal(cjkCases[c]),
				focus(true),
				...move,
				render(93),
			]);
		}
	}
	// keeps the cursor visible when horizontally scrolling wide text (width 20)
	add("keeps cursor visible when scrolling wide text", [
		setVal("가나다라마바사아자차카타파하"),
		focus(true),
		inp(CTRL_A),
		...moveRight(5),
		render(20),
	]);

	// A few extra render cases across widths / focus states (not overflowing).
	add("render short focused", [setVal("hello"), focus(true), render(20)]);
	add("render short unfocused", [setVal("hello"), focus(false), render(20)]);
	add("render empty focused", [focus(true), render(10)]);
	add("render narrow width", [setVal("hello world"), focus(true), render(3)]);
	add("render width equals prompt", [setVal("x"), focus(true), render(2)]);
	add("render cursor mid emoji", [setVal("ab🙂cd"), focus(true), inp(CTRL_A), ...moveRight(3), render(20)]);

	// --- Kill ring ---
	add("Ctrl+W saves and Ctrl+Y yanks", [
		setVal("foo bar baz"),
		inp(CTRL_E),
		inp(CTRL_W),
		inp(CTRL_A),
		inp(CTRL_Y),
	]);
	add("Ctrl+W preserves ASCII punctuation boundaries", [
		setVal("foo.bar"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("foo:bar"),
		inp(CTRL_E),
		inp(CTRL_W),
	]);
	add("Ctrl+W handles Unicode word boundaries", [
		setVal("你好世界。你好，世界"),
		inp(CTRL_E),
		inp(CTRL_W),
		inp(CTRL_W),
		inp(CTRL_W),
		inp(CTRL_W),
		inp(CTRL_W),
		inp(CTRL_W),
	]);
	add("Ctrl+U saves deleted text", [
		setVal("hello world"),
		inp(CTRL_A),
		...moveRight(6),
		inp(CTRL_U),
		inp(CTRL_Y),
	]);
	add("Ctrl+K saves deleted text", [
		setVal("hello world"),
		inp(CTRL_A),
		inp(CTRL_K),
		inp(CTRL_Y),
	]);
	add("Ctrl+Y does nothing when kill ring empty", [setVal("test"), inp(CTRL_E), inp(CTRL_Y)]);
	add("Alt+Y cycles through kill ring after Ctrl+Y", [
		setVal("first"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("second"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("third"),
		inp(CTRL_E),
		inp(CTRL_W),
		inp(CTRL_Y),
		inp(ALT_Y),
		inp(ALT_Y),
		inp(ALT_Y),
	]);
	add("Alt+Y does nothing if not preceded by yank", [
		setVal("test"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("other"),
		inp(CTRL_E),
		inp("x"),
		inp(ALT_Y),
	]);
	add("Alt+Y does nothing if kill ring has one entry", [
		setVal("only"),
		inp(CTRL_E),
		inp(CTRL_W),
		inp(CTRL_Y),
		inp(ALT_Y),
	]);
	add("consecutive Ctrl+W accumulates", [
		setVal("one two three"),
		inp(CTRL_E),
		inp(CTRL_W),
		inp(CTRL_W),
		inp(CTRL_W),
		inp(CTRL_Y),
	]);
	add("non-delete actions break kill accumulation", [
		setVal("foo bar baz"),
		inp(CTRL_E),
		inp(CTRL_W),
		inp("x"),
		inp(CTRL_W),
		inp(CTRL_Y),
		inp(ALT_Y),
	]);
	add("non-yank actions break Alt+Y chain", [
		setVal("first"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("second"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal(""),
		inp(CTRL_Y),
		inp("x"),
		inp(ALT_Y),
	]);
	add("kill ring rotation persists after cycling", [
		setVal("first"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("second"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("third"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal(""),
		inp(CTRL_Y),
		inp(ALT_Y),
		inp("x"),
		setVal(""),
		inp(CTRL_Y),
	]);
	add("backward prepend, forward append during accumulation", [
		setVal("prefix|suffix"),
		inp(CTRL_A),
		...moveRight(6),
		inp(CTRL_K),
		inp(CTRL_Y),
	]);
	add("Alt+D deletes word forward and saves", [
		setVal("hello world test"),
		inp(CTRL_A),
		inp(ALT_D),
		inp(ALT_D),
		inp(CTRL_Y),
	]);
	add("Alt+D preserves ASCII punctuation boundaries", [
		setVal("foo.bar baz"),
		inp(CTRL_A),
		inp(ALT_D),
		inp(ALT_D),
		inp(ALT_D),
	]);
	add("Alt+D handles Unicode word boundaries", [
		setVal("你好世界。你好，世界"),
		inp(CTRL_A),
		inp(ALT_D),
		inp(ALT_D),
		inp(ALT_D),
		inp(ALT_D),
		inp(ALT_D),
		inp(ALT_D),
	]);
	add("handles yank in middle of text", [
		setVal("word"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("hello world"),
		inp(CTRL_A),
		...moveRight(6),
		inp(CTRL_Y),
	]);
	add("handles yank-pop in middle of text", [
		setVal("FIRST"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("SECOND"),
		inp(CTRL_E),
		inp(CTRL_W),
		setVal("hello world"),
		inp(CTRL_A),
		...moveRight(6),
		inp(CTRL_Y),
		inp(ALT_Y),
	]);

	// --- Undo ---
	add("undo does nothing when stack empty", [inp(UNDO)]);
	add("undo coalesces consecutive word characters", [
		...typeChars("hello world"),
		inp(UNDO),
		inp(UNDO),
	]);
	add("undo undoes spaces one at a time", [
		...typeChars("hello"),
		inp(" "),
		inp(" "),
		inp(UNDO),
		inp(UNDO),
		inp(UNDO),
	]);
	add("undo undoes backspace", [...typeChars("hello"), inp(BACKSPACE), inp(UNDO)]);
	add("undo undoes forward delete", [
		...typeChars("hello"),
		inp(CTRL_A),
		inp("\x1b[C"),
		inp(DELETE),
		inp(UNDO),
	]);
	add("undo undoes Ctrl+W", [...typeChars("hello world"), inp(CTRL_W), inp(UNDO)]);
	add("undo undoes Ctrl+K", [
		...typeChars("hello world"),
		inp(CTRL_A),
		...moveRight(6),
		inp(CTRL_K),
		inp(UNDO),
	]);
	add("undo undoes Ctrl+U", [
		...typeChars("hello world"),
		inp(CTRL_A),
		...moveRight(6),
		inp(CTRL_U),
		inp(UNDO),
	]);
	add("undo undoes yank", [
		...typeChars("hello"),
		inp(" "),
		inp(CTRL_W),
		inp(CTRL_Y),
		inp(UNDO),
	]);
	add("undo undoes paste atomically", [
		setVal("hello world"),
		inp(CTRL_A),
		...moveRight(5),
		inp("\x1b[200~beep boop\x1b[201~"),
		inp(UNDO),
	]);
	add("undo undoes Alt+D", [setVal("hello world"), inp(CTRL_A), inp(ALT_D), inp(UNDO)]);
	add("undo cursor movement starts new undo unit", [
		inp("a"),
		inp("b"),
		inp("c"),
		inp(CTRL_A),
		inp(CTRL_E),
		inp("d"),
		inp("e"),
		inp(UNDO),
		inp(UNDO),
	]);

	// --- extra editing / paste coverage ---
	add("cursor left/right grapheme by grapheme", [
		setVal("a🙂b"),
		inp("\x1b[C"),
		inp("\x1b[C"),
		inp("\x1b[D"),
		inp("\x1b[D"),
	]);
	add("word left/right navigation", [
		setVal("foo bar baz"),
		inp(CTRL_A),
		inp("\x1bf"),
		inp("\x1bf"),
		inp("\x1bb"),
	]);
	add("paste strips newlines and tabs", [
		inp("\x1b[200~line1\nline2\r\ntab\there\x1b[201~"),
	]);
	add("paste with trailing input after marker", [
		inp("\x1b[200~abc\x1b[201~xyz"),
	]);
	add("escape triggers onEscape", [setVal("hi"), inp("\x1b")]);
	add("submit via newline", [...typeChars("hi"), inp("\n")]);

	dump("input_scenarios", scenarios);
}

// ===========================================================================
// SelectList
// ===========================================================================

const selectTheme = {
	selectedPrefix: (t) => t,
	selectedText: (t) => t,
	description: (t) => t,
	scrollInfo: (t) => t,
	noMatch: (t) => t,
};

// truncatePrimary overrides selected by tag (mirrored on the Rust side).
function truncatePrimaryFor(tag) {
	switch (tag) {
		case undefined:
		case null:
			return undefined;
		case "ellipsis":
			return ({ text, maxWidth }) => {
				if (text.length <= maxWidth) {
					return text;
				}
				return `${text.slice(0, Math.max(0, maxWidth - 1))}…`;
			};
		default:
			throw new Error(`unknown truncatePrimary tag: ${tag}`);
	}
}

function buildLayout(layout) {
	if (!layout) return {};
	const built = {};
	if (layout.min !== undefined) built.minPrimaryColumnWidth = layout.min;
	if (layout.max !== undefined) built.maxPrimaryColumnWidth = layout.max;
	const tp = truncatePrimaryFor(layout.truncateTag);
	if (tp) built.truncatePrimary = tp;
	return built;
}

function runSelectList(spec) {
	const list = new SelectList(spec.items, spec.maxVisible, selectTheme, buildLayout(spec.layout));
	const selections = [];
	const selected = [];
	let cancelled = 0;
	list.onSelectionChange = (item) => selections.push(item.value);
	list.onSelect = (item) => selected.push(item.value);
	list.onCancel = () => {
		cancelled++;
	};

	const trace = [];
	for (const step of spec.steps) {
		let renderLines = null;
		switch (step.op) {
			case "render":
				renderLines = list.render(step.width);
				break;
			case "input":
				list.handleInput(step.data);
				break;
			case "setFilter":
				list.setFilter(step.filter);
				break;
			case "setSelectedIndex":
				list.setSelectedIndex(step.index);
				break;
			default:
				throw new Error(`unknown select-list op: ${step.op}`);
		}
		const sel = list.getSelectedItem();
		trace.push({ ...step, selectedItem: sel ? sel.value : null, render: renderLines });
	}
	return { ...spec, steps: trace, selections, selected, cancelled };
}

{
	const scenarios = [];
	const add = (spec) => scenarios.push(runSelectList(spec));

	// --- the five select-list.test.ts render cases ---
	add({
		name: "normalizes multiline descriptions to single line",
		items: [{ value: "test", label: "test", description: "Line one\nLine two\nLine three" }],
		maxVisible: 5,
		layout: null,
		steps: [render(100)],
	});
	add({
		name: "keeps descriptions aligned when primary text truncated",
		items: [
			{ value: "short", label: "short", description: "short description" },
			{
				value: "very-long-command-name-that-needs-truncation",
				label: "very-long-command-name-that-needs-truncation",
				description: "long description",
			},
		],
		maxVisible: 5,
		layout: null,
		steps: [render(80)],
	});
	add({
		name: "uses configured minimum primary column width",
		items: [
			{ value: "a", label: "a", description: "first" },
			{ value: "bb", label: "bb", description: "second" },
		],
		maxVisible: 5,
		layout: { min: 12, max: 20 },
		steps: [render(80)],
	});
	add({
		name: "uses configured maximum primary column width",
		items: [
			{
				value: "very-long-command-name-that-needs-truncation",
				label: "very-long-command-name-that-needs-truncation",
				description: "first",
			},
			{ value: "short", label: "short", description: "second" },
		],
		maxVisible: 5,
		layout: { min: 12, max: 20 },
		steps: [render(80)],
	});
	add({
		name: "allows overriding primary truncation while preserving alignment",
		items: [
			{
				value: "very-long-command-name-that-needs-truncation",
				label: "very-long-command-name-that-needs-truncation",
				description: "first",
			},
			{ value: "short", label: "short", description: "second" },
		],
		maxVisible: 5,
		layout: { min: 12, max: 12, truncateTag: "ellipsis" },
		steps: [render(80)],
	});

	// --- extra render coverage: no items, scrolling, narrow widths, no desc ---
	const many = Array.from({ length: 8 }, (_, i) => ({
		value: `cmd${i}`,
		label: `command-${i}`,
		description: `description number ${i}`,
	}));
	add({ name: "empty list shows no-match", items: [], maxVisible: 5, layout: null, steps: [render(40)] });
	add({
		name: "scrolling many items renders indicator",
		items: many,
		maxVisible: 3,
		layout: null,
		steps: [render(60), inp("\x1b[B"), render(60), inp("\x1b[B"), inp("\x1b[B"), inp("\x1b[B"), render(60)],
	});
	add({
		name: "narrow width without description column",
		items: [
			{ value: "alpha", label: "alpha", description: "should be dropped at narrow width" },
			{ value: "beta", label: "beta", description: "also dropped" },
		],
		maxVisible: 5,
		layout: null,
		steps: [render(30)],
	});
	add({
		name: "items without descriptions",
		items: [
			{ value: "one", label: "one" },
			{ value: "two", label: "two" },
			{ value: "three", label: "" },
		],
		maxVisible: 5,
		layout: null,
		steps: [render(40)],
	});
	add({
		name: "CJK labels and descriptions",
		items: [
			{ value: "jp", label: "日本語コマンド", description: "これは説明文です" },
			{ value: "cn", label: "中文命令", description: "这是描述" },
		],
		maxVisible: 5,
		layout: null,
		steps: [render(60), render(40)],
	});

	// --- navigation / callbacks (handle_input) ---
	add({
		name: "navigation wraps and confirms",
		items: many.slice(0, 4),
		maxVisible: 5,
		layout: null,
		steps: [
			inp("\x1b[A"), // up wraps to last
			inp("\x1b[B"), // down wraps to first
			inp("\x1b[B"), // down to 1
			inp("\r"), // confirm index 1
			inp("\x1b"), // cancel
		],
	});
	add({
		name: "setFilter narrows and resets selection",
		items: [
			{ value: "apple", label: "apple" },
			{ value: "apricot", label: "apricot" },
			{ value: "banana", label: "banana" },
		],
		maxVisible: 5,
		layout: null,
		steps: [{ op: "setSelectedIndex", index: 2 }, render(40), { op: "setFilter", filter: "ap" }, render(40)],
	});

	dump("select_list_scenarios", scenarios);
}

// ===========================================================================
// SettingsList (representative corpus; no pi test file)
// ===========================================================================

const settingsTheme = {
	label: (t, _selected) => t,
	value: (t, _selected) => t,
	description: (t) => t,
	cursor: "→ ",
	hint: (t) => t,
};

function makeItems(specs) {
	return specs.map((s) => ({
		id: s.id,
		label: s.label,
		currentValue: s.currentValue,
		...(s.description !== undefined ? { description: s.description } : {}),
		...(s.values !== undefined ? { values: s.values } : {}),
	}));
}

function runSettingsList(spec) {
	const items = makeItems(spec.items);
	const changes = [];
	let cancelled = 0;
	const list = new SettingsList(
		items,
		spec.maxVisible,
		settingsTheme,
		(id, newValue) => changes.push([id, newValue]),
		() => {
			cancelled++;
		},
		spec.options ?? {},
	);

	const trace = [];
	for (const step of spec.steps) {
		let renderLines = null;
		switch (step.op) {
			case "render":
				renderLines = list.render(step.width);
				break;
			case "input":
				list.handleInput(step.data);
				break;
			case "updateValue":
				list.updateValue(step.id, step.value);
				break;
			default:
				throw new Error(`unknown settings-list op: ${step.op}`);
		}
		trace.push({ ...step, render: renderLines });
	}
	return {
		name: spec.name,
		items: spec.items,
		maxVisible: spec.maxVisible,
		options: spec.options ?? {},
		steps: trace,
		changes,
		cancelled,
	};
}

{
	const scenarios = [];
	const add = (spec) => scenarios.push(runSettingsList(spec));

	const basicItems = [
		{ id: "theme", label: "Theme", currentValue: "dark", values: ["dark", "light", "system"] },
		{ id: "font", label: "Font Size", currentValue: "14", values: ["12", "14", "16"], description: "The editor font size in points." },
		{ id: "wrap", label: "Word Wrap", currentValue: "off", values: ["on", "off"] },
		{ id: "path", label: "Config Path", currentValue: "/home/user/.config/app/settings.json" },
	];

	add({ name: "empty list no settings", items: [], maxVisible: 5, steps: [render(40)] });
	add({ name: "basic render with description", items: basicItems, maxVisible: 5, steps: [render(60)] });
	add({ name: "basic render narrow", items: basicItems, maxVisible: 5, steps: [render(30)] });
	add({
		name: "navigate and cycle values",
		items: basicItems,
		maxVisible: 5,
		steps: [
			render(60),
			inp("\x1b[B"), // down to Font Size
			render(60),
			inp("\r"), // cycle 14 -> 16
			render(60),
			inp(" "), // space cycles 16 -> 12
			render(60),
			inp("\x1b[B"), // down to Word Wrap
			inp("\r"), // off -> on
			render(60),
		],
	});
	add({
		name: "up wraps and cancel",
		items: basicItems,
		maxVisible: 5,
		steps: [inp("\x1b[A"), render(60), inp("\x1b")],
	});
	add({
		name: "scroll indicator when overflowing",
		items: makeItems(
			Array.from({ length: 8 }, (_, i) => ({ id: `s${i}`, label: `Setting ${i}`, currentValue: `v${i}` })),
		).map((it, i) => ({ id: it.id, label: it.label, currentValue: it.currentValue })),
		maxVisible: 3,
		steps: [render(50), inp("\x1b[B"), inp("\x1b[B"), inp("\x1b[B"), render(50)],
	});
	add({
		name: "long value truncation",
		items: [
			{ id: "url", label: "Server URL", currentValue: "https://very-long-example-domain.example.com/api/v1/endpoint" },
		],
		maxVisible: 5,
		steps: [render(40)],
	});
	add({
		name: "updateValue reflects in render",
		items: basicItems,
		maxVisible: 5,
		steps: [render(60), { op: "updateValue", id: "theme", value: "light" }, render(60)],
	});

	// --- search-enabled ---
	add({
		name: "search enabled initial render",
		items: basicItems,
		maxVisible: 5,
		options: { enableSearch: true },
		steps: [render(60)],
	});
	add({
		name: "search filters via typed input",
		items: basicItems,
		maxVisible: 5,
		options: { enableSearch: true },
		steps: [render(60), inp("f"), render(60), inp("o"), render(60)],
	});
	add({
		name: "search no matching settings",
		items: basicItems,
		maxVisible: 5,
		options: { enableSearch: true },
		steps: [inp("z"), inp("z"), inp("z"), render(60)],
	});
	add({
		name: "search space is ignored",
		items: basicItems,
		maxVisible: 5,
		options: { enableSearch: true },
		steps: [inp(" "), render(60)],
	});
	add({
		name: "search enabled empty items",
		items: [],
		maxVisible: 5,
		options: { enableSearch: true },
		steps: [render(60)],
	});
	add({
		name: "CJK settings labels",
		items: [
			{ id: "lang", label: "言語設定", currentValue: "日本語", values: ["日本語", "English"], description: "アプリケーションの表示言語を選択します。" },
			{ id: "mode", label: "モード", currentValue: "標準" },
		],
		maxVisible: 5,
		steps: [render(50)],
	});

	dump("settings_list_scenarios", scenarios);
}

console.log(`\ntotal input/list vectors: ${total}`);
