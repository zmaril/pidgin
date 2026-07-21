// Native shim for packages/coding-agent/src/utils/paths.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `paths.__pi_original__.ts` and this shim takes
// its place, so pi's callers (and test/paths.test.ts) hit Rust.
//
// Scope of the native flip (pure): the lexical path transforms `canonicalizePath`,
// `isLocalPath`, `normalizePath`, `resolvePath`, and `getCwdRelativePath`, ported
// to `pidgin_coding::utils::paths`. These are pure path-string functions; the
// Rust code runs in-process, so its `std::env::current_dir` / `$HOME` reads
// observe the same cwd/home Node does.
//
// Two contract details are preserved:
//   * `resolvePath`'s JS default `baseDir = process.cwd()` is re-added HERE and
//     passed explicitly into the native call (the Rust fn has no default arg).
//   * `normalizePath`/`resolvePath` throw on a malformed `file://` URL (the Rust
//     `Err` maps to a thrown JS error). `getCwdRelativePath` returns
//     `string | undefined` in pi; napi marshals the Rust `None` to JS `null`, so
//     the shim remaps `null → undefined` to match pi's exact return type.
//
// `markPathIgnoredByCloudSync` is a side-effecting `xattr`/`setfattr` shell-out,
// NOT a pure transform — it is intentionally left delegated to pi's original via
// the `export *` re-export below and is not ported.

export * from "./paths.__pi_original__.ts";

import type { PathInputOptions } from "./paths.__pi_original__.ts";
import {
	canonicalizePath as nativeCanonicalizePath,
	getCwdRelativePath as nativeGetCwdRelativePath,
	isLocalPath as nativeIsLocalPath,
	normalizePath as nativeNormalizePath,
	resolvePath as nativeResolvePath,
} from "pidgin-napi";

export function canonicalizePath(path: string): string {
	return nativeCanonicalizePath(path);
}

export function isLocalPath(value: string): boolean {
	return nativeIsLocalPath(value);
}

export function normalizePath(input: string, options: PathInputOptions = {}): string {
	return nativeNormalizePath(input, options);
}

export function resolvePath(input: string, baseDir: string = process.cwd(), options: PathInputOptions = {}): string {
	return nativeResolvePath(input, baseDir, options);
}

export function getCwdRelativePath(filePath: string, cwd: string): string | undefined {
	// pi returns `string | undefined`; napi returns the Rust `None` as `null`.
	return nativeGetCwdRelativePath(filePath, cwd) ?? undefined;
}
