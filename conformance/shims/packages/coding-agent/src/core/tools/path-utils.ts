// straitjacket-allow-file:duplication — the HYBRID `resolveReadPath` below is a
// faithful line-by-line mirror of pi's original fallback ladder (resolved →
// AM/PM → NFD → curly → NFD+curly), because the Rust port takes an injected
// `exists` closure instead of touching the filesystem; the shim must rebuild
// pi's sync `accessSync` probe over that ordering, so the structural overlap
// with the preserved pi original is intentional and load-bearing.
//
// Native shim for packages/coding-agent/src/core/tools/path-utils.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved alongside as `path-utils.__pi_original__.ts` and this
// shim takes its place, so pi's tools (and test/path-utils.test.ts, which
// deep-imports this module and probes real temp dirs) hit Rust.
//
// Scope of the native flip (HYBRID / partial): the pure `expandPath` and
// `resolveToCwd`, ported to `pidgin_coding::core::tools::path_utils`. The Rust
// port returns a `Result`; the addon maps `Err` to a thrown JS error, which
// preserves pi's string-return + throw-on-bad-input contract. `resolveReadPath`
// is rebuilt here: the Rust port takes an injected `exists` closure rather than
// touching the filesystem, so this shim supplies a real `accessSync`-backed
// probe (pi's sync fs behavior) and drives the native macOS filename transforms
// (`tryMacOSScreenshotPath`/`tryNFDVariant`/`tryCurlyQuoteVariant`, private in
// pi) in pi's fallback order. The async `pathExists`/`resolveReadPathAsync` are
// NOT ported and are re-exported unchanged from the original.

export * from "./path-utils.__pi_original__.ts";

import { accessSync, constants } from "node:fs";
import {
	expandPath as nativeExpandPath,
	pathTryCurlyQuoteVariant as nativeTryCurlyQuoteVariant,
	pathTryMacosScreenshotPath as nativeTryMacosScreenshotPath,
	pathTryNfdVariant as nativeTryNfdVariant,
	resolveToCwd as nativeResolveToCwd,
} from "pidgin-napi";

export function expandPath(filePath: string): string {
	return nativeExpandPath(filePath);
}

export function resolveToCwd(filePath: string, cwd: string): string {
	return nativeResolveToCwd(filePath, cwd);
}

function fileExists(filePath: string): boolean {
	try {
		accessSync(filePath, constants.F_OK);
		return true;
	} catch {
		return false;
	}
}

export function resolveReadPath(filePath: string, cwd: string): string {
	const resolved = resolveToCwd(filePath, cwd);

	if (fileExists(resolved)) {
		return resolved;
	}

	// Try macOS AM/PM variant (narrow no-break space before AM/PM)
	const amPmVariant = nativeTryMacosScreenshotPath(resolved);
	if (amPmVariant !== resolved && fileExists(amPmVariant)) {
		return amPmVariant;
	}

	// Try NFD variant (macOS stores filenames in NFD form)
	const nfdVariant = nativeTryNfdVariant(resolved);
	if (nfdVariant !== resolved && fileExists(nfdVariant)) {
		return nfdVariant;
	}

	// Try curly quote variant (macOS uses U+2019 in screenshot names)
	const curlyVariant = nativeTryCurlyQuoteVariant(resolved);
	if (curlyVariant !== resolved && fileExists(curlyVariant)) {
		return curlyVariant;
	}

	// Try combined NFD + curly quote (for French macOS screenshots like "Capture d'écran")
	const nfdCurlyVariant = nativeTryCurlyQuoteVariant(nfdVariant);
	if (nfdCurlyVariant !== resolved && fileExists(nfdCurlyVariant)) {
		return nfdCurlyVariant;
	}

	return resolved;
}
