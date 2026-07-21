// Native shim for packages/ai/src/utils/retry.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `retry.__pi_original__.ts` and this shim takes its place, so pi's
// tests import `../src/utils/retry.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's provider-error retry classifier
// (`isRetryableAssistantError`) is ported bit-exactly in `crates/pidgin-ai`
// (`utils/retry.rs`). Every decision runs in Rust — the ordered
// non-retryable-limit-vs-retryable pattern tables and the "stop reason must be
// `error` and carry a non-empty message" gate. Only `isRetryableAssistantError`
// is overridden to delegate.
//
// The flip boundary: pi's `isRetryableAssistantError(message: AssistantMessage)`
// reads a plain, fully-serializable value — no closures, streams, or live object
// identity — so the shim marshals the whole message across honestly with
// `JSON.stringify` and the native layer deserializes the complete
// `AssistantMessage` before classifying. Nothing is projected away: the classifier
// receives exactly the message the caller passed. `undefined` optionals are
// dropped by `JSON.stringify` and read back as absent in Rust.

export * from "./retry.__pi_original__.ts";

import { isRetryableAssistantError as isRetryableAssistantErrorNative } from "pidgin-napi";
import type { AssistantMessage } from "../types.ts";

/**
 * Native `isRetryableAssistantError`: classify whether a failed assistant
 * message looks like a transient provider/transport error worth retrying. The
 * shim JSON-stringifies the whole message and delegates; the ordered pattern
 * tables and the stop-reason/error-message gate run in Rust.
 */
export function isRetryableAssistantError(message: AssistantMessage): boolean {
	return isRetryableAssistantErrorNative(JSON.stringify(message));
}
