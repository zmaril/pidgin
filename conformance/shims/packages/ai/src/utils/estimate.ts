// Native shim for packages/ai/src/utils/estimate.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `estimate.__pi_original__.ts` and this shim takes its place, so
// pi's tests import `../src/utils/estimate.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's heuristic context-token accountant is ported
// bit-exactly in `crates/pidgin-ai` (`utils/estimate.rs`). Every arithmetic
// decision runs in Rust — the `ceil(chars / 4)` character heuristic over UTF-16
// code units, the flat 4800-char image estimate, the per-role/per-block message
// accounting, the most-recent-applicable-assistant-usage anchoring (including the
// stale-usage-behind-a-newer-message rule), and the system-prompt/tools prefix.
// `calculateContextTokens`, `estimateTextTokens`,
// `estimateTextAndImageContentTokens`, `estimateMessageTokens`, and
// `estimateContextTokens` are all overridden to delegate.
//
// The flip boundary: each estimator reads a plain, fully-serializable value (a
// `Usage`, a `Message`, a `string | Array<TextContent | ImageContent>`, or a
// `Context`) — no closures, streams, or live object identity — so the shim
// marshals the argument honestly with `JSON.stringify` and the native layer
// deserializes the complete value before estimating. Nothing is projected away.
// `estimateContextTokens` also accepts a bare `Message[]`; the shim wraps that as
// `{ messages }`, which the Rust port scores identically (no system prompt, no
// tools → zero prefix tokens), matching pi's `estimateMessages`-only path for an
// array argument. `undefined` optionals are dropped by `JSON.stringify` and read
// back as absent in Rust; `estimateContextTokens` returns its
// `ContextUsageEstimate` as JSON with `lastUsageIndex` emitted as `null` (not
// omitted) so `JSON.parse` round-trips it faithfully.
//
// The `ContextUsageEstimate` type is re-exported from the original unchanged.

export * from "./estimate.__pi_original__.ts";

import {
	calculateContextTokens as calculateContextTokensNative,
	estimateContextTokens as estimateContextTokensNative,
	estimateMessageTokens as estimateMessageTokensNative,
	estimateTextAndImageContentTokens as estimateTextAndImageContentTokensNative,
	estimateTextTokens as estimateTextTokensNative,
} from "pidgin-napi";
import type { Context, ImageContent, Message, TextContent, Usage } from "../types.ts";
import type { ContextUsageEstimate } from "./estimate.__pi_original__.ts";

/**
 * Native `calculateContextTokens`: `usage.totalTokens`, or the component sum when
 * `totalTokens` is zero (pi's `||` falsy-zero fallback). The shim JSON-stringifies
 * the `Usage`; the fallback decision runs in Rust.
 */
export function calculateContextTokens(usage: Usage): number {
	return calculateContextTokensNative(JSON.stringify(usage));
}

/** Native `estimateTextTokens`: `ceil(text.length / 4)` over UTF-16 code units. */
export function estimateTextTokens(text: string): number {
	return estimateTextTokensNative(text);
}

/**
 * Native `estimateTextAndImageContentTokens`: `ceil(chars / 4)` where text blocks
 * count their length and every other block counts as a flat 4800-char image. The
 * shim JSON-stringifies the content (a bare string or a block list) and delegates.
 */
export function estimateTextAndImageContentTokens(
	content: string | Array<TextContent | ImageContent>,
): number {
	return estimateTextAndImageContentTokensNative(JSON.stringify(content));
}

/**
 * Native `estimateMessageTokens`: the per-role/per-block character accounting for
 * a single message. The shim JSON-stringifies the whole message and delegates.
 */
export function estimateMessageTokens(message: Message): number {
	return estimateMessageTokensNative(JSON.stringify(message));
}

/**
 * Native `estimateContextTokens`: the full context estimate — usage anchoring,
 * added-tool accounting, and system-prompt/tools prefix. The shim marshals the
 * `Context` across as JSON (wrapping a bare `Message[]` as `{ messages }`, which
 * the port scores identically) and parses back the `ContextUsageEstimate`.
 */
export function estimateContextTokens(
	context: Context | readonly Message[],
): ContextUsageEstimate {
	const envelope = Array.isArray(context) ? { messages: context } : context;
	return JSON.parse(estimateContextTokensNative(JSON.stringify(envelope))) as ContextUsageEstimate;
}
