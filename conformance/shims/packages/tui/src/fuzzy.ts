// Native shim for packages/tui/src/fuzzy.ts, backed by the atilla Rust addon
// (`atilla-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `fuzzy.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/fuzzy.ts` unchanged and hit Rust.
//
// Scope of the native flip: the bespoke fuzzy scorer ported bit-exactly in
// `crates/atilla-tui` (validated against pi's fuzzy.test.ts). `fuzzyMatch` is
// backed by the Rust `fuzzyMatch`. `fuzzyFilter` takes a `getText` callback
// that cannot cross the addon boundary, so it is re-implemented here verbatim
// on top of the native `fuzzyMatch` — every scoring decision still runs through
// Rust. The `FuzzyMatch` interface is re-exported from the original unchanged.

export * from "./fuzzy.__pi_original__.ts";

import { fuzzyMatch as nativeFuzzyMatch } from "atilla-napi";
import type { FuzzyMatch } from "./fuzzy.__pi_original__.ts";

export function fuzzyMatch(query: string, text: string): FuzzyMatch {
	return nativeFuzzyMatch(query, text);
}

/**
 * Filter and sort items by fuzzy match quality (best matches first).
 * Supports whitespace- and slash-separated tokens: all tokens must match.
 *
 * Re-implemented from pi's original on top of the native `fuzzyMatch`.
 */
export function fuzzyFilter<T>(items: T[], query: string, getText: (item: T) => string): T[] {
	if (!query.trim()) {
		return items;
	}

	const tokens = query
		.trim()
		.split(/[\s/]+/)
		.filter((t) => t.length > 0);

	if (tokens.length === 0) {
		return items;
	}

	const results: { item: T; totalScore: number }[] = [];

	for (const item of items) {
		const text = getText(item);
		let totalScore = 0;
		let allMatch = true;

		for (const token of tokens) {
			const match = fuzzyMatch(token, text);
			if (match.matches) {
				totalScore += match.score;
			} else {
				allMatch = false;
				break;
			}
		}

		if (allMatch) {
			results.push({ item, totalScore });
		}
	}

	results.sort((a, b) => a.totalScore - b.totalScore);
	return results.map((r) => r.item);
}
