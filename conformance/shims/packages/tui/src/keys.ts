// Native shim for packages/tui/src/keys.ts, backed by the atilla Rust addon
// (`atilla-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `keys.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/keys.ts` unchanged and hit Rust.
//
// Scope of the native flip: the key parser ported bit-exactly in
// `crates/atilla-tui` (validated against vectors extracted from pi) —
// `parseKey`, `matchesKey`, `decodeKittyPrintable`, `decodePrintableKey`, and
// `setKittyProtocolActive`. The kitty-protocol flag lives in a Rust static, so
// the setter and the readers (`parseKey`/decoders) are overridden together and
// stay consistent within the single addon instance. Everything else — the `Key`
// runtime constant, the `KeyId`/`KeyEventType` types, `isKittyProtocolActive`,
// `isKeyRelease`, `isKeyRepeat` — is re-exported from the original unchanged, so
// the public runtime surface stays byte-for-byte pi's.

export * from "./keys.__pi_original__.ts";

import {
	decodeKittyPrintable as nativeDecodeKittyPrintable,
	decodePrintableKey as nativeDecodePrintableKey,
	matchesKey as nativeMatchesKey,
	parseKey as nativeParseKey,
	setKittyProtocolActive as nativeSetKittyProtocolActive,
} from "atilla-napi";
import type { KeyId } from "./keys.__pi_original__.ts";

export function parseKey(data: string): string | undefined {
	return nativeParseKey(data) ?? undefined;
}

export function matchesKey(data: string, keyId: KeyId): boolean {
	return nativeMatchesKey(data, keyId);
}

export function decodeKittyPrintable(data: string): string | undefined {
	return nativeDecodeKittyPrintable(data) ?? undefined;
}

export function decodePrintableKey(data: string): string | undefined {
	return nativeDecodePrintableKey(data) ?? undefined;
}

export function setKittyProtocolActive(active: boolean): void {
	nativeSetKittyProtocolActive(active);
}
