//! Persistent model catalogs keyed by provider ID (`models-store.ts`).
//!
//! pi caches each provider's discovered models so it does not have to re-query
//! the remote catalog on every run. Two interfaces describe that cache: a
//! [`ModelsStore`] that a host implements against real storage (keyed by
//! provider ID), and a [`ProviderModelsStore`] — the same operations scoped to a
//! single provider so a provider cannot read or clobber another's catalog.
//!
//! The crate is synchronous throughout, so pi's `Promise`-returning methods are
//! ported as plain synchronous trait methods; [`InMemoryModelsStore`] mirrors
//! pi's `structuredClone`-on-read isolation with Rust's deep [`Clone`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::types::Model;

/// A cached provider catalog plus the freshness marker for its last remote check
/// (`models-store.ts:3`).
///
/// pi types `models` as `readonly Model<Api>[]`; the Rust [`Model`] already
/// carries the `api` discriminator as a field, so the default (untyped) compat
/// view is used here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelsStoreEntry {
    /// The provider's cached models.
    pub models: Vec<Model>,
    /// Unix timestamp of the last completed remote check.
    pub checked_at: Option<i64>,
}

/// Persistent model catalogs keyed by provider ID (`models-store.ts:10`).
pub trait ModelsStore: Send + Sync {
    /// Read a provider's cached catalog, or `None` if nothing is stored.
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry>;
    /// Write (replace) a provider's cached catalog.
    fn write(&self, provider_id: &str, entry: ModelsStoreEntry);
    /// Delete a provider's cached catalog.
    fn delete(&self, provider_id: &str);
}

/// [`ModelsStore`] scoped to one provider. Providers cannot access other
/// providers' catalogs (`models-store.ts:17`).
pub trait ProviderModelsStore: Send + Sync {
    /// Read this provider's cached catalog, or `None` if nothing is stored.
    fn read(&self) -> Option<ModelsStoreEntry>;
    /// Write (replace) this provider's cached catalog.
    fn write(&self, entry: ModelsStoreEntry);
    /// Delete this provider's cached catalog.
    fn delete(&self);
}

/// An in-memory [`ModelsStore`] (`models-store.ts:23`).
///
/// Reads return a deep clone so a caller mutating the returned entry cannot
/// affect the stored copy, mirroring pi's `structuredClone`.
#[derive(Debug, Default)]
pub struct InMemoryModelsStore {
    entries: Mutex<BTreeMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
    /// Construct an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModelsStore for InMemoryModelsStore {
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry> {
        self.entries.lock().unwrap().get(provider_id).cloned()
    }

    fn write(&self, provider_id: &str, entry: ModelsStoreEntry) {
        self.entries
            .lock()
            .unwrap()
            .insert(provider_id.to_string(), entry);
    }

    fn delete(&self, provider_id: &str) {
        self.entries.lock().unwrap().remove(provider_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Modality, ModelCost};

    fn model(id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "test-api".to_string(),
            provider: "test-provider".to_string(),
            base_url: "https://example.test/v1".to_string(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            context_window: 10_000,
            max_tokens: 1_000,
            headers: None,
            compat: None,
        }
    }

    fn entry(id: &str) -> ModelsStoreEntry {
        ModelsStoreEntry {
            models: vec![model(id)],
            checked_at: Some(1_700_000_000),
        }
    }

    #[test]
    fn read_missing_provider_is_none() {
        let store = InMemoryModelsStore::new();
        assert_eq!(store.read("absent"), None);
    }

    #[test]
    fn write_then_read_round_trips() {
        let store = InMemoryModelsStore::new();
        store.write("openai", entry("gpt"));
        assert_eq!(store.read("openai"), Some(entry("gpt")));
    }

    #[test]
    fn write_replaces_existing_entry() {
        let store = InMemoryModelsStore::new();
        store.write("openai", entry("gpt"));
        store.write("openai", entry("gpt-next"));
        assert_eq!(store.read("openai"), Some(entry("gpt-next")));
    }

    #[test]
    fn delete_removes_entry() {
        let store = InMemoryModelsStore::new();
        store.write("openai", entry("gpt"));
        store.delete("openai");
        assert_eq!(store.read("openai"), None);
    }

    #[test]
    fn delete_missing_provider_is_noop() {
        let store = InMemoryModelsStore::new();
        store.delete("absent");
        assert_eq!(store.read("absent"), None);
    }

    #[test]
    fn read_returns_isolated_clone() {
        let store = InMemoryModelsStore::new();
        store.write("openai", entry("gpt"));

        let mut taken = store.read("openai").unwrap();
        taken.models.clear();
        taken.checked_at = None;

        // Mutating the returned entry must not disturb the stored copy.
        assert_eq!(store.read("openai"), Some(entry("gpt")));
    }

    #[test]
    fn usable_as_trait_object() {
        let store: Box<dyn ModelsStore> = Box::new(InMemoryModelsStore::new());
        store.write("openai", entry("gpt"));
        assert_eq!(store.read("openai"), Some(entry("gpt")));
    }
}
