//! The parsed catalog container and lazy accessors over the embedded snapshot.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::types::Model;

/// The aggregate catalog: provider id -> (model id -> [`Model`]).
///
/// A newtype over the nested [`BTreeMap`] so it can carry ergonomic accessor
/// methods and a stable public surface for consumers like atilla-ai.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog(BTreeMap<String, BTreeMap<String, Model>>);

impl Catalog {
    /// Iterate provider ids in sorted order.
    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Number of providers in the catalog.
    pub fn provider_count(&self) -> usize {
        self.0.len()
    }

    /// Look up the model map for a single provider.
    pub fn provider(&self, id: &str) -> Option<&BTreeMap<String, Model>> {
        self.0.get(id)
    }

    /// Look up one model by provider id and model id.
    pub fn model(&self, provider: &str, id: &str) -> Option<&Model> {
        self.0.get(provider)?.get(id)
    }

    /// Iterate every `(provider id, model)` pair across all providers.
    pub fn all_models(&self) -> impl Iterator<Item = (&str, &Model)> {
        self.0
            .iter()
            .flat_map(|(provider, models)| models.values().map(move |m| (provider.as_str(), m)))
    }

    /// Total number of models across all providers.
    pub fn len(&self) -> usize {
        self.0.values().map(BTreeMap::len).sum()
    }

    /// Whether the catalog contains no models.
    pub fn is_empty(&self) -> bool {
        self.0.values().all(BTreeMap::is_empty)
    }
}

/// Provenance metadata for the embedded snapshot, parsed from `data/manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Upstream repository the snapshot was generated from.
    pub upstream_repo: String,
    /// The pi commit the snapshot was generated at.
    pub pi_pin: String,
    /// ISO-8601 UTC timestamp of when the snapshot was generated.
    pub generated_at: String,
    /// Human description of the data sources.
    pub source: String,
    /// The generator invocation used.
    pub generator: String,
    /// Number of providers captured.
    pub provider_count: usize,
    /// Number of models captured.
    pub model_count: usize,
}

const MODELS_JSON: &str = include_str!("../data/models.json");
const MANIFEST_JSON: &str = include_str!("../data/manifest.json");

/// The parsed model catalog, embedded at build time and parsed once on first use.
///
/// Panics only if the embedded data is corrupt — a condition the crate's tests
/// guard against, so it cannot occur for a published build.
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(MODELS_JSON).expect("embedded models.json must be valid Catalog")
    })
}

/// The parsed snapshot manifest, embedded at build time and parsed once on first use.
///
/// Panics only if the embedded manifest is corrupt (guarded by tests).
pub fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(MANIFEST_JSON).expect("embedded manifest.json must be valid Manifest")
    })
}
