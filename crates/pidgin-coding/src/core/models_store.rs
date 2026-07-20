// straitjacket-allow-file:duplication — a faithful transcription of pi's
// `models-store.ts`. The in-memory and file-backed stores expose the same
// `read`/`write`/`delete` shape over different substrates, so their method
// bodies are deliberately parallel; the clone detector reads that mirror as
// duplication.
//! Locked JSON storage for dynamically refreshed provider catalogs.
//!
//! Ported from pi's `core/models-store.ts` at pinned commit `3da591ab`.
//! Two stores implement the [`ModelsStore`] seam:
//!
//! - [`InMemoryCodingAgentModelsStore`] — a process-local map.
//! - [`FileModelsStore`] — a locked `models-store.json`, layered on the same
//!   [`FileAuthStorageBackend`](super::auth::auth_storage::FileAuthStorageBackend)
//!   the credential store uses.
//!
//! # The `ModelsStore` seam
//!
//! pi-ai defines the `ModelsStore` / `ModelsStoreEntry` interface consumed by
//! `createModels({ modelsStore })` and the remote-catalog overlay. pidgin-ai
//! has not ported that interface (there is no `trait ModelsStore` in
//! `crates/pidgin-ai`), so this module defines the seam locally, matching
//! pi-ai's shape. It is **synchronous**, like the rest of pidgin's storage
//! layer (pi's is `Promise`-returning). When pidgin-ai lands a canonical
//! `ModelsStore`, these stores can `impl` it with no logic change.

use std::collections::BTreeMap;
use std::sync::Mutex;

use pidgin_ai::types::Model;
use serde::{Deserialize, Serialize};

use super::auth::auth_storage::{AuthStorageBackend, FileAuthStorageBackend};
use super::skills::get_agent_dir;

/// One provider's persisted catalog: the last-known model list plus the epoch
/// millisecond timestamp it was checked at (pi's `ModelsStoreEntry`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStoreEntry {
    /// The last-known models for the provider.
    pub models: Vec<Model>,
    /// When the catalog was last checked (epoch ms), if ever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
}

/// Persisted per-provider catalog storage (pi-ai's `ModelsStore`).
///
/// The seam is defined locally in pidgin-coding because pidgin-ai has not
/// ported pi-ai's `ModelsStore`. Synchronous, unlike pi's async interface.
pub trait ModelsStore: Send + Sync {
    /// The stored entry for `provider_id`, or `None` when absent.
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry>;
    /// Persist `entry` for `provider_id`, replacing any prior entry.
    fn write(&self, provider_id: &str, entry: ModelsStoreEntry);
    /// Remove any stored entry for `provider_id`.
    fn delete(&self, provider_id: &str);
}

/// The keyed catalog map persisted as JSON (pi's `StoredModels`).
type StoredModels = BTreeMap<String, ModelsStoreEntry>;

/// A process-local [`ModelsStore`] (pi's `InMemoryCodingAgentModelsStore`).
#[derive(Default)]
pub struct InMemoryCodingAgentModelsStore {
    entries: Mutex<BTreeMap<String, ModelsStoreEntry>>,
}

impl InMemoryCodingAgentModelsStore {
    /// An empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModelsStore for InMemoryCodingAgentModelsStore {
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry> {
        self.entries
            .lock()
            .expect("models store mutex poisoned")
            .get(provider_id)
            .cloned()
    }

    fn write(&self, provider_id: &str, entry: ModelsStoreEntry) {
        self.entries
            .lock()
            .expect("models store mutex poisoned")
            .insert(provider_id.to_string(), entry);
    }

    fn delete(&self, provider_id: &str) {
        self.entries
            .lock()
            .expect("models store mutex poisoned")
            .remove(provider_id);
    }
}

/// Locked JSON-backed storage for dynamically refreshed provider catalogs
/// (pi's `FileModelsStore`). Layers on a
/// [`FileAuthStorageBackend`](super::auth::auth_storage::FileAuthStorageBackend)
/// so writes take the same exclusive file lock the credential store uses.
pub struct FileModelsStore {
    storage: Box<dyn AuthStorageBackend>,
}

impl FileModelsStore {
    /// A store over `models-store.json` at `path`.
    pub fn new(path: &str) -> Self {
        Self {
            storage: Box::new(FileAuthStorageBackend::new(path)),
        }
    }

    /// A store over the default `<agent_dir>/models-store.json` path.
    pub fn with_default_path() -> Self {
        Self::new(&format!("{}/models-store.json", get_agent_dir()))
    }

    /// A store over an arbitrary backend (used by the runtime to share a
    /// backend / for tests).
    pub fn from_storage(storage: Box<dyn AuthStorageBackend>) -> Self {
        Self { storage }
    }

    fn parse(content: Option<&str>) -> StoredModels {
        content
            .and_then(|c| serde_json::from_str(c).ok())
            .unwrap_or_default()
    }
}

impl ModelsStore for FileModelsStore {
    fn read(&self, provider_id: &str) -> Option<ModelsStoreEntry> {
        let mut result: Option<ModelsStoreEntry> = None;
        let _ = self.storage.with_lock(&mut |content| {
            result = Self::parse(content).get(provider_id).cloned();
            None
        });
        result
    }

    fn write(&self, provider_id: &str, entry: ModelsStoreEntry) {
        let _ = self.storage.with_lock(&mut |content| {
            let mut current = Self::parse(content);
            current.insert(provider_id.to_string(), entry.clone());
            serde_json::to_string_pretty(&current).ok()
        });
    }

    fn delete(&self, provider_id: &str) {
        let _ = self.storage.with_lock(&mut |content| {
            let mut current = Self::parse(content);
            current.remove(provider_id);
            serde_json::to_string_pretty(&current).ok()
        });
    }
}

#[cfg(test)]
mod tests;
