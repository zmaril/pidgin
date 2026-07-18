// Vector generator for the bit-exact Rust port of pi's TUI width module.
//
// Runs pi's own exported functions from vendor/pi/packages/tui/src/utils.ts
// (Node 22 strips TS types natively; get-east-asian-width@1.6.0 resolves
// transitively relative to utils.ts) and dumps input -> expected-output JSON
// vectors that the Rust test suite (crates/atilla-tui/tests/width_vectors.rs)
// asserts byte-identical.
//
// Run from this directory:  node generate.mjs
// Output is written to ../../tests/vectors/*.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10),
// get-east-asian-width @ 1.6.0.

import {
	visibleWidth,
	truncateToWidth,
	wrapTextWithAnsi,
	sliceByColumn,
	sliceWithWidth,
	extractSegments,
	normalizeTerminalOutput,
	extractAnsiCode,
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

// --- helpers to enumerate codepoints -------------------------------------

const cp = (n) => String.fromCodePoint(n);
function range(lo, hi, step = 1) {
	const out = [];
	for (let i = lo; i <= hi; i += step) out.push(i);
	return out;
}

// -------------------------------------------------------------------------
// visibleWidth vectors: the whole Unicode-relevant surface.
// -------------------------------------------------------------------------

const visibleWidthInputs = new Set();
const addW = (s) => visibleWidthInputs.add(s);

// ASCII
addW("");
addW(" ");
addW("a");
addW("hello");
addW("hello world this is a test");
for (let c = 0x20; c <= 0x7e; c++) addW(cp(c));
addW("The quick brown fox jumps over the lazy dog 0123456789");

// Tabs
addW("\t");
addW("\t\t");
addW("a\tb");
addW("\t\x1b[31m界\x1b[0m");
addW("a\tb\tc");

// Control / C0
for (let c = 0x00; c <= 0x1f; c++) addW(cp(c));
addW("\x7f");
addW("a\x00b");
addW("a\x08b");

// Combining marks
addW("́"); // combining acute (isolated)
addW("é"); // e + combining acute
addW("à́̂"); // multiple combining
addW("ก่"); // Thai consonant + tone mark
addW("̨"); // combining ogonek isolated
addW("̈́"); // combining greek dialytika tonos

// Zero-width / format / default-ignorable
addW("​"); // zero width space
addW("‌"); // ZWNJ
addW("‍"); // ZWJ (isolated)
addW("﻿"); // BOM / zero width no-break space
addW("­"); // soft hyphen
addW("⁠"); // word joiner
addW("᠎"); // mongolian vowel separator
addW("⁤"); // invisible plus
addW("a​b");

// NOTE: pi's zero-width class includes \p{Surrogate} for lone surrogates that
// can appear in JS UTF-16 strings. Rust `String`/`&str` is guaranteed valid
// UTF-8 and can never hold a lone surrogate, so that code path is unreachable
// in the Rust port and no lone-surrogate vectors are emitted (they would also
// break JSON parsing, since serde_json rejects unpaired surrogate escapes).

// CJK wide
addW("界");
addW("中文汉字");
addW("あ"); // hiragana
addW("ア"); // katakana
addW("한"); // hangul
addW("ㄅ"); // bopomofo
addW("你好世界");
addW("日本語のテスト");
addW("龍"); // CJK ext
addW("𠀀"); // CJK ext B (astral)

// Fullwidth / halfwidth forms (U+FF00-FFEF)
addW("Ａ"); // fullwidth A (U+FF21)
addW("１２３"); // fullwidth digits
addW("！"); // fullwidth exclamation
addW("ｱ"); // halfwidth katakana A (U+FF71)
addW("ﾟ"); // halfwidth katakana handakuten (U+FF9F)
addW("Ａｱ"); // mixed
for (const n of range(0xff00, 0xffef)) addW(cp(n));

// Ambiguous width
addW("§"); // U+00A7 ambiguous
addW("±"); // U+00B1
addW("×"); // U+00D7
addW("÷"); // U+00F7
addW("‐"); // U+2010 hyphen ambiguous
addW("’"); // U+2019 right single quote
addW("“"); // U+201C
addW("…"); // U+2026 horizontal ellipsis
addW("←"); // U+2190 arrow ambiguous
addW("○"); // U+25CB circle
addW("★"); // U+2605
addW("♠"); // U+2660

// Emoji — single
addW("🙂");
addW("😀");
addW("👍");
addW("✅");
addW("⚡");
addW("👨");
addW("🎉");
addW("🔥");
addW("🌍");
addW("🚀");

// Emoji — with variation selectors
addW("⚡️"); // lightning + VS16
addW("✅️");
addW("❤️"); // red heart (U+2764 U+FE0F)
addW("☺️");
addW("©️"); // copyright + VS16
addW("®️");
addW("™️");
addW("▶️");
addW("⏸️");
addW("#️"); // hash + VS16 (partial keycap)
addW("❤"); // bare heart no VS16

// Emoji — keycaps
addW("#️⃣"); // keycap hash
addW("*️⃣"); // keycap star
addW("0️⃣");
addW("1️⃣");
addW("9️⃣");
addW("1️⃣"); // explicit keycap sequence

// Emoji — ZWJ sequences
addW("👨‍💻"); // man technologist
addW("👨‍👩‍👧‍👦"); // family
addW("👩‍🚀"); // woman astronaut
addW("🏳️‍🌈"); // rainbow flag
addW("🏴‍☠️"); // pirate flag
addW("👨‍❤️‍👨"); // couple
addW("🧑‍🤝‍🧑");

// Emoji — skin tones
addW("👍🏻");
addW("👍🏼");
addW("👍🏽");
addW("👍🏾");
addW("👍🏿");
addW("🤚🏽");
addW("👋🏿");
addW("🏻"); // isolated skin tone modifier

// Regional indicators — isolated (each U+1F1E6..U+1F1FF)
for (const n of range(0x1f1e6, 0x1f1ff)) addW(cp(n));
// Regional indicators — pairs (flags)
addW("🇯🇵");
addW("🇺🇸");
addW("🇬🇧");
addW("🇨🇳");
addW("🇩🇪");
addW("🇫🇷");
addW("🇨"); // partial
addW("      - 🇨"); // list line from regression test
addW("🇨🇳🇺🇸"); // two flags adjacent
addW("🇦🇧🇨"); // three RIs (odd count)

// Misc symbol blocks in couldBeEmoji prefilter ranges
addW("⌚"); // U+231A watch (Misc technical)
addW("⌨"); // U+2328 keyboard
addW("⏰"); // U+23F0
addW("☀"); // U+2600 (misc symbols)
addW("☎"); // U+260E
addW("♻"); // U+267B recycling
addW("✂"); // U+2702 scissors
addW("➡"); // U+27a1 dingbat arrow
addW("⭐"); // U+2b50 star
addW("⭕"); // U+2b55 circle
addW("☕"); // U+2615 hot beverage

// Thai / Lao SARA AM
addW("ำ"); // U+0E33 isolated
addW("ຳ"); // U+0EB3 isolated
addW("กำ"); // Thai consonant + AM
addW("ກຳ"); // Lao consonant + AM
addW("ำabc");
addW("ຳabc");
addW("กำข"); // consonant AM consonant
addW("กํา"); // already-decomposed form

// ANSI / OSC / APC escape sequences
addW("\x1b[31mred\x1b[0m");
addW("\x1b[1;38;5;240mstyled\x1b[0m");
addW("\x1b[38;2;255;128;0mrgb\x1b[0m");
addW("\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\");
addW("\x1b]133;A\x07hello\x1b]133;B\x07"); // OSC 133 BEL
addW("\x1b]133;A\x1b\\hello\x1b]133;B\x1b\\"); // OSC 133 ST
addW("\x1b_payload\x07"); // APC BEL
addW("\x1b_G...\x1b\\"); // APC ST
addW("\x1b]0;window title\x07");
addW("abc\x1bnot-ansi def"); // malformed / bare ESC
addW("\x1b[31munterminated"); // no terminator char
addW("界\x1b[0m界");
addW("\x1b[38;5;196m🔥\x1b[0m");

// Mixed real-world lines
addW("out 192M\t.pi/skill-tests/results-ha");
addW("read this thread \x1b[4mhttps://example.com/very/long/path\x1b[24m");
addW("🙂界🙂界🙂界");
addW("🙂\t界 \x1b_abc\x07");

const visibleWidthVectors = [...visibleWidthInputs].map((input) => ({
	input,
	expected: visibleWidth(input),
}));
dump("visible_width", visibleWidthVectors);

// -------------------------------------------------------------------------
// graphemeWidth vectors: single graphemes fed through visibleWidth.
// graphemeWidth is not exported, but for a single grapheme cluster with no
// tab/ANSI, visibleWidth(g) === graphemeWidth(g). We only include inputs that
// segment as exactly one grapheme so the mapping is exact.
// -------------------------------------------------------------------------

const seg = new Intl.Segmenter(undefined, { granularity: "grapheme" });
const isSingleGrapheme = (s) => {
	let n = 0;
	for (const _ of seg.segment(s)) {
		n++;
		if (n > 1) return false;
	}
	return n === 1;
};

const graphemeInputs = new Set();
const addG = (s) => {
	if (s.length > 0 && !s.includes("\x1b") && isSingleGrapheme(s)) graphemeInputs.add(s);
};
// Reuse the whole width corpus, keep only single graphemes.
for (const s of visibleWidthInputs) addG(s);
// Plus explicit per-grapheme probes.
addG("\t");
for (let c = 0x20; c <= 0x7e; c++) addG(cp(c));
for (const n of range(0x1f1e6, 0x1f1ff)) addG(cp(n));
for (const n of range(0xff00, 0xffef)) addG(cp(n));

const graphemeWidthVectors = [...graphemeInputs].map((input) => ({
	input,
	expected: visibleWidth(input),
}));
dump("grapheme_width", graphemeWidthVectors);

// -------------------------------------------------------------------------
// eastAsianWidth codepoint sweep (base-codepoint answers get-east-asian-width
// gives). visibleWidth of a single non-combining, non-emoji codepoint equals
// eastAsianWidth(cp) in pi. We sweep broad codepoint ranges to pin the width
// table the Rust side must match. Filter to single graphemes with no
// combining/zero-width surprises by only keeping printable base scalars.
// -------------------------------------------------------------------------

const sweepPoints = new Set();
const addP = (n) => sweepPoints.add(n);
// Broad sweeps across width-relevant blocks.
for (const n of range(0x20, 0x7e)) addP(n); // ASCII
for (const n of range(0xa0, 0x2ff)) addP(n); // Latin-1 supp, Latin ext, IPA
for (const n of range(0x370, 0x4ff)) addP(n); // Greek, Cyrillic
for (const n of range(0x1100, 0x11ff)) addP(n); // Hangul Jamo (wide)
for (const n of range(0x2000, 0x27ff)) addP(n); // punctuation, symbols, arrows, dingbats
for (const n of range(0x2e80, 0x303f)) addP(n); // CJK radicals, kangxi, symbols
for (const n of range(0x3040, 0x30ff)) addP(n); // hiragana, katakana
for (const n of range(0x3400, 0x34ff)) addP(n); // CJK ext A start
for (const n of range(0x4e00, 0x4eff)) addP(n); // CJK unified start
for (const n of range(0xac00, 0xacff)) addP(n); // Hangul syllables start
for (const n of range(0xf900, 0xfaff)) addP(n); // CJK compat ideographs
for (const n of range(0xfe30, 0xfe4f)) addP(n); // CJK compat forms
for (const n of range(0xff00, 0xffef)) addP(n); // halfwidth/fullwidth forms
for (const n of range(0x1f300, 0x1f5ff)) addP(n); // misc symbols and pictographs
for (const n of range(0x1f600, 0x1f64f)) addP(n); // emoticons
for (const n of range(0x1f900, 0x1f9ff)) addP(n); // supplemental symbols
for (const n of range(0x20000, 0x2007f)) addP(n); // CJK ext B start (wide)

const eawVectors = [];
for (const n of sweepPoints) {
	// Skip surrogate range (invalid scalars).
	if (n >= 0xd800 && n <= 0xdfff) continue;
	const s = cp(n);
	// Only keep single-grapheme, non-tab inputs so visibleWidth == graphemeWidth.
	if (!isSingleGrapheme(s)) continue;
	eawVectors.push({ codepoint: n, input: s, expected: visibleWidth(s) });
}
dump("east_asian_width", eawVectors);

// -------------------------------------------------------------------------
// extractAnsiCode vectors: (string, pos) -> {code, length} | null
// -------------------------------------------------------------------------

const ansiCases = [
	["\x1b[31m", 0],
	["\x1b[0m", 0],
	["\x1b[38;5;240m", 0],
	["\x1b[38;2;255;0;0m", 0],
	["\x1b[2J", 0],
	["\x1b[10G", 0],
	["\x1b[2K", 0],
	["\x1b[H", 0],
	["\x1b[1;5H", 0],
	["\x1b]8;;https://x.co\x1b\\", 0], // OSC ST
	["\x1b]8;;https://x.co\x07", 0], // OSC BEL
	["\x1b]0;title\x07", 0],
	["\x1b]133;A\x1b\\", 0],
	["\x1b_payload\x07", 0], // APC BEL
	["\x1b_Gdata\x1b\\", 0], // APC ST
	["prefix\x1b[31mred", 6], // pos in middle
	["\x1b[31munterminated", 0], // CSI no terminator -> null
	["\x1b]8;;unterminated", 0], // OSC no terminator -> null
	["\x1b_unterminated", 0], // APC no terminator -> null
	["\x1bX", 0], // ESC + unknown -> null
	["notescape", 0], // no ESC at pos -> null
	["a\x1b[31m", 0], // pos 0 not ESC -> null
	["", 0], // empty -> null
	["\x1b", 0], // lone ESC -> null
	["\x1b[", 0], // ESC [ then end -> null
];

const extractAnsiVectors = ansiCases.map(([str, pos]) => {
	const r = extractAnsiCode(str, pos);
	return { input: str, pos, expected: r === null ? null : { code: r.code, length: r.length } };
});
dump("extract_ansi_code", extractAnsiVectors);

// -------------------------------------------------------------------------
// normalizeTerminalOutput vectors
// -------------------------------------------------------------------------

const normInputs = [
	"ำ",
	"ຳ",
	"ำabc",
	"ຳabc",
	"กำ",
	"ກຳ",
	"no tabs no am",
	"a\tb",
	"\ttext",
	"a\tb\tc\t",
	"\x1b]8;;https://example.test/a\tb\x07label\ttext",
	"\x1b]0;window\ttitle\x1b\\label\ttext",
	"\x1b_payload\tdata\x1b\\label\ttext",
	"ำ\tabc",
	"\x1b[31m\tred\x1b[0m",
	"",
	"plain",
];
const normVectors = normInputs.map((input) => ({ input, expected: normalizeTerminalOutput(input) }));
dump("normalize_terminal_output", normVectors);

// -------------------------------------------------------------------------
// truncateToWidth vectors: (text, maxWidth, ellipsis, pad) -> string
// -------------------------------------------------------------------------

const truncCases = [
	// From truncate-to-width.test.ts
	["🙂界".repeat(50), 40, "…", false],
	[`\x1b[31m${"hello ".repeat(20)}\x1b[0m`, 20, "…", false],
	[`abc\x1bnot-ansi ${"🙂".repeat(20)}`, 20, "…", false],
	["abcdef", 1, "🙂", false],
	["abcdef", 2, "🙂", false],
	["a", 2, "🙂", false],
	["界", 2, "🙂", false],
	["🙂界🙂界🙂界", 8, "…", true],
	[`\x1b[31m${"hello".repeat(20)}`, 10, "", false],
	["🙂\t界 \x1b_abc\x07", 7, "…", true],
	// Broader coverage
	["hello", 10, "...", false],
	["hello", 10, "...", true],
	["hello world", 5, "...", false],
	["hello world", 5, "...", true],
	["hello world", 8, "…", false],
	["", 5, "...", false],
	["", 5, "...", true],
	["abc", 0, "...", false],
	["abc", 3, "...", false],
	["abcdefgh", 3, "...", false],
	["abcdefgh", 4, "...", false],
	["界界界界", 5, "…", false],
	["界界界界", 5, "…", true],
	["界界界界", 6, "...", false],
	["🙂🙂🙂🙂", 5, "…", false],
	["\x1b[1;32mgreen bold text here\x1b[0m", 12, "...", false],
	["\x1b[44mbg text long enough to truncate\x1b[0m", 10, "…", true],
	["tab\there\tmore", 8, "…", false],
	["tab\there\tmore", 8, "…", true],
	["ascii only text", 20, "...", true],
	["ascii only text", 20, "...", false],
	["🇨🇳flags🇺🇸here", 6, "…", false],
	["👨‍💻coding", 4, "…", false],
	["a\x1b[31mb\x1b[0mc", 2, "…", false],
	["日本語のテスト文字列", 10, "…", true],
	["short", 100, "...", true],
];
const truncVectors = truncCases.map(([text, maxWidth, ellipsis, pad]) => ({
	text,
	maxWidth,
	ellipsis,
	pad,
	expected: truncateToWidth(text, maxWidth, ellipsis, pad),
}));
dump("truncate_to_width", truncVectors);

// -------------------------------------------------------------------------
// wrapTextWithAnsi vectors: (text, width) -> string[]
// -------------------------------------------------------------------------

const wrapCases = [
	// From wrap-ansi.test.ts
	["read this thread \x1b[4mhttps://example.com/very/long/path/that/will/wrap\x1b[24m", 40],
	["\x1b[4munderlined text here \x1b[24mmore", 18],
	["prefix \x1b[4mhttps://example.com/very/long/path/that/will/definitely/wrap\x1b[24m suffix", 30],
	["\x1b[44mhello world this is blue background text\x1b[0m", 15],
	["\x1b[41mprefix \x1b[4mUNDERLINED_CONTENT_THAT_WRAPS\x1b[24m suffix\x1b[0m", 20],
	["first\nsecond\r\nthird\rfourth", 80],
	["\x1b[31mfirst\r\nsecond\rthird\x1b[0m", 80],
	["hello world this is a test", 10],
	["This is an example 中文汉字测试段落内容中文汉字测试段落内容.", 40],
	["\x1b[31mThis is an example 中文汉字测试段落内容中文汉字测试段落内容.\x1b[0m", 40],
	["  ", 1],
	["\x1b[31mhello world this is red\x1b[0m", 10],
	["      - 🇨", 9],
	// OSC 8 hyperlinks
	["\x1b]8;;https://example.com\x1b\\0123456789\x1b]8;;\x1b\\", 6],
	[`\x1b]8;;https://example.com/oauth/${"a".repeat(32)}\x07https://example.com/oauth/${"a".repeat(32)}\x1b]8;;\x07`, 20],
	["before \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ after", 80],
	// Broader coverage
	["", 10],
	["short", 80],
	["a b c d e f g h i j k l m n o p", 5],
	["supercalifragilisticexpialidocious", 10],
	["word1 word2 word3", 6],
	["中文汉字测试段落内容", 6],
	["日本語のテストです", 8],
	["🙂🙂🙂🙂🙂🙂🙂🙂", 4],
	["line one\nline two\nline three", 10],
	["\x1b[1mbold\x1b[0m normal \x1b[3mitalic\x1b[0m", 8],
	["trailing spaces here     ", 10],
	["tab\tseparated\ttext here", 10],
	["one", 1],
	["\x1b[38;5;208morange colored text that wraps around\x1b[0m", 12],
	["mixing ASCII and 中文 in one line for wrapping", 15],
];
const wrapVectors = wrapCases.map(([text, width]) => ({
	text,
	width,
	expected: wrapTextWithAnsi(text, width),
}));
dump("wrap_text_with_ansi", wrapVectors);

// -------------------------------------------------------------------------
// sliceByColumn / sliceWithWidth vectors: (line, startCol, length, strict)
// -------------------------------------------------------------------------

const sliceCases = [
	["out 192M\t.pi/skill-tests/results-ha", 0, 10, true],
	["out 192M\t.pi/skill-tests/results-ha", 0, 10, false],
	["hello world", 0, 5, false],
	["hello world", 6, 5, false],
	["hello world", 2, 3, false],
	["hello world", 0, 100, false],
	["hello world", 0, 0, false],
	["hello world", 20, 5, false],
	["\x1b[31mred text here\x1b[0m", 0, 3, false],
	["\x1b[31mred text here\x1b[0m", 4, 4, false],
	["\x1b[31mred\x1b[0m normal", 2, 6, false],
	["界界界界界", 0, 5, false],
	["界界界界界", 0, 5, true],
	["界界界界界", 1, 4, false],
	["界界界界界", 1, 4, true],
	["a界b界c", 0, 3, false],
	["a界b界c", 0, 3, true],
	["a界b界c", 1, 2, true],
	["🙂界🙂界", 0, 4, false],
	["🙂界🙂界", 0, 3, true],
	["tab\there\tmore", 0, 5, false],
	["tab\there\tmore", 0, 5, true],
	["\x1b[1;44mstyled bg text\x1b[0m", 3, 5, false],
	["hello", 2, 10, true],
	["中文abc英文", 0, 4, true],
	["中文abc英文", 2, 5, false],
];
const sliceVectors = sliceCases.map(([line, startCol, length, strict]) => {
	const sw = sliceWithWidth(line, startCol, length, strict);
	return {
		line,
		startCol,
		length,
		strict,
		expectedText: sliceByColumn(line, startCol, length, strict),
		expectedWidth: sw.width,
	};
});
dump("slice_by_column", sliceVectors);

// -------------------------------------------------------------------------
// extractSegments vectors:
// (line, beforeEnd, afterStart, afterLen, strictAfter)
// -------------------------------------------------------------------------

const extractSegCases = [
	["out 192M\t.pi/skill-tests/results-ha", 10, 13, 10, true],
	["out 192M\t.pi/skill-tests/results-ha", 11, 13, 10, true],
	["out 192M\t.pi/skill-tests/results-ha", 8, 13, 10, true],
	["hello world foo bar", 5, 6, 5, false],
	["hello world foo bar", 5, 6, 5, true],
	["hello world foo bar", 0, 6, 5, false],
	["\x1b[31mred\x1b[0m green blue", 3, 4, 5, false],
	["\x1b[1;44mstyled overlay text here\x1b[0m", 6, 8, 6, false],
	["\x1b[1;44mstyled overlay text here\x1b[0m", 6, 8, 6, true],
	["界界界界界界", 4, 6, 4, true],
	["界界界界界界", 4, 6, 4, false],
	["界界界界界界", 3, 6, 4, true],
	["a界b界c界d", 2, 4, 3, true],
	["🙂界🙂界🙂界", 4, 6, 4, false],
	["prefix\x1b[32m middle \x1b[0msuffix", 6, 8, 6, false],
	["overlay test", 5, 5, 0, false],
	["overlay test", 5, 5, 0, true],
	["tab\there\tmore text", 5, 8, 6, true],
];
const extractSegVectors = extractSegCases.map(([line, beforeEnd, afterStart, afterLen, strictAfter]) => {
	const r = extractSegments(line, beforeEnd, afterStart, afterLen, strictAfter);
	return {
		line,
		beforeEnd,
		afterStart,
		afterLen,
		strictAfter,
		expectedBefore: r.before,
		expectedBeforeWidth: r.beforeWidth,
		expectedAfter: r.after,
		expectedAfterWidth: r.afterWidth,
	};
});
dump("extract_segments", extractSegVectors);

// -------------------------------------------------------------------------
// Grapheme segmentation vectors: verify unicode-segmentation (Rust) matches
// Intl.Segmenter (pi). Each vector is a string and its exact grapheme cluster
// array as produced by the shared grapheme segmenter.
// -------------------------------------------------------------------------

const segStrings = [
	"hello",
	"café",
	"é", // combining
	"áb̂c", // combining marks
	"中文汉字",
	"👍🏻",
	"👨‍💻",
	"👨‍👩‍👧‍👦",
	"🏳️‍🌈",
	"🇯🇵🇺🇸",
	"🇨🇳",
	"🇨",
	"#️⃣",
	"⚡️",
	"❤️",
	"a🙂b界c",
	"ก่",
	"กำ",
	"ກຳ",
	"This is an example 中文汉字测试段落内容中文汉字测试段落内容.",
	"é̂̃", // multiple stacked
	"🧑‍🤝‍🧑",
	"🏴‍☠️",
	"tab\there",
	"🙂界🙂界",
	"가", // hangul jamo L + V
	"각", // precomposed hangul
	"a‍b", // ZWJ between letters
];
const segVectors = segStrings.map((input) => ({
	input,
	graphemes: [...seg.segment(input)].map((s) => s.segment),
}));
dump("grapheme_segmentation", segVectors);

console.log(`\nTOTAL VECTORS: ${total}`);
