//! Node-API bridge for atilla, built with napi-rs as a `cdylib` addon.
//!
//! This crate exposes the Rust engine to JavaScript. Today it carries a single
//! placeholder export that proves the addon compiles, links, and loads from
//! Node; the real bindings land as the engine surface fills in.

use napi_derive::napi;

/// Returns the crate version. Proves the native addon builds and loads.
///
/// Exported to JavaScript as `atillaNativeVersion`.
#[napi(js_name = "atillaNativeVersion")]
pub fn atilla_native_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
