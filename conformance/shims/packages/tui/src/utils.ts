// Native shim for packages/tui/src/utils.ts, backed by the pidgin Rust addon
// (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `utils.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/utils.ts` unchanged and hit Rust.
//
// Scope of the native flip: the width layer ported bit-exactly in
// `crates/pidgin-tui` (validated against 8029 vectors extracted from pi) —
// `visibleWidth`, `normalizeTerminalOutput`, `truncateToWidth`,
// `wrapTextWithAnsi`, `sliceWithWidth`, and `extractSegments`. Every other
// export (segmenters, regexes, whitespace/punctuation helpers,
// `applyBackgroundToLine`, `extractAnsiCode`, `sliceByColumn`) is re-exported
// from the original unchanged. The public runtime surface therefore stays
// byte-for-byte pi's — required because `tab-width.test.ts` drives pi's real
// renderer through this module and pi crashes on any width mismatch.

export * from "./utils.__pi_original__.ts";

import {
	extractSegments as nativeExtractSegments,
	normalizeTerminalOutput as nativeNormalizeTerminalOutput,
	sliceWithWidth as nativeSliceWithWidth,
	truncateToWidth as nativeTruncateToWidth,
	visibleWidth as nativeVisibleWidth,
	wrapTextWithAnsi as nativeWrapTextWithAnsi,
} from "pidgin-napi";

export function visibleWidth(str: string): number {
	return nativeVisibleWidth(str);
}

export function normalizeTerminalOutput(str: string): string {
	return nativeNormalizeTerminalOutput(str);
}

export function truncateToWidth(
	text: string,
	maxWidth: number,
	ellipsis = "...",
	pad = false,
): string {
	return nativeTruncateToWidth(text, maxWidth, ellipsis, pad);
}

export function wrapTextWithAnsi(text: string, width: number): string[] {
	return nativeWrapTextWithAnsi(text, width);
}

export function sliceWithWidth(
	line: string,
	startCol: number,
	length: number,
	strict = false,
): { text: string; width: number } {
	return nativeSliceWithWidth(line, startCol, length, strict);
}

export function extractSegments(
	line: string,
	beforeEnd: number,
	afterStart: number,
	afterLen: number,
	strictAfter = false,
): { before: string; beforeWidth: number; after: string; afterWidth: number } {
	return nativeExtractSegments(line, beforeEnd, afterStart, afterLen, strictAfter);
}
