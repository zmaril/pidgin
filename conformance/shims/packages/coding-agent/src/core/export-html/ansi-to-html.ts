// Native shim for packages/coding-agent/src/core/export-html/ansi-to-html.ts,
// backed by the pidgin Rust addon (`pidgin-napi`). Installed by
// conformance/codegen.mjs when the module is marked `native` in
// conformance/manifest.json: the original pi file is preserved alongside as
// `ansi-to-html.__pi_original__.ts` and this shim takes its place, so pi's tests
// (and the sibling `tool-renderer.ts`, which stays original) import
// `./ansi-to-html.ts` unchanged and hit Rust.
//
// Scope of the native flip: `ansiToHtml` and `ansiLinesToHtml`, ported
// byte-for-byte to `pidgin_coding::core::export_html::ansi_to_html`. Any other
// export is re-exported unchanged from the original.

export * from "./ansi-to-html.__pi_original__.ts";

import { ansiLinesToHtml as nativeAnsiLinesToHtml, ansiToHtml as nativeAnsiToHtml } from "pidgin-napi";

export function ansiToHtml(text: string): string {
	return nativeAnsiToHtml(text);
}

export function ansiLinesToHtml(lines: string[]): string {
	return nativeAnsiLinesToHtml(lines);
}
