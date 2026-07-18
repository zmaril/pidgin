// Native shim for packages/tui/src/word-navigation.ts, backed by the atilla
// Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `word-navigation.__pi_original__.ts` and this shim
// takes its place, so pi's tests import `../src/word-navigation.ts` unchanged
// and hit Rust.
//
// Scope of the native flip: `findWordBackward`/`findWordForward` ported
// bit-exactly in `crates/atilla-tui` (validated against pi's
// word-navigation.test.ts). Cursors are UTF-16 string indices, as in pi. The
// native surface covers only the default `Intl.Segmenter` path; when a caller
// supplies `options.segment` or `options.isAtomicSegment` (JS callbacks that
// cannot cross the addon boundary) this shim delegates to pi's original,
// preserving full behavior. The `WordNavigationOptions` interface is
// re-exported from the original unchanged.

export * from "./word-navigation.__pi_original__.ts";

import {
	findWordBackward as nativeFindWordBackward,
	findWordForward as nativeFindWordForward,
} from "atilla-napi";
import {
	findWordBackward as originalFindWordBackward,
	findWordForward as originalFindWordForward,
	type WordNavigationOptions,
} from "./word-navigation.__pi_original__.ts";

function hasCustomSegmentation(options?: WordNavigationOptions): boolean {
	return !!(options && (options.segment || options.isAtomicSegment));
}

export function findWordBackward(text: string, cursor: number, options?: WordNavigationOptions): number {
	if (hasCustomSegmentation(options)) {
		return originalFindWordBackward(text, cursor, options);
	}
	if (cursor <= 0) return 0;
	return nativeFindWordBackward(text, cursor);
}

export function findWordForward(text: string, cursor: number, options?: WordNavigationOptions): number {
	if (hasCustomSegmentation(options)) {
		return originalFindWordForward(text, cursor, options);
	}
	if (cursor >= text.length) return text.length;
	return nativeFindWordForward(text, cursor);
}
