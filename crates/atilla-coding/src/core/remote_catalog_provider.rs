// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `remote-catalog-provider.ts` pure helpers.
//! Persisted pi.dev catalog overlay for a static built-in provider.
//!
//! Ported from pi's `core/remote-catalog-provider.ts` at pinned commit
//! `3da591ab`.
//!
//! # Scope of this slice
//!
//! pi's `withRemoteCatalog(provider, baseUrl)` returns a new pi-ai `Provider`
//! that wraps the base provider, overriding `getModels()` to merge a
//! network-fetched dynamic catalog and adding a `refreshModels(context)`
//! closure. That closure reads/writes the [`ModelsStore`], performs an HTTP
//! `GET /api/models/providers/{id}` against pi.dev, honors the refresh TTL, the
//! abort signal, and the `force`/`allowNetwork` flags, and captures the fetched
//! models in provider-local state.
//!
//! **The stateful network overlay is deferred.** atilla-ai's [`RegistryProvider`]
//! exposes a `fetch_models: Fn(&RefreshContext) -> Vec<Model>` hook that receives
//! neither the [`ModelsStore`](super::models_store::ModelsStore) handle nor an
//! HTTP client / abort signal, so pi's store-backed, TTL-gated, cancelable
//! refresh cannot be represented without fabricating that surface. The HTTP
//! client itself is also unported (see `crates/atilla-ai` builtins, which route
//! via `ApiRouting::Unimplemented`). This module therefore ports the two pure,
//! credential-blind, synchronously-testable helpers the overlay is built from —
//! [`merge_models`] and [`parse_catalog`] — plus the refresh-interval constant.
//! When atilla-ai lands a store-and-network-aware refresh hook, the wrapper can
//! be assembled from these helpers with no change to their semantics.

use atilla_ai::types::Model;
use serde_json::Value;

/// Default catalog base URL (pi's `DEFAULT_CATALOG_BASE_URL`).
pub const DEFAULT_CATALOG_BASE_URL: &str = "https://pi.dev";

/// How long a persisted catalog stays fresh before a network re-check
/// (pi's `REMOTE_CATALOG_REFRESH_INTERVAL_MS`, 4 hours).
pub const REMOTE_CATALOG_REFRESH_INTERVAL_MS: i64 = 4 * 60 * 60 * 1000;

/// Merge a dynamic catalog over a baseline, replacing by id and appending new
/// ids (pi's `mergeModels`, `remote-catalog-provider.ts:8-16`).
pub fn merge_models(baseline: &[Model], dynamic: &[Model]) -> Vec<Model> {
    let mut merged = baseline.to_vec();
    for model in dynamic {
        match merged.iter().position(|entry| entry.id == model.id) {
            Some(index) => merged[index] = model.clone(),
            None => merged.push(model.clone()),
        }
    }
    merged
}

/// Parse a pi.dev catalog payload into a model list, stamping each entry's
/// provider (pi's `parseCatalog`, `remote-catalog-provider.ts:18-30`).
///
/// Accepts three JSON shapes: a bare array, `{ models: [...] }`, or an object
/// whose values are the entries. Only object entries carrying an `"id"` are
/// kept; each keeps its fields with `provider` overwritten to `provider_id`.
/// Returns `Err` for any other shape, mirroring pi's thrown
/// `Invalid model catalog for provider "..."`.
pub fn parse_catalog(provider_id: &str, value: &Value) -> Result<Vec<Model>, String> {
    let entries: Vec<Value> = match value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => match map.get("models") {
            Some(Value::Array(items)) => items.clone(),
            _ => map.values().cloned().collect(),
        },
        _ => {
            return Err(format!(
                "Invalid model catalog for provider \"{provider_id}\""
            ))
        }
    };
    Ok(entries
        .into_iter()
        .filter(|entry| entry.is_object() && entry.get("id").is_some())
        .filter_map(|entry| {
            serde_json::from_value::<Model>(entry)
                .ok()
                .map(|mut model| {
                    model.provider = provider_id.to_string();
                    model
                })
        })
        .collect())
}

#[cfg(test)]
mod tests;
