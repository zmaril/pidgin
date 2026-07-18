//! Session storage trait and in-memory implementation, mirroring
//! `packages/agent/src/harness/session/memory-storage.ts` and the
//! `SessionStorage` interface from `types.ts`.
//!
//! Methods take `&self` and use interior mutability so a single storage handle
//! can back several [`Session`](super::session::Session) instances (and be
//! shared by a repository), matching pi where a storage object is passed around
//! by reference.

use std::cell::RefCell;
use std::collections::HashMap;

use super::uuid::uuidv7;
use crate::harness::types::{
    LeafEntry, SessionError, SessionErrorCode, SessionMetadata, SessionTreeEntry,
};

/// Append-only session-tree storage. Mirrors pi's `SessionStorage`.
pub trait SessionStorage {
    fn get_metadata(&self) -> SessionMetadata;
    fn get_leaf_id(&self) -> Result<Option<String>, SessionError>;
    /// Persist a leaf entry recording the active session-tree leaf.
    fn set_leaf_id(&self, leaf_id: Option<&str>) -> Result<(), SessionError>;
    fn create_entry_id(&self) -> String;
    fn append_entry(&self, entry: SessionTreeEntry) -> Result<(), SessionError>;
    fn get_entry(&self, id: &str) -> Option<SessionTreeEntry>;
    fn find_entries(&self, entry_type: &str) -> Vec<SessionTreeEntry>;
    fn get_label(&self, id: &str) -> Option<String>;
    fn get_path_to_root(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionTreeEntry>, SessionError>;
    fn get_entries(&self) -> Vec<SessionTreeEntry>;
}

/// Update the label cache for one entry. A label with non-empty trimmed text
/// sets the mapping; an empty or cleared label removes it.
pub(crate) fn update_label_cache(
    labels_by_id: &mut HashMap<String, String>,
    entry: &SessionTreeEntry,
) {
    let SessionTreeEntry::Label(label) = entry else {
        return;
    };
    let text = label.label.as_deref().unwrap_or("").trim();
    if text.is_empty() {
        labels_by_id.remove(&label.target_id);
    } else {
        labels_by_id.insert(label.target_id.clone(), text.to_string());
    }
}

pub(crate) fn build_labels_by_id(entries: &[SessionTreeEntry]) -> HashMap<String, String> {
    let mut labels_by_id = HashMap::new();
    for entry in entries {
        update_label_cache(&mut labels_by_id, entry);
    }
    labels_by_id
}

/// Generate a short entry id from the random tail of a uuidv7, retrying on
/// collision. Mirrors pi's `generateEntryId`.
pub(crate) fn generate_entry_id(by_id: &HashMap<String, SessionTreeEntry>) -> String {
    for _ in 0..100 {
        let uuid = uuidv7();
        let id = uuid[uuid.len() - 8..].to_string();
        if !by_id.contains_key(&id) {
            return id;
        }
    }
    uuidv7()
}

/// Build the `leaf` entry appended by `set_leaf_id`: a fresh id, the current
/// leaf as parent, and `target_id` as the new active leaf (or `None` to clear).
pub(crate) fn make_leaf_entry(
    by_id: &HashMap<String, SessionTreeEntry>,
    parent_id: Option<String>,
    target_id: Option<&str>,
) -> SessionTreeEntry {
    SessionTreeEntry::Leaf(LeafEntry {
        id: generate_entry_id(by_id),
        parent_id,
        timestamp: now_iso(),
        target_id: target_id.map(str::to_string),
    })
}

/// Walk `parentId` links from a leaf to the root, root-first. Mirrors the
/// `getPathToRoot` shared by both storages.
pub(crate) fn path_to_root(
    by_id: &HashMap<String, SessionTreeEntry>,
    leaf_id: Option<&str>,
) -> Result<Vec<SessionTreeEntry>, SessionError> {
    let Some(leaf_id) = leaf_id else {
        return Ok(Vec::new());
    };
    let mut path: Vec<SessionTreeEntry> = Vec::new();
    let mut current = by_id
        .get(leaf_id)
        .cloned()
        .ok_or_else(|| SessionError::entry_not_found(leaf_id))?;
    loop {
        let parent_id = current.parent_id().map(str::to_string);
        path.insert(0, current);
        let Some(parent_id) = parent_id else {
            break;
        };
        current = by_id.get(&parent_id).cloned().ok_or_else(|| {
            SessionError::new(
                SessionErrorCode::InvalidSession,
                format!("Entry {parent_id} not found"),
            )
        })?;
    }
    Ok(path)
}

struct MemInner {
    entries: Vec<SessionTreeEntry>,
    by_id: HashMap<String, SessionTreeEntry>,
    labels_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
}

/// In-memory session storage. Mirrors `InMemorySessionStorage`.
pub struct InMemorySessionStorage {
    metadata: SessionMetadata,
    inner: RefCell<MemInner>,
}

impl InMemorySessionStorage {
    /// Create empty in-memory storage with a generated metadata id.
    pub fn new() -> Self {
        Self::with_options(None, None)
    }

    /// Create storage seeded with `entries` and/or explicit `metadata`.
    /// Initial entries are copied defensively and the leaf is reconstructed
    /// from the last entry, matching pi's constructor.
    pub fn with_options(
        entries: Option<Vec<SessionTreeEntry>>,
        metadata: Option<SessionMetadata>,
    ) -> Self {
        let entries = entries.unwrap_or_default();
        let by_id: HashMap<String, SessionTreeEntry> = entries
            .iter()
            .map(|entry| (entry.id().to_string(), entry.clone()))
            .collect();
        let labels_by_id = build_labels_by_id(&entries);
        let mut leaf_id = None;
        for entry in &entries {
            leaf_id = entry.leaf_id_after();
        }
        if let Some(id) = &leaf_id {
            if !by_id.contains_key(id) {
                panic!("Entry {id} not found");
            }
        }
        let metadata = metadata.unwrap_or_else(|| SessionMetadata::in_memory(uuidv7(), now_iso()));
        Self {
            metadata,
            inner: RefCell::new(MemInner {
                entries,
                by_id,
                labels_by_id,
                leaf_id,
            }),
        }
    }
}

impl Default for InMemorySessionStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStorage for InMemorySessionStorage {
    fn get_metadata(&self) -> SessionMetadata {
        self.metadata.clone()
    }

    fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        let inner = self.inner.borrow();
        if let Some(id) = &inner.leaf_id {
            if !inner.by_id.contains_key(id) {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {id} not found"),
                ));
            }
        }
        Ok(inner.leaf_id.clone())
    }

    fn set_leaf_id(&self, leaf_id: Option<&str>) -> Result<(), SessionError> {
        let mut inner = self.inner.borrow_mut();
        if let Some(id) = leaf_id {
            if !inner.by_id.contains_key(id) {
                return Err(SessionError::entry_not_found(id));
            }
        }
        let entry = make_leaf_entry(&inner.by_id, inner.leaf_id.clone(), leaf_id);
        inner.by_id.insert(entry.id().to_string(), entry.clone());
        inner.entries.push(entry);
        inner.leaf_id = leaf_id.map(str::to_string);
        Ok(())
    }

    fn create_entry_id(&self) -> String {
        generate_entry_id(&self.inner.borrow().by_id)
    }

    fn append_entry(&self, entry: SessionTreeEntry) -> Result<(), SessionError> {
        let mut inner = self.inner.borrow_mut();
        inner.by_id.insert(entry.id().to_string(), entry.clone());
        update_label_cache(&mut inner.labels_by_id, &entry);
        inner.leaf_id = entry.leaf_id_after();
        inner.entries.push(entry);
        Ok(())
    }

    fn get_entry(&self, id: &str) -> Option<SessionTreeEntry> {
        self.inner.borrow().by_id.get(id).cloned()
    }

    fn find_entries(&self, entry_type: &str) -> Vec<SessionTreeEntry> {
        self.inner
            .borrow()
            .entries
            .iter()
            .filter(|entry| entry.type_str() == entry_type)
            .cloned()
            .collect()
    }

    fn get_label(&self, id: &str) -> Option<String> {
        self.inner.borrow().labels_by_id.get(id).cloned()
    }

    fn get_path_to_root(
        &self,
        leaf_id: Option<&str>,
    ) -> Result<Vec<SessionTreeEntry>, SessionError> {
        path_to_root(&self.inner.borrow().by_id, leaf_id)
    }

    fn get_entries(&self) -> Vec<SessionTreeEntry> {
        self.inner.borrow().entries.clone()
    }
}

/// A best-effort ISO-8601 UTC timestamp for entries created at runtime.
pub(crate) fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(millis)
}

/// Format epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.sssZ`.
pub(crate) fn iso_from_millis(millis: i64) -> String {
    let total_secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000);
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// Inverse of `days_from_civil` (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}
