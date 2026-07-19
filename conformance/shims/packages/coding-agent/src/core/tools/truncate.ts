// Native shim for packages/coding-agent/src/core/tools/truncate.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved alongside as `truncate.__pi_original__.ts` and this shim
// takes its place, so pi's read/bash tools (which import `./truncate.ts`
// unchanged) hit Rust. No test deep-imports truncate.ts; it is exercised
// transitively via test/tools.test.ts (read + bash blocks).
//
// Scope of the native flip: `formatSize`, `truncateHead`, `truncateTail`,
// `truncateLine`, ported to `pidgin_coding::core::tools::truncate`. The Rust
// port dropped pi's JS default arguments and required `TruncationOptions`
// fields, so this shim re-adds the `options = {}` / `maxChars` defaults and
// supplies the `DEFAULT_MAX_LINES`/`DEFAULT_MAX_BYTES`/`GREP_MAX_LINE_LENGTH`
// defaults before crossing. Structured results cross as JSON strings using pi's
// exact field names; the shim `JSON.parse`s them (the `TruncatedBy` enum +
// `Option` arriving as pi's `"lines" | "bytes" | null` union). The consts and
// the `TruncationResult`/`TruncationOptions` types are re-exported unchanged.

export * from "./truncate.__pi_original__.ts";

import {
	truncateFormatSize as nativeFormatSize,
	truncateHead as nativeTruncateHead,
	truncateLine as nativeTruncateLine,
	truncateTail as nativeTruncateTail,
} from "pidgin-napi";
import {
	DEFAULT_MAX_BYTES,
	DEFAULT_MAX_LINES,
	GREP_MAX_LINE_LENGTH,
	type TruncationOptions,
	type TruncationResult,
} from "./truncate.__pi_original__.ts";

export function formatSize(bytes: number): string {
	return nativeFormatSize(bytes);
}

export function truncateHead(content: string, options: TruncationOptions = {}): TruncationResult {
	const maxLines = options.maxLines ?? DEFAULT_MAX_LINES;
	const maxBytes = options.maxBytes ?? DEFAULT_MAX_BYTES;
	return JSON.parse(nativeTruncateHead(content, maxLines, maxBytes)) as TruncationResult;
}

export function truncateTail(content: string, options: TruncationOptions = {}): TruncationResult {
	const maxLines = options.maxLines ?? DEFAULT_MAX_LINES;
	const maxBytes = options.maxBytes ?? DEFAULT_MAX_BYTES;
	return JSON.parse(nativeTruncateTail(content, maxLines, maxBytes)) as TruncationResult;
}

export function truncateLine(
	line: string,
	maxChars: number = GREP_MAX_LINE_LENGTH,
): { text: string; wasTruncated: boolean } {
	return JSON.parse(nativeTruncateLine(line, maxChars)) as { text: string; wasTruncated: boolean };
}
