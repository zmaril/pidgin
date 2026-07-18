//! Session repositories and fork utilities, mirroring
//! `packages/agent/src/harness/session/{repo-utils,memory-repo,jsonl-repo}.ts`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Map, Value};

use super::jsonl_storage::{load_jsonl_session_metadata, JsonlCreateOptions, JsonlSessionStorage};
use super::session::Session;
use super::storage::{InMemorySessionStorage, SessionStorage};
use super::uuid::uuidv7;
use crate::harness::types::{SessionError, SessionErrorCode, SessionMetadata, SessionTreeEntry};

/// Where to fork relative to a target entry. Mirrors pi's `position` option.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ForkPosition {
    /// Path up to (and excluding) the target, which must be a user message.
    Before,
    /// Path up to and including the target.
    At,
}

/// Options for forking a session. Mirrors the fork option subset of
/// `SessionForkOptions`.
#[derive(Default)]
pub struct ForkOptions {
    pub entry_id: Option<String>,
    pub position: Option<ForkPosition>,
    pub id: Option<String>,
}

/// Generate a full uuidv7 session id. Mirrors `createSessionId`.
pub fn create_session_id() -> String {
    uuidv7()
}

/// Compute the entries a fork should copy. Mirrors `getEntriesToFork`.
pub fn get_entries_to_fork(
    storage: &dyn SessionStorage,
    entry_id: Option<&str>,
    position: Option<ForkPosition>,
) -> Result<Vec<SessionTreeEntry>, SessionError> {
    let Some(entry_id) = entry_id else {
        return Ok(storage.get_entries());
    };
    let target = storage.get_entry(entry_id).ok_or_else(|| {
        SessionError::new(
            SessionErrorCode::InvalidForkTarget,
            format!("Entry {entry_id} not found"),
        )
    })?;
    let effective_leaf_id: Option<String> = if position.unwrap_or(ForkPosition::Before)
        == ForkPosition::At
    {
        Some(target.id().to_string())
    } else {
        let is_user_message = matches!(
            &target,
            SessionTreeEntry::Message(e) if e.message.get("role").and_then(Value::as_str) == Some("user")
        );
        if !is_user_message {
            return Err(SessionError::new(
                SessionErrorCode::InvalidForkTarget,
                format!("Entry {entry_id} is not a user message"),
            ));
        }
        target.parent_id().map(str::to_string)
    };
    storage.get_path_to_root(effective_leaf_id.as_deref())
}

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
            super::storage::now_iso(),
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
        let metadata = SessionMetadata::in_memory(
            options.id.unwrap_or_else(create_session_id),
            super::storage::now_iso(),
        );
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

/// Options for [`JsonlSessionRepo::create`]/`fork`. Mirrors
/// `JsonlSessionCreateOptions`.
#[derive(Default)]
pub struct JsonlCreate {
    pub cwd: String,
    pub id: Option<String>,
    pub parent_session_path: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

/// Encode a cwd into a session directory name. Mirrors `encodeCwd`:
/// strip a leading slash/backslash, then replace `/ \ :` with `-`, wrapped in
/// `--`.
fn encode_cwd(cwd: &str) -> String {
    let stripped = cwd.strip_prefix(['/', '\\']).unwrap_or(cwd);
    let replaced: String = stripped
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{replaced}--")
}

/// JSONL session repository rooted at an injectable sessions directory. Mirrors
/// `JsonlSessionRepo` (the base dir is a plain path so tests can point it at a
/// temp directory).
pub struct JsonlSessionRepo {
    sessions_root: PathBuf,
}

impl JsonlSessionRepo {
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
        }
    }

    fn session_dir(&self, cwd: &str) -> PathBuf {
        self.sessions_root.join(encode_cwd(cwd))
    }

    fn session_file_path(&self, cwd: &str, session_id: &str, timestamp: &str) -> PathBuf {
        let stamp: String = timestamp
            .chars()
            .map(|c| if c == ':' || c == '.' { '-' } else { c })
            .collect();
        self.session_dir(cwd)
            .join(format!("{stamp}_{session_id}.jsonl"))
    }

    fn storage_error(message: impl Into<String>) -> SessionError {
        SessionError::new(SessionErrorCode::Storage, message.into())
    }

    pub fn create(&self, options: JsonlCreate) -> Result<Session, SessionError> {
        let id = options.id.unwrap_or_else(create_session_id);
        let created_at = super::storage::now_iso();
        let session_dir = self.session_dir(&options.cwd);
        fs::create_dir_all(&session_dir)
            .map_err(|e| Self::storage_error(format!("Failed to create session directory: {e}")))?;
        let file_path = self.session_file_path(&options.cwd, &id, &created_at);
        let storage = JsonlSessionStorage::create(
            &path_str(&file_path),
            JsonlCreateOptions {
                cwd: options.cwd,
                session_id: id,
                parent_session_path: options.parent_session_path,
                metadata: options.metadata,
            },
        )?;
        Ok(Session::new(Rc::new(storage)))
    }

    pub fn open(&self, metadata: &SessionMetadata) -> Result<Session, SessionError> {
        let path = metadata.path.as_deref().unwrap_or("");
        if !Path::new(path).exists() {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Session not found: {path}"),
            ));
        }
        let storage = JsonlSessionStorage::open(path)?;
        Ok(Session::new(Rc::new(storage)))
    }

    pub fn list(&self, cwd: Option<&str>) -> Result<Vec<SessionMetadata>, SessionError> {
        let dirs: Vec<PathBuf> = match cwd {
            Some(cwd) => vec![self.session_dir(cwd)],
            None => self.list_session_dirs()?,
        };
        let mut sessions = Vec::new();
        for dir in dirs {
            if !dir.exists() {
                continue;
            }
            let read_dir = fs::read_dir(&dir)
                .map_err(|e| Self::storage_error(format!("Failed to list sessions: {e}")))?;
            for entry in read_dir {
                let entry = entry
                    .map_err(|e| Self::storage_error(format!("Failed to list sessions: {e}")))?;
                let path = entry.path();
                if path.is_dir() || path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                match load_jsonl_session_metadata(&path_str(&path)) {
                    Ok(metadata) => sessions.push(metadata),
                    Err(error) if error.code == SessionErrorCode::InvalidSession => {}
                    Err(error) => return Err(error),
                }
            }
        }
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(sessions)
    }

    pub fn delete(&self, metadata: &SessionMetadata) -> Result<(), SessionError> {
        let path = metadata.path.as_deref().unwrap_or("");
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Self::storage_error(format!(
                "Failed to delete session {path}: {e}"
            ))),
        }
    }

    pub fn fork(
        &self,
        source_metadata: &SessionMetadata,
        create: JsonlCreate,
        fork: ForkOptions,
    ) -> Result<Session, SessionError> {
        let source = self.open(source_metadata)?;
        let forked_entries = get_entries_to_fork(
            source.get_storage().as_ref(),
            fork.entry_id.as_deref(),
            fork.position,
        )?;
        let id = create.id.unwrap_or_else(create_session_id);
        let created_at = super::storage::now_iso();
        let session_dir = self.session_dir(&create.cwd);
        fs::create_dir_all(&session_dir)
            .map_err(|e| Self::storage_error(format!("Failed to create session directory: {e}")))?;
        let file_path = self.session_file_path(&create.cwd, &id, &created_at);
        let storage = JsonlSessionStorage::create(
            &path_str(&file_path),
            JsonlCreateOptions {
                cwd: create.cwd,
                session_id: id,
                parent_session_path: create
                    .parent_session_path
                    .or_else(|| source_metadata.path.clone()),
                metadata: create.metadata.or_else(|| source_metadata.metadata.clone()),
            },
        )?;
        for entry in forked_entries {
            storage.append_entry(entry)?;
        }
        Ok(Session::new(Rc::new(storage)))
    }

    fn list_session_dirs(&self) -> Result<Vec<PathBuf>, SessionError> {
        if !self.sessions_root.exists() {
            return Ok(Vec::new());
        }
        let read_dir = fs::read_dir(&self.sessions_root)
            .map_err(|e| Self::storage_error(format!("Failed to list sessions root: {e}")))?;
        let mut dirs = Vec::new();
        for entry in read_dir {
            let entry = entry
                .map_err(|e| Self::storage_error(format!("Failed to list sessions root: {e}")))?;
            if entry.path().is_dir() {
                dirs.push(entry.path());
            }
        }
        Ok(dirs)
    }
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
