//! In-memory session repository, mirroring
//! `packages/agent/src/harness/session/memory-repo.ts`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::repo_utils::{create_session_id, get_entries_to_fork, ForkOptions};
use super::session::Session;
use super::storage::{now_iso, InMemorySessionStorage, SessionStorage};
use crate::harness::types::{SessionError, SessionErrorCode, SessionMetadata};

/// In-memory session repository. Mirrors `InMemorySessionRepo`.
pub struct InMemorySessionRepo {
    sessions: RefCell<HashMap<String, Rc<InMemorySessionStorage>>>,
}

impl Default for InMemorySessionRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySessionRepo {
    pub fn new() -> Self {
        Self {
            sessions: RefCell::new(HashMap::new()),
        }
    }

    pub fn create(&self, id: Option<&str>) -> Session {
        let metadata = SessionMetadata::in_memory(
            id.map(str::to_string).unwrap_or_else(create_session_id),
            now_iso(),
        );
        let storage = Rc::new(InMemorySessionStorage::with_options(
            None,
            Some(metadata.clone()),
        ));
        self.sessions
            .borrow_mut()
            .insert(metadata.id, storage.clone());
        Session::new(storage)
    }

    pub fn open(&self, metadata: &SessionMetadata) -> Result<Session, SessionError> {
        let storage = self.sessions.borrow().get(&metadata.id).cloned();
        match storage {
            Some(storage) => Ok(Session::new(storage)),
            None => Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Session not found: {}", metadata.id),
            )),
        }
    }

    pub fn list(&self) -> Vec<SessionMetadata> {
        self.sessions
            .borrow()
            .values()
            .map(|storage| storage.get_metadata())
            .collect()
    }

    pub fn delete(&self, metadata: &SessionMetadata) {
        self.sessions.borrow_mut().remove(&metadata.id);
    }

    pub fn fork(
        &self,
        source_metadata: &SessionMetadata,
        options: ForkOptions,
    ) -> Result<Session, SessionError> {
        let source = self.open(source_metadata)?;
        let forked_entries = get_entries_to_fork(
            source.get_storage().as_ref(),
            options.entry_id.as_deref(),
            options.position,
        )?;
        let metadata =
            SessionMetadata::in_memory(options.id.unwrap_or_else(create_session_id), now_iso());
        let storage = Rc::new(InMemorySessionStorage::with_options(
            Some(forked_entries),
            Some(metadata.clone()),
        ));
        self.sessions
            .borrow_mut()
            .insert(metadata.id, storage.clone());
        Ok(Session::new(storage))
    }
}
