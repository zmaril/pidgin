// Native shim for packages/coding-agent/src/core/http-dispatcher.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved alongside as `http-dispatcher.__pi_original__.ts` and
// this shim takes its place, so pi's idle-timeout helpers hit Rust.
//
// Scope of the native flip: the pure idle-timeout parse/format helpers,
// `parseHttpIdleTimeoutMs` and `formatHttpIdleTimeoutMs`, ported to
// `pidgin_coding::core::http_dispatcher`. pi's undici global-dispatcher install
// plumbing (`applyHttpProxySettings`, `configureHttpDispatcher`, the undici
// factories) and the `DEFAULT_HTTP_IDLE_TIMEOUT_MS` / `HTTP_IDLE_TIMEOUT_CHOICES`
// constants are NOT ported and continue to come from the preserved original via
// the re-export below.

export * from "./http-dispatcher.__pi_original__.ts";

import {
	formatHttpIdleTimeoutMs as nativeFormatHttpIdleTimeoutMs,
	parseHttpIdleTimeoutMsFromNumber as nativeParseHttpIdleTimeoutMsFromNumber,
	parseHttpIdleTimeoutMsFromString as nativeParseHttpIdleTimeoutMsFromString,
} from "pidgin-napi";

// Reproduce pi's `parseHttpIdleTimeoutMs(value: unknown)` dispatch exactly: a
// string goes through the string branch (handling "disabled"/empty/numeric), a
// number through the numeric branch, anything else (undefined/null/…) is
// `undefined`. The native fns return `number | null`; `?? undefined` maps the
// `null` (Rust `None`) to `undefined` while preserving a genuine `0`.
export function parseHttpIdleTimeoutMs(value: unknown): number | undefined {
	if (typeof value === "string") {
		return nativeParseHttpIdleTimeoutMsFromString(value) ?? undefined;
	}
	if (typeof value === "number") {
		return nativeParseHttpIdleTimeoutMsFromNumber(value) ?? undefined;
	}
	return undefined;
}

export function formatHttpIdleTimeoutMs(timeoutMs: number): string {
	return nativeFormatHttpIdleTimeoutMs(timeoutMs);
}
