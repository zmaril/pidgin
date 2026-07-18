// Native shim for packages/coding-agent/src/utils/ansi.ts, backed by the atilla
// Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `ansi.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/utils/ansi.ts` unchanged and hit Rust.
//
// Scope of the native flip: `stripAnsi`, ported to `atilla_coding::utils::ansi`.
// The non-string `TypeError` guard has no Rust equivalent (the Rust type system
// guarantees a `&str`), so it is reproduced here in the shim to keep pi's
// `TypeError`-on-non-string behavior byte-for-byte. Everything else in the
// module is re-exported unchanged.

export * from "./ansi.__pi_original__.ts";

import { stripAnsi as nativeStripAnsi } from "atilla-napi";

export function stripAnsi(value: string): string {
	if (typeof value !== "string") {
		throw new TypeError(`Expected a \`string\`, got \`${typeof value}\``);
	}
	return nativeStripAnsi(value);
}
