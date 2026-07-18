// straitjacket-allow-file:duplication — vector/table generator scaffolding; the
// per-property extraction blocks are intentionally parallel, not shared logic.
// Generate crates/atilla-tui/src/unicode_tables.rs by extracting Unicode
// property membership directly from the same V8/ICU engine pi runs on
// (Node 22). This guarantees the Rust port's property predicates match the
// exact Unicode version behind pi's `\p{...}` regexes, avoiding version skew
// against any third-party Rust unicode crate.
//
// Run from this directory:  node gen_unicode_tables.mjs && (cd ../../../.. && cargo fmt --all)
//
// Properties emitted (all tested with the `u` flag, matching utils.ts):
//   - Default_Ignorable_Code_Point  (zeroWidthRegex, leadingNonPrintingRegex)
//   - Control    = \p{Control}       (== gc=Cc)
//   - Mark       = \p{Mark}          (== gc=M: Mn|Mc|Me)
//   - Format     = \p{Format}        (== gc=Cf)  (leadingNonPrintingRegex only)
//   - Emoji, Emoji_Modifier, Emoji_Modifier_Base (structural RGI_Emoji matcher)
//   - RGI_SINGLE, RGI_VS16_BASE (exact per-codepoint RGI leaf decisions)
//
// Surrogates (\p{Surrogate}) are intentionally omitted: Rust &str cannot hold
// lone surrogates, so that class is unreachable in the port.

import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "..", "..", "src", "unicode_tables.rs");

const props = [
	["DEFAULT_IGNORABLE", /\p{Default_Ignorable_Code_Point}/u],
	["CONTROL", /\p{Control}/u],
	["MARK", /\p{Mark}/u],
	["FORMAT", /\p{Format}/u],
	["EMOJI", /\p{Emoji}/u],
	["EMOJI_MODIFIER", /\p{Emoji_Modifier}/u],
	["EMOJI_MODIFIER_BASE", /\p{Emoji_Modifier_Base}/u],
	// cjkBreakRegex: union of Script_Extensions for the CJK scripts that pi
	// breaks on when wrapping. Used by splitIntoTokensWithAnsi.
	[
		"CJK_BREAK",
		/[\p{Script_Extensions=Han}\p{Script_Extensions=Hiragana}\p{Script_Extensions=Katakana}\p{Script_Extensions=Hangul}\p{Script_Extensions=Bopomofo}]/u,
	],
];

function ranges(re) {
	const out = [];
	let start = -1;
	for (let i = 0; i <= 0x10ffff; i++) {
		// Skip surrogate scalars; they cannot exist in a Rust char.
		const isSurrogate = i >= 0xd800 && i <= 0xdfff;
		const match = !isSurrogate && re.test(String.fromCodePoint(i));
		if (match && start === -1) {
			start = i;
		} else if (!match && start !== -1) {
			out.push([start, i - 1]);
			start = -1;
		}
	}
	if (start !== -1) out.push([start, 0x10ffff]);
	return out;
}

let src = "";
src += "// Generated from Node 22's V8/ICU Unicode property data by\n";
src += "// crates/atilla-tui/vectors/gen/gen_unicode_tables.mjs. Do not edit by hand.\n";
src += "//\n";
src += "// Each table is a sorted, non-overlapping list of inclusive codepoint ranges\n";
src += "// for the named Unicode property, extracted from the same engine pi uses so\n";
src += "// the membership tests are bit-exact against pi's `\\p{...}` regexes.\n\n";

function emitRanges(name, rs) {
	src += `pub(crate) const ${name}: &[(u32, u32)] = &[\n`;
	for (const [s, e] of rs) {
		src += `    (0x${s.toString(16).toUpperCase()}, 0x${e.toString(16).toUpperCase()}),\n`;
	}
	src += "];\n\n";
	console.log(`${name}: ${rs.length} ranges`);
}

for (const [name, re] of props) {
	emitRanges(name, ranges(re));
}

// Leaf RGI_Emoji decisions extracted per codepoint from V8's v-flag
// `\p{RGI_Emoji}` sequence property. These pin the exact Basic_Emoji membership
// (single-codepoint emoji, and X+VS16 presentation sequences) that a structural
// grammar cannot reproduce without Unicode's curated sequence data. For example
// `#`, `*` and the digits are Emoji=Yes but `# U+FE0F` is NOT RGI (only the
// keycap `# U+FE0F U+20E3` is), so they must be excluded here.
const rgi = /^\p{RGI_Emoji}$/v;
function rgiRanges(makeSeq) {
	const out = [];
	let start = -1;
	for (let i = 0; i <= 0x10ffff; i++) {
		const isSurrogate = i >= 0xd800 && i <= 0xdfff;
		const match = !isSurrogate && rgi.test(makeSeq(i));
		if (match && start === -1) start = i;
		else if (!match && start !== -1) {
			out.push([start, i - 1]);
			start = -1;
		}
	}
	if (start !== -1) out.push([start, 0x10ffff]);
	return out;
}

// Codepoints that are a complete RGI emoji on their own.
emitRanges("RGI_SINGLE", rgiRanges((i) => String.fromCodePoint(i)));
// Codepoints X such that "X U+FE0F" is a complete RGI emoji (VS16 sequence).
emitRanges("RGI_VS16_BASE", rgiRanges((i) => String.fromCodePoint(i) + "️"));

writeFileSync(out, src);
console.log(`wrote ${out}`);
