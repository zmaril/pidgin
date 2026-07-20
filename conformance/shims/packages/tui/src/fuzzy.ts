// Native shim for packages/tui/src/fuzzy.ts, backed by the pidgin Rust addon
// (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `fuzzy.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/fuzzy.ts` unchanged and hit Rust.
//
// Scope of the native flip: the bespoke fuzzy scorer ported bit-exactly in
// `crates/pidgin-tui` (validated against pi's fuzzy.test.ts). `fuzzyMatch` is
// backed by the Rust `fuzzyMatch`. `fuzzyFilter` delegates the WHOLE filter to
// the Rust `fuzzyFilter`, which takes the materialized item texts plus the
// query and returns the surviving items' original indices, ranked. pi's
// `getText` callback cannot cross the addon boundary, so the shim materializes
// texts with it in JS, hands them to Rust, and maps the returned indices back —
// every scoring, gating, and sort decision runs through Rust. The `FuzzyMatch`
// interface is re-exported from the original unchanged.

export * from "./fuzzy.__pi_original__.ts";

import { fuzzyFilter as nativeFuzzyFilter, fuzzyMatch as nativeFuzzyMatch } from "pidgin-napi";
import type { FuzzyMatch } from "./fuzzy.__pi_original__.ts";

export function fuzzyMatch(query: string, text: string): FuzzyMatch {
	return nativeFuzzyMatch(query, text);
}

/**
 * Filter and sort items by fuzzy match quality (best matches first).
 * Supports whitespace- and slash-separated tokens: all tokens must match.
 *
 * Delegates the whole filter to the native `fuzzyFilter`: materialize each
 * item's text via `getText`, let Rust rank the original indices, then map those
 * indices back to items.
 */
export function fuzzyFilter<T>(items: T[], query: string, getText: (item: T) => string): T[] {
	const texts = items.map(getText);
	const indices = nativeFuzzyFilter(texts, query);
	return indices.map((i) => items[i]!);
}
