// Native shim for packages/coding-agent/src/core/tools/edit-diff.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved alongside as `edit-diff.__pi_original__.ts` and this shim
// takes its place, so pi's edit tool (src/core/tools/edit.ts) and the
// index.ts re-exports import `./edit-diff.ts` unchanged and hit Rust. Exercised
// by test/tools.test.ts (edit block, including the jsdiff `applyPatch`
// round-trip on the generated unified patch) and test/edit-tool-no-full-redraw.
//
// Scope of the native flip: the 10 sync pure functions `detectLineEnding`,
// `normalizeToLF`, `restoreLineEndings`, `normalizeForFuzzyMatch`,
// `fuzzyFindText`, `stripBom`, `applyReplacementsPreservingUnchangedLines`,
// `applyEditsToNormalizedContent`, `generateUnifiedPatch`, `generateDiffString`,
// ported to `pidgin_coding::core::tools::edit_diff`. The async
// `computeEditsDiff`/`computeEditDiff` are NOT ported and are re-exported
// unchanged from the original (they call the original file's own internal pure
// helpers, so they keep running pi's TypeScript).
//
// Marshaling caveats handled here: the Rust port dropped pi's `contextLines = 4`
// default on `generateUnifiedPatch`/`generateDiffString`, so this shim re-adds
// it. The `LineEnding` enum crosses as pi's `"\r\n" | "\n"` union. Structured
// results cross as JSON strings with pi's exact field names (`stripBom` →
// `{ bom, text }`, `fuzzyFindText` → `FuzzyMatchResult`, `generateDiffString` →
// `{ diff, firstChangedLine }` with `null` mapped back to `undefined`). Match
// failures throw, matching pi.

export * from "./edit-diff.__pi_original__.ts";

import {
	applyEditsToNormalizedContent as nativeApplyEdits,
	applyReplacementsPreservingUnchangedLines as nativeApplyReplacements,
	detectLineEnding as nativeDetectLineEnding,
	fuzzyFindText as nativeFuzzyFindText,
	generateDiffString as nativeGenerateDiffString,
	generateUnifiedPatch as nativeGenerateUnifiedPatch,
	normalizeForFuzzyMatch as nativeNormalizeForFuzzyMatch,
	normalizeToLf as nativeNormalizeToLf,
	restoreLineEndings as nativeRestoreLineEndings,
	stripBom as nativeStripBom,
} from "pidgin-napi";
import type {
	AppliedEditsResult,
	Edit,
	FuzzyMatchResult,
} from "./edit-diff.__pi_original__.ts";

export function detectLineEnding(content: string): "\r\n" | "\n" {
	return nativeDetectLineEnding(content) as "\r\n" | "\n";
}

export function normalizeToLF(text: string): string {
	return nativeNormalizeToLf(text);
}

export function restoreLineEndings(text: string, ending: "\r\n" | "\n"): string {
	return nativeRestoreLineEndings(text, ending);
}

export function normalizeForFuzzyMatch(text: string): string {
	return nativeNormalizeForFuzzyMatch(text);
}

export function fuzzyFindText(content: string, oldText: string): FuzzyMatchResult {
	return JSON.parse(nativeFuzzyFindText(content, oldText)) as FuzzyMatchResult;
}

export function stripBom(content: string): { bom: string; text: string } {
	return JSON.parse(nativeStripBom(content)) as { bom: string; text: string };
}

export function applyReplacementsPreservingUnchangedLines(
	originalContent: string,
	baseContent: string,
	replacements: Array<{ matchIndex: number; matchLength: number; newText: string }>,
): string {
	return nativeApplyReplacements(originalContent, baseContent, JSON.stringify(replacements));
}

export function applyEditsToNormalizedContent(
	normalizedContent: string,
	edits: Edit[],
	path: string,
): AppliedEditsResult {
	return JSON.parse(
		nativeApplyEdits(normalizedContent, JSON.stringify(edits), path),
	) as AppliedEditsResult;
}

export function generateUnifiedPatch(
	path: string,
	oldContent: string,
	newContent: string,
	contextLines = 4,
): string {
	return nativeGenerateUnifiedPatch(path, oldContent, newContent, contextLines);
}

export function generateDiffString(
	oldContent: string,
	newContent: string,
	contextLines = 4,
): { diff: string; firstChangedLine: number | undefined } {
	const result = JSON.parse(nativeGenerateDiffString(oldContent, newContent, contextLines)) as {
		diff: string;
		firstChangedLine: number | null;
	};
	return {
		diff: result.diff,
		firstChangedLine: result.firstChangedLine ?? undefined,
	};
}
