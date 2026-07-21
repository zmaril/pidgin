// Native shim for packages/ai/src/utils/error-body.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `error-body.__pi_original__.ts` and this shim takes its place, so
// pi's tests import `../src/utils/error-body.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's provider HTTP error-body normalizer is ported
// bit-exactly in `crates/pidgin-ai` (`utils/error_body.rs`). Every decision the
// normalizer makes runs in Rust — the SDK-shape field-probe precedence (Mistral
// `statusCode`/`body` -> `openai` `status`/`error` -> `@google/genai` `status` ->
// AWS Bedrock `$metadata`/`$response`), the empty-object-is-no-body rule, the
// UTF-16 truncation cap, the `messageCarriesBody` flag, and the
// `formatProviderError` compose rules. `normalizeProviderError`,
// `formatProviderError`, and `truncateErrorText` are overridden to delegate.
//
// The flip boundary: `normalizeProviderError(error: unknown)` catches an
// arbitrary JS value and branches on `instanceof Error`. That `unknown` unwrap is
// the ONE thing only the JS runtime can do — it cannot cross the addon boundary
// as a live object — so this shim keeps ONLY that plumbing in TS: it splits
// `Error` vs non-`Error` and plucks the candidate SDK carrier fields off the
// caught value into a plain JSON envelope. It makes NO normalization decisions
// (which field wins, whether a body is empty, every output string are all decided
// in Rust). The numeric/string carriers cross raw and are `typeof`-gated in the
// native layer exactly as pi gates them.
//
// pi's exported `safeJsonStringify` stays in TS (re-exported unchanged): it
// exists to absorb JS-runtime `JSON.stringify` edge cases (`undefined`,
// functions, circular refs, `toJSON`) over arbitrary values, which cannot be
// reproduced from Rust without a JS engine — an inherently JS-runtime boundary.
// `MAX_PROVIDER_ERROR_BODY_CHARS` and the `NormalizedProviderError` type are
// re-exported from the original unchanged.

export * from "./error-body.__pi_original__.ts";

import {
	formatProviderError as formatProviderErrorNative,
	normalizeProviderError as normalizeProviderErrorNative,
	truncateErrorText as truncateErrorTextNative,
} from "pidgin-napi";
import type { NormalizedProviderError } from "./error-body.__pi_original__.ts";

/**
 * Native `normalizeProviderError`: probe an SDK error object into a
 * `NormalizedProviderError`. See the file header for the flip boundary. The shim
 * splits `Error` vs non-`Error` and plucks the carrier fields (`statusCode`,
 * `status`, `body`, `error`, `$metadata.httpStatusCode`, `$response.statusCode`,
 * `$response.body`) into a JSON envelope; the whole field-probe precedence,
 * truncation, and `messageCarriesBody` computation run in Rust. Undefined
 * carriers are dropped by `JSON.stringify` and read back as absent in Rust.
 */
export function normalizeProviderError(error: unknown): NormalizedProviderError {
	let envelope: string;
	if (error instanceof Error) {
		const sdk = error as Error & {
			statusCode?: unknown;
			status?: unknown;
			body?: unknown;
			error?: unknown;
			$metadata?: { httpStatusCode?: unknown };
			$response?: { statusCode?: unknown; body?: unknown };
		};
		envelope = JSON.stringify({
			kind: "error",
			message: error.message,
			statusCode: sdk.statusCode,
			status: sdk.status,
			body: sdk.body,
			error: sdk.error,
			metadataHttpStatusCode: sdk.$metadata?.httpStatusCode,
			responseStatusCode: sdk.$response?.statusCode,
			responseBody: sdk.$response?.body,
		});
	} else {
		envelope = JSON.stringify({ kind: "other", value: error });
	}
	return JSON.parse(normalizeProviderErrorNative(envelope)) as NormalizedProviderError;
}

/**
 * Native `formatProviderError`: compose a display string from a normalized error
 * (optionally with a provider prefix). The compose rules run in Rust; the shim
 * only marshals the struct across as JSON.
 */
export function formatProviderError(norm: NormalizedProviderError, prefix?: string): string {
	return formatProviderErrorNative(JSON.stringify(norm), prefix);
}

/**
 * Native `truncateErrorText`: truncate `text` to `maxChars` UTF-16 code units,
 * appending pi's `... [truncated N chars]` suffix when it was over the cap.
 */
export function truncateErrorText(text: string, maxChars: number): string {
	return truncateErrorTextNative(text, maxChars);
}
