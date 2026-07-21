// Native shim for packages/ai/src/api/simple-options.ts, backed by the pidgin
// Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module
// is marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `simple-options.__pi_original__.ts` and this shim takes
// its place, so pi's tests import `../src/api/simple-options.ts` unchanged and hit
// Rust.
//
// Scope of the native flip: pi's `maxTokens` context clamp
// (`clampMaxTokensToContext`) is ported bit-exactly in `crates/pidgin-ai`
// (`api/anthropic/simple_options.rs`, consuming its sibling `estimate.rs`). The
// `contextWindow − estimateContextTokens − CONTEXT_SAFETY_TOKENS` arithmetic, the
// `MIN_MAX_TOKENS` floor, the `min(maxTokens, …)` cap, and the zero-window
// short-circuit all run in Rust. `clampMaxTokensToContext` is overridden to
// delegate, and `buildBaseOptions` is overridden to route its `maxTokens` through
// the native clamp.
//
// The flip boundary: `clampMaxTokensToContext(model, context, maxTokens)` reads
// only `model.contextWindow` (a number) and a plain, fully-serializable `Context`,
// so the shim passes the window and the JSON-stringified context to Rust — nothing
// is projected away from the estimate. `buildBaseOptions`, by contrast, assembles
// a `StreamOptions` object that carries genuinely non-serializable JS values —
// `signal` (an `AbortSignal`), `onPayload` / `onResponse` (callbacks), and
// `transport` (a live object) — which cannot cross the addon boundary. So the shim
// keeps the whole options-object assembly in TS, exactly as pi builds it, and
// routes ONLY the numeric `maxTokens` clamp through native. `clampReasoning` and
// `adjustMaxTokensForThinking` are re-exported from the original unchanged (the
// test that drives this flip exercises `buildBaseOptions`).

export * from "./simple-options.__pi_original__.ts";

import { clampMaxTokensToContext as clampMaxTokensToContextNative } from "pidgin-napi";
import type {
	Api,
	Context,
	Model,
	SimpleStreamOptions,
	StreamOptions,
} from "../types.ts";

/**
 * Native `clampMaxTokensToContext`: clamp `maxTokens` to what the context window
 * can still hold after the estimated context tokens and the safety margin. pi
 * reads only `model.contextWindow`; the shim passes that window and the
 * JSON-stringified context to Rust, where the whole clamp (and the context-token
 * estimate it consumes) runs.
 */
export function clampMaxTokensToContext(
	model: Model<Api>,
	context: Context,
	maxTokens: number,
): number {
	return clampMaxTokensToContextNative(model.contextWindow, JSON.stringify(context), maxTokens);
}

/**
 * Native-backed `buildBaseOptions`: pi's base `StreamOptions` assembly, with only
 * the `maxTokens` clamp routed through native. The object is built in TS,
 * field-for-field as in pi's original, so the non-serializable JS values (`signal`,
 * `onPayload`, `onResponse`, `transport`) stay live and uncrossed.
 */
export function buildBaseOptions(
	model: Model<Api>,
	context: Context,
	options?: SimpleStreamOptions,
	apiKey?: string,
): StreamOptions {
	return {
		temperature: options?.temperature,
		maxTokens: clampMaxTokensToContext(model, context, options?.maxTokens ?? model.maxTokens),
		signal: options?.signal,
		apiKey: apiKey || options?.apiKey,
		transport: options?.transport,
		cacheRetention: options?.cacheRetention,
		sessionId: options?.sessionId,
		headers: options?.headers,
		onPayload: options?.onPayload,
		onResponse: options?.onResponse,
		timeoutMs: options?.timeoutMs,
		websocketConnectTimeoutMs: options?.websocketConnectTimeoutMs,
		maxRetries: options?.maxRetries,
		maxRetryDelayMs: options?.maxRetryDelayMs,
		metadata: options?.metadata,
		env: options?.env,
	};
}
