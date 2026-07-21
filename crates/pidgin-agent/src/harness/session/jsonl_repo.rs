//! JSONL session repository, mirroring
//! `packages/agent/src/harness/session/jsonl-repo.ts`.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Map, Value};

use super::jsonl_storage::{load_jsonl_session_metadata, JsonlCreateOptions, JsonlSessionStorage};
use super::repo_utils::{create_session_id, get_entries_to_fork, ForkOptions};
use super::session::Session;
use super::storage::{now_iso, SessionStorage};
use crate::harness::types::{SessionError, SessionErrorCode, SessionMetadata};

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

    pub fn create(&self, options: JsonlCreate) -> Result<Session, SessionError> {
        let id = options.id.unwrap_or_else(create_session_id);
        let created_at = now_iso();
        let session_dir = self.session_dir(&options.cwd);
        fs::create_dir_all(&session_dir).map_err(|e| {
            SessionError::storage(format!("Failed to create session directory: {e}"))
        })?;
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
                .map_err(|e| SessionError::storage(format!("Failed to list sessions: {e}")))?;
            for entry in read_dir {
                let entry = entry
                    .map_err(|e| SessionError::storage(format!("Failed to list sessions: {e}")))?;
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
            Err(e) => Err(SessionError::storage(format!(
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
        let created_at = now_iso();
        let session_dir = self.session_dir(&create.cwd);
        fs::create_dir_all(&session_dir).map_err(|e| {
            SessionError::storage(format!("Failed to create session directory: {e}"))
        })?;
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
            .map_err(|e| SessionError::storage(format!("Failed to list sessions root: {e}")))?;
        let mut dirs = Vec::new();
        for entry in read_dir {
            let entry = entry
                .map_err(|e| SessionError::storage(format!("Failed to list sessions root: {e}")))?;
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
