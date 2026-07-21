// Native shim for packages/coding-agent/src/core/experimental.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `experimental.__pi_original__.ts` and this shim
// takes its place, so pi's callers and test/experimental.test.ts import
// `../src/core/experimental.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's whole module is one function. The Rust port
// (`pidgin_coding::core::experimental`) owns the sole decision —
// `PI_EXPERIMENTAL === "1"` — reading the process environment live via
// `std::env::var` at call time. The addon shares the JS process's environment
// table, so `process.env.PI_EXPERIMENTAL = …` mutations (as the test performs)
// are observed by the next native call.

export * from "./experimental.__pi_original__.ts";

import { areExperimentalFeaturesEnabled as nativeAreExperimentalFeaturesEnabled } from "pidgin-napi";

export function areExperimentalFeaturesEnabled(): boolean {
	return nativeAreExperimentalFeaturesEnabled();
}
