//! Node-API exports for agent-core session storage (`crates/pidgin-agent`),
//! backing the native `packages/agent/src/harness/session/jsonl-storage.ts` and
//! `memory-storage.ts` shims.
//!
//! Rich pi structures cross the boundary as JSON strings: entries and metadata
//! are `serde_json`-serialized here and `JSON.parse`d in the shim (and vice
//! versa). Fallible reads/writes return a `{ok,value}` / `{ok,error}` JSON
//! envelope the shim reshapes into pi's `Result`/`SessionError`; the `open` and
//! `create` factories instead throw a `{code,message}` JSON reason the shim
//! rebuilds into a `SessionError`.
//!
//! straitjacket-allow-file:duplication — the two storage handles below
//! (`JsonlSessionStorageCore` / `InMemorySessionStorageCore`) expose the same
//! `SessionStorage` surface, so their trivial method bodies are identical by
//! design, mirroring the two parallel storage classes in pi.

use napi_derive::napi;
use serde_json::{json, Map, Value};

use pidgin_agent::harness::session::{
    load_jsonl_session_metadata, InMemorySessionStorage as CoreInMemStorage, JsonlCreateOptions,
    JsonlSessionStorage as CoreJsonlStorage, SessionStorage,
};
use pidgin_agent::harness::types::{SessionError, SessionMetadata, SessionTreeEntry};

/// pi's `SessionError` as a `{code,message}` JSON value (the shim rebuilds a
/// `SessionError` from it).
fn session_err_value(error: &SessionError) -> Value {
    json!({ "code": error.code.as_str(), "message": error.message })
}

/// `{ok:false,error:{code,message}}` — a failed [`SessionStorage`] operation.
fn err_json(error: &SessionError) -> String {
    json!({ "ok": false, "error": session_err_value(error) }).to_string()
}

/// `{ok:true,value:…}` — a successful [`SessionStorage`] operation.
fn ok_json(value: Value) -> String {
    json!({ "ok": true, "value": value }).to_string()
}

/// Serialize [`SessionMetadata`] to pi's `SessionMetadata`/`JsonlSessionMetadata`
/// JSON shape, omitting the JSONL-only fields (`cwd`/`path`/`parentSessionPath`/
/// `metadata`) when absent so an in-memory record compares `toEqual` against
/// pi's `{id,createdAt}`.
fn metadata_to_value(metadata: &SessionMetadata) -> Value {
    let mut obj = Map::new();
    obj.insert("id".into(), Value::String(metadata.id.clone()));
    obj.insert(
        "createdAt".into(),
        Value::String(metadata.created_at.clone()),
    );
    if let Some(cwd) = &metadata.cwd {
        obj.insert("cwd".into(), Value::String(cwd.clone()));
    }
    if let Some(path) = &metadata.path {
        obj.insert("path".into(), Value::String(path.clone()));
    }
    if let Some(parent) = &metadata.parent_session_path {
        obj.insert("parentSessionPath".into(), Value::String(parent.clone()));
    }
    if let Some(meta) = &metadata.metadata {
        obj.insert("metadata".into(), Value::Object(meta.clone()));
    }
    Value::Object(obj)
}

/// Serialize a slice of session-tree entries to a JSON array.
fn entries_to_value(entries: &[SessionTreeEntry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
            .collect(),
    )
}

/// Parse one session-tree entry from its JSON representation.
fn parse_entry(entry_json: &str) -> napi::Result<SessionTreeEntry> {
    serde_json::from_str(entry_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid session entry: {e}")))
}

/// Rebuild [`SessionMetadata`] from pi's metadata JSON (only `id`/`createdAt`
/// are relevant for the in-memory constructor; the JSONL-only fields carry
/// through if present).
fn metadata_from_value(value: &Value) -> SessionMetadata {
    let obj = value.as_object();
    let get = |key: &str| {
        obj.and_then(|o| o.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    SessionMetadata {
        id: get("id").unwrap_or_default(),
        created_at: get("createdAt").unwrap_or_default(),
        cwd: get("cwd"),
        path: get("path"),
        parent_session_path: get("parentSessionPath"),
        metadata: obj
            .and_then(|o| o.get("metadata"))
            .and_then(Value::as_object)
            .cloned(),
    }
}

/// `loadJsonlSessionMetadata` (jsonl-storage.ts) over the host filesystem: read
/// only the header line and return its metadata JSON. The shim routes here when
/// the injected `fs` is the Rust-backed `NodeExecutionEnv`; a foreign/async JS
/// `FileSystem` object stays on pi's original (which drives that injected `fs`).
/// Throws a `{code,message}` JSON reason on failure.
#[napi(js_name = "loadJsonlSessionMetadataNative")]
pub fn load_jsonl_session_metadata_native(file_path: String) -> napi::Result<String> {
    match load_jsonl_session_metadata(&file_path) {
        Ok(metadata) => Ok(metadata_to_value(&metadata).to_string()),
        Err(error) => Err(napi::Error::from_reason(
            session_err_value(&error).to_string(),
        )),
    }
}

/// The Rust-backed JSONL session storage, exposed to JavaScript as
/// `JsonlSessionStorageCore`. Reads and writes the session file directly on the
/// host filesystem (pi's tests inject a `NodeExecutionEnv` over the same real
/// disk), so no JS `FileSystem` callback crosses the boundary.
#[napi(js_name = "JsonlSessionStorageCore")]
pub struct JsonlSessionStorageCore {
    inner: CoreJsonlStorage,
}

#[napi]
impl JsonlSessionStorageCore {
    /// `JsonlSessionStorage.open`: open an existing session file. Throws a
    /// `{code,message}` JSON reason when the file is missing or malformed.
    #[napi(factory, js_name = "open")]
    pub fn open(file_path: String) -> napi::Result<Self> {
        match CoreJsonlStorage::open(&file_path) {
            Ok(inner) => Ok(Self { inner }),
            Err(error) => Err(napi::Error::from_reason(
                session_err_value(&error).to_string(),
            )),
        }
    }

    /// `JsonlSessionStorage.create`: write the header and return fresh storage.
    /// `options_json` carries `{cwd,sessionId,parentSessionPath?,metadata?}`.
    #[napi(factory, js_name = "create")]
    pub fn create(file_path: String, options_json: String) -> napi::Result<Self> {
        let value: Value = serde_json::from_str(&options_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid create options: {e}")))?;
        let options = JsonlCreateOptions {
            cwd: value
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            session_id: value
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            parent_session_path: value
                .get("parentSessionPath")
                .and_then(Value::as_str)
                .map(str::to_string),
            metadata: value.get("metadata").and_then(Value::as_object).cloned(),
        };
        match CoreJsonlStorage::create(&file_path, options) {
            Ok(inner) => Ok(Self { inner }),
            Err(error) => Err(napi::Error::from_reason(
                session_err_value(&error).to_string(),
            )),
        }
    }

    /// pi's `getMetadata`.
    #[napi(js_name = "getMetadata")]
    pub fn get_metadata(&self) -> String {
        metadata_to_value(&self.inner.get_metadata()).to_string()
    }

    /// pi's `getLeafId`.
    #[napi(js_name = "getLeafId")]
    pub fn get_leaf_id(&self) -> String {
        match self.inner.get_leaf_id() {
            Ok(leaf) => ok_json(leaf.map(Value::String).unwrap_or(Value::Null)),
            Err(error) => err_json(&error),
        }
    }

    /// pi's `setLeafId`.
    #[napi(js_name = "setLeafId")]
    pub fn set_leaf_id(&self, leaf_id: Option<String>) -> String {
        match self.inner.set_leaf_id(leaf_id.as_deref()) {
            Ok(()) => ok_json(Value::Null),
            Err(error) => err_json(&error),
        }
    }

    /// pi's `createEntryId`.
    #[napi(js_name = "createEntryId")]
    pub fn create_entry_id(&self) -> String {
        self.inner.create_entry_id()
    }

    /// pi's `appendEntry`. `entry_json` is one serialized session-tree entry.
    #[napi(js_name = "appendEntry")]
    pub fn append_entry(&self, entry_json: String) -> napi::Result<String> {
        let entry = parse_entry(&entry_json)?;
        Ok(match self.inner.append_entry(entry) {
            Ok(()) => ok_json(Value::Null),
            Err(error) => err_json(&error),
        })
    }

    /// pi's `getEntry`; `undefined` (JS) for a missing id.
    #[napi(js_name = "getEntry")]
    pub fn get_entry(&self, id: String) -> Option<String> {
        self.inner.get_entry(&id).map(|entry| {
            serde_json::to_value(&entry)
                .unwrap_or(Value::Null)
                .to_string()
        })
    }

    /// pi's `findEntries`.
    #[napi(js_name = "findEntries")]
    pub fn find_entries(&self, entry_type: String) -> String {
        entries_to_value(&self.inner.find_entries(&entry_type)).to_string()
    }

    /// pi's `getLabel`; `undefined` (JS) for a missing/cleared label.
    #[napi(js_name = "getLabel")]
    pub fn get_label(&self, id: String) -> Option<String> {
        self.inner.get_label(&id)
    }

    /// pi's `getPathToRoot`.
    #[napi(js_name = "getPathToRoot")]
    pub fn get_path_to_root(&self, leaf_id: Option<String>) -> String {
        match self.inner.get_path_to_root(leaf_id.as_deref()) {
            Ok(entries) => ok_json(entries_to_value(&entries)),
            Err(error) => err_json(&error),
        }
    }

    /// pi's `getEntries`.
    #[napi(js_name = "getEntries")]
    pub fn get_entries(&self) -> String {
        entries_to_value(&self.inner.get_entries()).to_string()
    }
}

/// The Rust-backed in-memory session storage, exposed to JavaScript as
/// `InMemorySessionStorageCore`.
#[napi(js_name = "InMemorySessionStorageCore")]
pub struct InMemorySessionStorageCore {
    inner: CoreInMemStorage,
}

#[napi]
impl InMemorySessionStorageCore {
    /// pi's `new InMemorySessionStorage({ entries?, metadata? })`. Both options
    /// cross as JSON strings; initial entries are copied defensively by the
    /// Rust constructor.
    #[napi(constructor)]
    pub fn new(entries_json: Option<String>, metadata_json: Option<String>) -> napi::Result<Self> {
        let entries = match entries_json {
            Some(json) => Some(
                serde_json::from_str::<Vec<SessionTreeEntry>>(&json)
                    .map_err(|e| napi::Error::from_reason(format!("invalid entries: {e}")))?,
            ),
            None => None,
        };
        let metadata = match metadata_json {
            Some(json) => {
                let value: Value = serde_json::from_str(&json)
                    .map_err(|e| napi::Error::from_reason(format!("invalid metadata: {e}")))?;
                Some(metadata_from_value(&value))
            }
            None => None,
        };
        Ok(Self {
            inner: CoreInMemStorage::with_options(entries, metadata),
        })
    }

    /// pi's `getMetadata`.
    #[napi(js_name = "getMetadata")]
    pub fn get_metadata(&self) -> String {
        metadata_to_value(&self.inner.get_metadata()).to_string()
    }

    /// pi's `getLeafId`.
    #[napi(js_name = "getLeafId")]
    pub fn get_leaf_id(&self) -> String {
        match self.inner.get_leaf_id() {
            Ok(leaf) => ok_json(leaf.map(Value::String).unwrap_or(Value::Null)),
            Err(error) => err_json(&error),
        }
    }

    /// pi's `setLeafId`.
    #[napi(js_name = "setLeafId")]
    pub fn set_leaf_id(&self, leaf_id: Option<String>) -> String {
        match self.inner.set_leaf_id(leaf_id.as_deref()) {
            Ok(()) => ok_json(Value::Null),
            Err(error) => err_json(&error),
        }
    }

    /// pi's `createEntryId`.
    #[napi(js_name = "createEntryId")]
    pub fn create_entry_id(&self) -> String {
        self.inner.create_entry_id()
    }

    /// pi's `appendEntry`. `entry_json` is one serialized session-tree entry.
    #[napi(js_name = "appendEntry")]
    pub fn append_entry(&self, entry_json: String) -> napi::Result<String> {
        let entry = parse_entry(&entry_json)?;
        Ok(match self.inner.append_entry(entry) {
            Ok(()) => ok_json(Value::Null),
            Err(error) => err_json(&error),
        })
    }

    /// pi's `getEntry`; `undefined` (JS) for a missing id.
    #[napi(js_name = "getEntry")]
    pub fn get_entry(&self, id: String) -> Option<String> {
        self.inner.get_entry(&id).map(|entry| {
            serde_json::to_value(&entry)
                .unwrap_or(Value::Null)
                .to_string()
        })
    }

    /// pi's `findEntries`.
    #[napi(js_name = "findEntries")]
    pub fn find_entries(&self, entry_type: String) -> String {
        entries_to_value(&self.inner.find_entries(&entry_type)).to_string()
    }

    /// pi's `getLabel`; `undefined` (JS) for a missing/cleared label.
    #[napi(js_name = "getLabel")]
    pub fn get_label(&self, id: String) -> Option<String> {
        self.inner.get_label(&id)
    }

    /// pi's `getPathToRoot`.
    #[napi(js_name = "getPathToRoot")]
    pub fn get_path_to_root(&self, leaf_id: Option<String>) -> String {
        match self.inner.get_path_to_root(leaf_id.as_deref()) {
            Ok(entries) => ok_json(entries_to_value(&entries)),
            Err(error) => err_json(&error),
        }
    }

    /// pi's `getEntries`.
    #[napi(js_name = "getEntries")]
    pub fn get_entries(&self) -> String {
        entries_to_value(&self.inner.get_entries()).to_string()
    }
}
