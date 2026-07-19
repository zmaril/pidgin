// Native shim for packages/coding-agent/src/utils/version-check.ts, backed by
// the pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved alongside as `version-check.__pi_original__.ts` and this
// shim takes its place, so pi's tests import `../src/utils/version-check.ts`
// unchanged and hit Rust.
//
// Scope of the native flip: the pure comparison helpers `comparePackageVersions`
// and `isNewerPackageVersion`, ported to `pidgin_coding::utils::version_check`.
// The native `comparePackageVersions` returns `null` for the incomparable case
// (napi `Option::None`); pi's signature is `number | undefined`, so the shim
// converts `null` to `undefined`. The HTTP-backed `getLatestPiRelease`,
// `getLatestPiVersion`, `checkForNewPiVersion`, and the `LatestPiRelease` type
// are re-exported unchanged from the original (their fetch-mock tests drive the
// original implementations).

export * from "./version-check.__pi_original__.ts";

import {
	comparePackageVersions as nativeComparePackageVersions,
	isNewerPackageVersion as nativeIsNewerPackageVersion,
} from "pidgin-napi";

export function comparePackageVersions(leftVersion: string, rightVersion: string): number | undefined {
	return nativeComparePackageVersions(leftVersion, rightVersion) ?? undefined;
}

export function isNewerPackageVersion(candidateVersion: string, currentVersion: string): boolean {
	return nativeIsNewerPackageVersion(candidateVersion, currentVersion);
}
