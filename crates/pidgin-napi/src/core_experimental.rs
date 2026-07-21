//! Node-API surface for experimental-feature gating (the single
//! `areExperimentalFeaturesEnabled` function).
//!
//! This exposes the Rust [`pidgin_coding::core::experimental`] port — a faithful
//! port of pi's `core/experimental.ts`
//! (`vendor/pi/packages/coding-agent/src/core/experimental.ts`) — to pi's
//! `packages/coding-agent` `experimental.test.ts`. pi's whole module is one
//! function: `process.env.PI_EXPERIMENTAL === "1"`.
//!
//! # The seam: the env read happens in Rust, at call time
//!
//! pi's test mutates `process.env.PI_EXPERIMENTAL` in-process between calls and
//! asserts the return value tracks each mutation. The Rust port reads the
//! variable with `std::env::var` *inside* the function body (never cached at
//! module load), and the addon runs in the same process as the JS test, sharing
//! one process environment table — so a JS-side `process.env.PI_EXPERIMENTAL =
//! …` is observed by the very next native call. This mirrors the already-native
//! `resolve-config-value` module, which reads the process environment through
//! `std::env::var` for the same reason. No state crosses the boundary and there
//! is nothing to marshal: the function takes no arguments and returns a `bool`.

use napi_derive::napi;

/// pi's `areExperimentalFeaturesEnabled`: whether experimental features are on
/// for this process, i.e. `PI_EXPERIMENTAL` is exactly `"1"`. The environment is
/// read live via `std::env::var` at call time, so in-process `process.env`
/// mutations made by the caller before this call are reflected.
#[napi(js_name = "areExperimentalFeaturesEnabled")]
pub fn are_experimental_features_enabled() -> bool {
    pidgin_coding::core::experimental::are_experimental_features_enabled()
}
