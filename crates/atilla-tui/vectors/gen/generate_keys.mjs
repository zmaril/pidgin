// Vector generator for the bit-exact Rust port of pi's TUI key parser.
//
// Runs pi's own exported functions from vendor/pi/packages/tui/src/keys.ts
// (Node 22 strips TS types natively; keys.ts has no runtime dependencies) and
// dumps input -> expected-output JSON vectors that the Rust test suite
// (crates/atilla-tui/tests/keys_vectors.rs) asserts byte-identical.
//
// Run from this directory:  node generate_keys.mjs
// Output is written to ../../tests/vectors/keys_*.json
//
// pi upstream pin: vendor/pi submodule @ 3da591a (pi v0.80.10).
//
// Global state: pi's key parser has two pieces of ambient state that change
// parse results:
//   1. the Kitty protocol active flag (setKittyProtocolActive), and
//   2. isWindowsTerminalSession(), derived from WT_SESSION / SSH_* env vars.
// Every matchesKey / parseKey vector carries the `kitty` flag and a `wt`
// boolean (the isWindowsTerminalSession result) so the Rust harness can
// reproduce the exact state before asserting.

import {
	matchesKey,
	parseKey,
	decodeKittyPrintable,
	decodePrintableKey,
	isKeyRelease,
	isKeyRepeat,
	setKittyProtocolActive,
} from "../../../../vendor/pi/packages/tui/src/keys.ts";
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

// --- env helpers ----------------------------------------------------------
// isWindowsTerminalSession() = WT_SESSION set (truthy) AND none of
// SSH_CONNECTION / SSH_CLIENT / SSH_TTY set. We realize a desired `wt` result
// by setting/clearing WT_SESSION with SSH always cleared.
const SSH_VARS = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"];
function setEnvForWt(wt) {
	for (const v of SSH_VARS) delete process.env[v];
	if (wt) process.env.WT_SESSION = "test-session";
	else delete process.env.WT_SESSION;
}
function setEnvRaw(env) {
	for (const v of ["WT_SESSION", ...SSH_VARS]) {
		if (env && env[v] !== undefined && env[v] !== null) process.env[v] = env[v];
		else delete process.env[v];
	}
}
// Start from a clean, deterministic env.
setEnvForWt(false);

// --- corpora --------------------------------------------------------------

const dataCorpus = new Set();
const addD = (s) => dataCorpus.add(s);

// C0 control chars + DEL
for (let c = 0; c <= 0x1f; c++) addD(String.fromCharCode(c));
addD("\x7f");

// Printable ASCII letters/digits/symbols (representative subset)
for (const ch of "aczAZ019") addD(ch);
for (const ch of "`-[]\\;/_+~<?") addD(ch);
addD(" ");

// Plain escape and two-byte escape combos
addD("\x1b");
addD("\x1b\x1b");
addD("\x1b\r");
addD("\x1b\n");
addD("\x1b ");
addD("\x1b\x7f");
addD("\x1b\b");
addD("\x1bOM");

// Alt + printable (ESC prefix)
for (const ch of "abyz1,.") addD(`\x1b${ch}`);
for (const ch of "BFbfpn") addD(`\x1b${ch}`);

// Ctrl+Alt (ESC + control char)
for (const c of [1, 2, 3, 4, 26]) addD(`\x1b${String.fromCharCode(c)}`);
addD("\x1b\x1c");
addD("\x1b\x1d");
addD("\x1b\x1f");

// Legacy CSI arrows / SS3 arrows
for (const t of "ABCD") {
	addD(`\x1b[${t}`);
	addD(`\x1bO${t}`);
}
addD("\x1bOH");
addD("\x1bOF");
addD("\x1b[H");
addD("\x1b[F");
addD("\x1b[E");
// SS3 function keys P-S and rxvt ctrl SS3 a-e
for (const t of "PQRS") addD(`\x1bO${t}`);
for (const t of "abcde") addD(`\x1bO${t}`);

// Arrows / home-end with CSI modifiers
for (const m of [2, 3, 5]) {
	for (const t of "ABCD") addD(`\x1b[1;${m}${t}`);
	addD(`\x1b[1;${m}H`);
	addD(`\x1b[1;${m}F`);
}
addD("\x1b[1;5C");
addD("\x1b[1;5D");
addD("\x1b[1;3C");
addD("\x1b[1;3D");
addD("\x1b[1;3:2A"); // arrow with event type

// Functional ~ sequences
for (const n of [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 17, 18, 19, 20, 21, 23, 24]) {
	addD(`\x1b[${n}~`);
}
for (const m of [2, 3, 5]) {
	addD(`\x1b[3;${m}~`);
	addD(`\x1b[5;${m}~`);
}
addD("\x1b[3;5:3~"); // functional with event type
// Double-bracket legacy forms
for (const t of "ABCDE") addD(`\x1b[[${t}`);
addD("\x1b[[5~");
addD("\x1b[[6~");

// rxvt shift/ctrl modifier sequences
for (const t of "abcde") addD(`\x1b[${t}`);
for (const n of [2, 3, 5, 6, 7, 8]) {
	addD(`\x1b[${n}$`);
	addD(`\x1b[${n}^`);
}

// shift+tab
addD("\x1b[Z");

// Kitty CSI-u: plain, modified, alternate-key, event-type forms
for (const cp of [97, 99, 122, 107, 49, 13, 27, 9, 32, 127]) {
	addD(`\x1b[${cp}u`);
	for (const m of [2, 5, 9, 17]) addD(`\x1b[${cp};${m}u`);
}
// Event types on CSI-u
addD("\x1b[97;1:1u");
addD("\x1b[97;1:2u");
addD("\x1b[97;1:3u");
addD("\x1b[99;5:2u");
addD("\x1b[99;5:3u");
// Explicit test-file CSI-u alternate-key sequences
for (const s of [
	"\x1b[1089::99;5u",
	"\x1b[1074::100;5u",
	"\x1b[1103::122;5u",
	"\x1b[1079::112;6u",
	"\x1b[99;5u",
	"\x1b[107;9u",
	"\x1b[13;9u",
	"\x1b[107;13u",
	"\x1b[107;14u",
	"\x1b[49u",
	"\x1b[49;5u",
	"\x1b[99:67:99;2u",
	"\x1b[1089::99;5:3u",
	"\x1b[1089:1057:99;6:2u",
	"\x1b[107::118;5u",
	"\x1b[47::91;5u",
	"\x1b[69;2u",
	"\x1b[99;17u",
	"\x1b[104;7u",
]) {
	addD(s);
}
// Kitty keypad functional codepoints 57399..57426
for (let cp = 57399; cp <= 57426; cp++) addD(`\x1b[${cp}u`);
addD("\x1b[57414u"); // kpEnter
addD("\x1b[57414;2u");

// xterm modifyOtherKeys CSI 27 ; mod ; keycode ~
for (const [m, code] of [
	[5, 99],
	[5, 100],
	[5, 122],
	[5, 13],
	[2, 13],
	[3, 13],
	[2, 9],
	[5, 9],
	[3, 9],
	[1, 127],
	[5, 127],
	[3, 127],
	[1, 27],
	[1, 32],
	[5, 32],
	[5, 47],
	[5, 49],
	[2, 49],
	[2, 69],
	[6, 69],
	[7, 104],
	[2, 196],
]) {
	addD(`\x1b[27;${m};${code}~`);
}

// Multibyte / non-ASCII printable single graphemes
for (const s of ["é", "ä", "ñ", "中", "あ", "🙂", "€"]) addD(s);

// Bracketed paste and near-miss content (must not be treated as release/repeat)
addD("\x1b[200~90:62:3F:A5\x1b[201~");
addD("\x1b[200~foo:2Ubar\x1b[201~");

const dataList = [...dataCorpus];

// keyId corpus. Curated to cover every matching branch (each special key,
// a representative letter/digit/symbol, and each modifier-combo class) while
// staying bounded; all explicit keys.test.ts pairs are added separately below.
const keyIdCorpus = [
	// special keys (representative per matchesKey switch arm)
	"escape",
	"enter",
	"tab",
	"space",
	"backspace",
	"delete",
	"insert",
	"clear",
	"home",
	"end",
	"pageUp",
	"up",
	"left",
	"right",
	"f1",
	"f12",
	// printable base keys
	"c",
	"z",
	"1",
	"/",
	"_",
	// ctrl combos (letters, digit, symbols, specials)
	"ctrl+c",
	"ctrl+h",
	"ctrl+/",
	"ctrl+space",
	"ctrl+enter",
	"ctrl+backspace",
	"ctrl+left",
	// shift combos
	"shift+tab",
	"shift+enter",
	"shift+e",
	"shift+up",
	// alt combos
	"alt+enter",
	"alt+space",
	"alt+backspace",
	"alt+left",
	"alt+a",
	// super + multi-modifier combos
	"super+k",
	"ctrl+alt+h",
	"ctrl+shift+p",
	"ctrl+super+k",
	"ctrl+shift+super+k",
];

// --- matchesKey cross-product sweep --------------------------------------
// Every (data, keyId) pair under both Kitty states, env => not-WT. This pins
// matchesKey exhaustively (mostly negatives, which catch over-matching).
const matchesVectors = [];
for (const kitty of [false, true]) {
	setKittyProtocolActive(kitty);
	setEnvForWt(false);
	for (const input of dataList) {
		for (const keyId of keyIdCorpus) {
			matchesVectors.push({ input, keyId, kitty, wt: false, expected: matchesKey(input, keyId) });
		}
	}
}
setKittyProtocolActive(false);

// Explicit (input, keyId, kitty, wt) pairs transcribed from keys.test.ts so
// every one of its matchesKey assertions is represented byte-for-byte. The
// expected value is computed live from pi, so it always matches the suite.
const explicitMatches = [
	// Kitty protocol with alternate keys (kitty=true)
	["\x1b[1089::99;5u", "ctrl+c", true],
	["\x1b[1074::100;5u", "ctrl+d", true],
	["\x1b[1103::122;5u", "ctrl+z", true],
	["\x1b[1079::112;6u", "ctrl+shift+p", true],
	["\x1b[99;5u", "ctrl+c", true],
	["\x1b[107;9u", "super+k", true],
	["\x1b[13;9u", "super+enter", true],
	["\x1b[107;13u", "ctrl+super+k", true],
	["\x1b[107;14u", "ctrl+shift+super+k", true],
	["\x1b[107;13u", "super+k", true],
	["\x1b[49u", "1", true],
	["\x1b[49;5u", "ctrl+1", true],
	["\x1b[49;5u", "ctrl+2", true],
	["\x1b[57400u", "1", true],
	["\x1b[57410u", "/", true],
	["\x1b[57417u", "left", true],
	["\x1b[57426u", "delete", true],
	["\x1b[99:67:99;2u", "shift+c", true],
	["\x1b[1089::99;5:3u", "ctrl+c", true],
	["\x1b[1089:1057:99;6:2u", "ctrl+shift+c", true],
	["\x1b[107::118;5u", "ctrl+k", true],
	["\x1b[107::118;5u", "ctrl+v", true],
	["\x1b[47::91;5u", "ctrl+/", true],
	["\x1b[47::91;5u", "ctrl+[", true],
	["\x1b[1089::99;5u", "ctrl+d", true],
	["\x1b[1089::99;5u", "ctrl+shift+c", true],
	["\x1b[69;2u", "shift+e", true],
	// modifyOtherKeys matching (kitty=false)
	["\x1b[27;5;99~", "ctrl+c", false],
	["\x1b[27;5;100~", "ctrl+d", false],
	["\x1b[27;5;122~", "ctrl+z", false],
	["\x1b[27;5;13~", "ctrl+enter", false],
	["\x1b[27;2;13~", "shift+enter", false],
	["\x1b[27;3;13~", "alt+enter", false],
	["\x1b[27;2;9~", "shift+tab", false],
	["\x1b[27;5;9~", "ctrl+tab", false],
	["\x1b[27;3;9~", "alt+tab", false],
	["\x1b[27;1;127~", "backspace", false],
	["\x1b[27;5;127~", "ctrl+backspace", false],
	["\x1b[27;3;127~", "alt+backspace", false],
	["\x1b[27;1;27~", "escape", false],
	["\x1b[27;1;32~", "space", false],
	["\x1b[27;5;32~", "ctrl+space", false],
	["\x1b[27;5;47~", "ctrl+/", false],
	["\x1b[27;5;49~", "ctrl+1", false],
	["\x1b[27;2;49~", "shift+1", false],
	["\x1b[27;2;69~", "shift+e", false],
	["\x1b[27;6;69~", "ctrl+shift+e", false],
	["\x1b[104;7u", "ctrl+alt+h", false],
	["\x1b[27;7;104~", "ctrl+alt+h", false],
	// Legacy key matching (kitty=false)
	["\x03", "ctrl+c", false],
	["\x04", "ctrl+d", false],
	["\x1b", "escape", false],
	["\n", "enter", false],
	["\x00", "ctrl+space", false],
	["\x1c", "ctrl+\\", false],
	["\x1d", "ctrl+]", false],
	["\x1f", "ctrl+_", false],
	["\x1f", "ctrl+-", false],
	["\x1b\x1b", "ctrl+alt+[", false],
	["\x1b\x1c", "ctrl+alt+\\", false],
	["\x1b\x1d", "ctrl+alt+]", false],
	["\x1b\x1f", "ctrl+alt+_", false],
	["\x1b\x1f", "ctrl+alt+-", false],
	["\x1b ", "alt+space", false],
	["\x1b\b", "alt+backspace", false],
	["\x1b\x03", "ctrl+alt+c", false],
	["\x1bB", "alt+left", false],
	["\x1bF", "alt+right", false],
	["\x1ba", "alt+a", false],
	["\x1b1", "alt+1", false],
	["\x1b,", "alt+,", false],
	["\x1b.", "alt+.", false],
	["\x1by", "alt+y", false],
	["\x1bz", "alt+z", false],
	["\x1b[A", "up", false],
	["\x1b[B", "down", false],
	["\x1b[C", "right", false],
	["\x1b[D", "left", false],
	["\x1bOA", "up", false],
	["\x1bOB", "down", false],
	["\x1bOC", "right", false],
	["\x1bOD", "left", false],
	["\x1bOH", "home", false],
	["\x1bOF", "end", false],
	["\x1bOP", "f1", false],
	["\x1b[24~", "f12", false],
	["\x1b[E", "clear", false],
	["\x1bp", "alt+up", false],
	["\x1bp", "up", false],
	["\x1b[a", "shift+up", false],
	["\x1bOa", "ctrl+up", false],
	["\x1b[2$", "shift+insert", false],
	["\x1b[2^", "ctrl+insert", false],
	["\x1b[7$", "shift+home", false],
	["1", "1", false],
	// Legacy alt-prefixed sequences with kitty active (kitty=true)
	["\x1b ", "alt+space", true],
	["\x1b\b", "alt+backspace", true],
	["\x1b\x03", "ctrl+alt+c", true],
	["\x1bB", "alt+left", true],
	["\x1bF", "alt+right", true],
	["\x1ba", "alt+a", true],
	["\x1b1", "alt+1", true],
	["\x1b,", "alt+,", true],
	["\x1b.", "alt+.", true],
	["\x1by", "alt+y", true],
	// linefeed as shift+enter when kitty active
	["\n", "shift+enter", true],
	["\n", "enter", true],
];
for (const [input, keyId, kitty] of explicitMatches) {
	setKittyProtocolActive(kitty);
	setEnvForWt(false);
	matchesVectors.push({ input, keyId, kitty, wt: false, expected: matchesKey(input, keyId) });
}
setKittyProtocolActive(false);

// Explicit vectors from keys.test.ts that depend on Windows-Terminal env.
const wtCases = [
	// raw 0x08 / 0x7f behaviour, kitty inactive
	["\x7f", "backspace", false, false, true],
	["\x7f", "ctrl+backspace", false, false, false],
	["\x08", "backspace", false, false, true],
	["\x08", "ctrl+backspace", false, false, false],
	["\x08", "ctrl+h", false, false, true],
	["\x08", "ctrl+backspace", false, true, true],
	["\x08", "backspace", false, true, false],
	["\x08", "ctrl+h", false, true, true],
	["\x7f", "backspace", false, true, true],
];
for (const [input, keyId, kitty, wt, expected] of wtCases) {
	setKittyProtocolActive(kitty);
	setEnvForWt(wt);
	const got = matchesKey(input, keyId);
	if (got !== expected) throw new Error(`wt matchesKey mismatch: ${JSON.stringify(input)} ${keyId} wt=${wt} => ${got}`);
	matchesVectors.push({ input, keyId, kitty, wt, expected: got });
}
setKittyProtocolActive(false);
setEnvForWt(false);
dump("keys_matches_key", matchesVectors);

// --- parseKey sweep ------------------------------------------------------
const parseVectors = [];
for (const kitty of [false, true]) {
	setKittyProtocolActive(kitty);
	setEnvForWt(false);
	for (const input of dataList) {
		const r = parseKey(input);
		parseVectors.push({ input, kitty, wt: false, expected: r === undefined ? null : r });
	}
}
setKittyProtocolActive(false);
// Explicit parseKey inputs transcribed from keys.test.ts (kitty flag per case).
const explicitParse = [
	// Kitty protocol / CSI-u (kitty=true)
	["\x1b[107;9u", true],
	["\x1b[13;9u", true],
	["\x1b[107;13u", true],
	["\x1b[107;14u", true],
	["\x1b[49u", true],
	["\x1b[49;5u", true],
	["\x1b[57399u", true],
	["\x1b[57409u", true],
	["\x1b[57413u", true],
	["\x1b[57416u", true],
	["\x1b[57417u", true],
	["\x1b[57418u", true],
	["\x1b[57419u", true],
	["\x1b[57420u", true],
	["\x1b[57421u", true],
	["\x1b[57422u", true],
	["\x1b[57423u", true],
	["\x1b[57424u", true],
	["\x1b[57425u", true],
	["\x1b[57426u", true],
	["\x1b[1089::99;5u", true],
	["\x1b[107::118;5u", true],
	["\x1b[47::91;5u", true],
	["\x1b[99;5u", true],
	["\x1b[69;2u", true],
	["\x1b[99;17u", true],
	// modifyOtherKeys parse (kitty=false)
	["\x1b[27;5;99~", false],
	["\x1b[27;5;100~", false],
	["\x1b[27;5;122~", false],
	["\x1b[27;5;13~", false],
	["\x1b[27;2;13~", false],
	["\x1b[27;3;13~", false],
	["\x1b[27;2;9~", false],
	["\x1b[27;5;9~", false],
	["\x1b[27;3;9~", false],
	["\x1b[27;1;127~", false],
	["\x1b[27;5;127~", false],
	["\x1b[27;3;127~", false],
	["\x1b[27;1;27~", false],
	["\x1b[27;1;32~", false],
	["\x1b[27;5;32~", false],
	["\x1b[27;5;47~", false],
	["\x1b[27;5;49~", false],
	["\x1b[27;2;49~", false],
	["\x1b[27;2;69~", false],
	["\x1b[27;6;69~", false],
	["\x1b[104;7u", false],
	["\x1b[27;7;104~", false],
	// Legacy parse (kitty=false)
	["\x03", false],
	["\x04", false],
	["\x1b", false],
	["\t", false],
	["\r", false],
	["\n", false],
	["\x00", false],
	[" ", false],
	["1", false],
	["\x1c", false],
	["\x1d", false],
	["\x1f", false],
	["\x1b\x1b", false],
	["\x1b\x1c", false],
	["\x1b\x1d", false],
	["\x1b\x1f", false],
	["\x1b ", false],
	["\x1b\b", false],
	["\x1b\x03", false],
	["\x1bB", false],
	["\x1bF", false],
	["\x1ba", false],
	["\x1b1", false],
	["\x1b,", false],
	["\x1b.", false],
	["\x1by", false],
	["\x1bz", false],
	["\x1b[A", false],
	["\x1b[B", false],
	["\x1b[C", false],
	["\x1b[D", false],
	["\x1bOA", false],
	["\x1bOB", false],
	["\x1bOC", false],
	["\x1bOD", false],
	["\x1bOH", false],
	["\x1bOF", false],
	["\x1bOP", false],
	["\x1b[24~", false],
	["\x1b[E", false],
	["\x1b[2^", false],
	["\x1bp", false],
	["\x1b[[5~", false],
	// Legacy alt-prefixed parse with kitty active (kitty=true) -> undefined
	["\x1b ", true],
	["\x1b\b", true],
	["\x1b\x03", true],
	["\x1bB", true],
	["\x1bF", true],
	["\x1ba", true],
	["\x1b1", true],
	["\x1b,", true],
	["\x1b.", true],
	["\x1by", true],
	["\n", true],
];
for (const [input, kitty] of explicitParse) {
	setKittyProtocolActive(kitty);
	setEnvForWt(false);
	const r = parseKey(input);
	parseVectors.push({ input, kitty, wt: false, expected: r === undefined ? null : r });
}
setKittyProtocolActive(false);
// Windows-Terminal-dependent parseKey cases.
for (const [input, kitty, wt] of [
	["\x08", false, false],
	["\x08", false, true],
	["\x7f", false, false],
	["\x7f", false, true],
]) {
	setKittyProtocolActive(kitty);
	setEnvForWt(wt);
	const r = parseKey(input);
	parseVectors.push({ input, kitty, wt, expected: r === undefined ? null : r });
}
setKittyProtocolActive(false);
setEnvForWt(false);
dump("keys_parse_key", parseVectors);

// --- decodeKittyPrintable / decodePrintableKey (state independent) -------
const decodeInputs = new Set();
for (let cp = 57399; cp <= 57426; cp++) decodeInputs.add(`\x1b[${cp}u`);
for (const cp of [97, 65, 49, 32, 13, 27, 9, 127, 196, 99, 47, 33, 233]) {
	decodeInputs.add(`\x1b[${cp}u`);
	for (const m of [1, 2, 3, 5, 6, 9]) decodeInputs.add(`\x1b[${cp};${m}u`);
}
decodeInputs.add("\x1b[99:67:99;2u");
decodeInputs.add("\x1b[97:65;2u");
decodeInputs.add("\x1b[97u");
decodeInputs.add("\x1b[not-a-seq");
decodeInputs.add("\x03");
decodeInputs.add("a");
const decodeKittyVectors = [...decodeInputs].map((input) => {
	const r = decodeKittyPrintable(input);
	return { input, expected: r === undefined ? null : r };
});
dump("keys_decode_kitty_printable", decodeKittyVectors);

const decodePrintableInputs = new Set(decodeInputs);
for (const [m, code] of [
	[2, 69],
	[2, 196],
	[2, 32],
	[2, 13],
	[6, 69],
	[1, 97],
	[3, 97],
	[2, 65],
	[5, 99],
]) {
	decodePrintableInputs.add(`\x1b[27;${m};${code}~`);
}
const decodePrintableVectors = [...decodePrintableInputs].map((input) => {
	const r = decodePrintableKey(input);
	return { input, expected: r === undefined ? null : r };
});
dump("keys_decode_printable_key", decodePrintableVectors);

// --- isKeyRelease / isKeyRepeat (state independent substring scans) ------
const eventInputs = new Set();
for (const t of ["u", "~", "A", "B", "C", "D", "H", "F"]) {
	eventInputs.add(`\x1b[99;5:2${t}`);
	eventInputs.add(`\x1b[99;5:3${t}`);
	eventInputs.add(`\x1b[99;5:1${t}`);
}
eventInputs.add("\x1b[99;5u");
eventInputs.add("\x1b[99u");
eventInputs.add("\x03");
eventInputs.add("\x1b[200~90:62:3F:A5\x1b[201~");
eventInputs.add("\x1b[200~foo:2Ubar\x1b[201~");
eventInputs.add("\x1b[1;2:3A");
eventInputs.add("\x1b[3;5:2~");
const releaseVectors = [...eventInputs].map((input) => ({ input, expected: isKeyRelease(input) }));
dump("keys_is_key_release", releaseVectors);
const repeatVectors = [...eventInputs].map((input) => ({ input, expected: isKeyRepeat(input) }));
dump("keys_is_key_repeat", repeatVectors);

// --- isWindowsTerminalSession env derivation -----------------------------
// Directly pins the WT_SESSION / SSH_* logic. We re-check the derived boolean
// via matchesRawBackspace's observable behaviour: with kitty inactive,
// matchesKey("\x08", "ctrl+backspace") is true iff isWindowsTerminalSession().
const wtEnvCases = [
	{ WT_SESSION: null, SSH_CONNECTION: null, SSH_CLIENT: null, SSH_TTY: null },
	{ WT_SESSION: "test-session", SSH_CONNECTION: null, SSH_CLIENT: null, SSH_TTY: null },
	{ WT_SESSION: "test-session", SSH_CONNECTION: "1 2 3 4", SSH_CLIENT: null, SSH_TTY: null },
	{ WT_SESSION: "test-session", SSH_CONNECTION: null, SSH_CLIENT: "1 2 3", SSH_TTY: null },
	{ WT_SESSION: "test-session", SSH_CONNECTION: null, SSH_CLIENT: null, SSH_TTY: "/dev/pts/1" },
	{ WT_SESSION: "", SSH_CONNECTION: null, SSH_CLIENT: null, SSH_TTY: null },
	{ WT_SESSION: null, SSH_CONNECTION: "1 2 3 4", SSH_CLIENT: null, SSH_TTY: null },
];
setKittyProtocolActive(false);
const wtEnvVectors = wtEnvCases.map((env) => {
	setEnvRaw(env);
	const expected = matchesKey("\x08", "ctrl+backspace");
	return { env, expected };
});
setEnvForWt(false);
dump("keys_windows_terminal_session", wtEnvVectors);

console.log(`\nTOTAL KEYS VECTORS: ${total}`);
