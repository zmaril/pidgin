//! Coding-agent session-manager surface: drives pi's canonical CLI
//! `SessionManager` (`packages/coding-agent/src/core/session-manager.ts`)
//! natively through the Rust port
//! [`pidgin_coding::core::session_manager`] (PR #101, CLI-canonical).
//!
//! # Scope of the native flip
//!
//! The whole `session-manager.ts` module — the stateful [`SessionManager`] class
//! (create / open / list / append / tree traversal / rewrite) plus the pure
//! free functions the tests exercise (`migrateSessionEntries`,
//! `buildContextEntries`, `buildSessionContext`, `findMostRecentSession`,
//! `loadEntriesFromFile`) — runs in Rust. The JS shim re-exports the module's
//! un-flipped surface (types, `CURRENT_SESSION_VERSION`, `assertValidSessionId`,
//! `parseSessionEntries`, `getLatestCompactionEntry`,
//! `sessionEntryToContextMessages`, `getDefaultSessionDir`) from pi's preserved
//! original and fronts the ported surface with a thin `SessionManager` class +
//! free functions delegating here.
//!
//! # The JSON boundary
//!
//! napi's generated `.d.ts` cannot express pi's discriminated-union entry types,
//! so every rich value crosses the boundary as a JSON string: entries, headers,
//! tree nodes, contexts, and session-info records serialize in Rust and the shim
//! `JSON.parse`s them (converting `SessionInfo.created`/`modified` back to
//! `Date`). Inputs (messages, options, custom-entry data, label targets) cross
//! as `JSON.stringify`d strings the other way. Optional string params use
//! napi's `Option<String>` (JS `undefined` → `None`); the shim coerces any pi
//! `T | null` argument to `undefined` before the call so the napi boundary never
//! sees a JS `null` (which `Option<String>` would reject).
//!
//! # `&mut self` behind napi's shared `&self`
//!
//! napi hands class methods a shared `&self`, but the Rust `SessionManager`
//! mutators take `&mut self`, so the wrapped manager lives in a [`RefCell`] and
//! each mutator `borrow_mut`s — the same idiom as `oauth.rs`. `tokensBefore`
//! crosses as an `f64` (JS safe-integer double) and is cast to `i64`, sidestep-
//! ping napi BigInt friction.
//!
//! # Faithful-port limitations (untested, no session consumer)
//!
//! - `open(path, sessionDir?, cwdOverride?)`: the Rust port derives the session
//!   directory from the file's parent, so the optional `sessionDir` override is
//!   accepted but ignored — no session test (they pass a `sessionDir` equal to
//!   the file's parent) nor coding-agent importer exercises it. `cwdOverride` IS
//!   honored via `SessionManager::set_cwd` after the load.
//! - `SessionListProgress` (`onProgress`): the Rust discovery layer surfaces no
//!   incremental progress, so the fire-and-forget callback is accepted by the
//!   shim and not invoked. It is optional and asserted by no test.

use std::cell::RefCell;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde_json::{json, Map, Value};

use pidgin_coding::core::session_manager as sm;
use sm::NewSessionOptions;

/// Map a `serde_json` error to a thrown JS `Error`.
fn je(error: serde_json::Error) -> Error {
    Error::from_reason(error.to_string())
}

/// Parse an optional `JSON.stringify`d value into an owned `serde_json::Value`.
/// A missing argument yields `None`; a malformed payload throws.
fn parse_value(json: Option<String>) -> Result<Option<Value>> {
    match json {
        None => Ok(None),
        Some(raw) => Ok(Some(serde_json::from_str(&raw).map_err(je)?)),
    }
}

/// pi's `NewSessionOptions` as it crosses the boundary (camelCase keys), parsed
/// at the edge and mapped onto [`NewSessionOptions`] (which is not `Deserialize`).
#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct NewSessionOptionsJson {
    id: Option<String>,
    parent_session: Option<String>,
}

/// Parse an optional `JSON.stringify`d `NewSessionOptions`.
fn parse_options(json: Option<String>) -> Result<NewSessionOptions> {
    match json {
        None => Ok(NewSessionOptions::default()),
        Some(raw) => {
            let parsed: NewSessionOptionsJson = serde_json::from_str(&raw).map_err(je)?;
            Ok(NewSessionOptions {
                id: parsed.id,
                parent_session: parsed.parent_session,
            })
        }
    }
}

/// Render a [`sm::SessionContext`] to pi's `{messages, thinkingLevel, model}`
/// JSON shape (the struct is intentionally non-`Serialize` in the port).
fn session_context_to_json(ctx: &sm::SessionContext) -> Value {
    let model = match &ctx.model {
        Some(model) => json!({ "provider": model.provider, "modelId": model.model_id }),
        None => Value::Null,
    };
    json!({
        "messages": ctx.messages,
        "thinkingLevel": ctx.thinking_level,
        "model": model,
    })
}

/// Render a [`sm::SessionTreeNode`] (and its children) to pi's tree JSON. The
/// optional `label` / `labelTimestamp` keys are omitted when absent, matching
/// pi's optional fields.
fn tree_node_to_json(node: &sm::SessionTreeNode) -> Result<Value> {
    let entry = serde_json::to_value(&node.entry).map_err(je)?;
    let children = node
        .children
        .iter()
        .map(tree_node_to_json)
        .collect::<Result<Vec<Value>>>()?;
    let mut obj = Map::new();
    obj.insert("entry".to_string(), entry);
    obj.insert("children".to_string(), Value::Array(children));
    if let Some(label) = &node.label {
        obj.insert("label".to_string(), json!(label));
    }
    if let Some(timestamp) = &node.label_timestamp {
        obj.insert("labelTimestamp".to_string(), json!(timestamp));
    }
    Ok(Value::Object(obj))
}

/// Render a [`sm::SessionInfo`] to pi's `SessionInfo` JSON. `created`/`modified`
/// cross as ISO strings; the shim rehydrates them into `Date`. Optional
/// `name`/`parentSessionPath` are omitted when absent.
fn session_info_to_json(info: &sm::SessionInfo) -> Value {
    let mut obj = Map::new();
    obj.insert("path".to_string(), json!(info.path));
    obj.insert("id".to_string(), json!(info.id));
    obj.insert("cwd".to_string(), json!(info.cwd));
    if let Some(name) = &info.name {
        obj.insert("name".to_string(), json!(name));
    }
    if let Some(parent) = &info.parent_session_path {
        obj.insert("parentSessionPath".to_string(), json!(parent));
    }
    obj.insert("created".to_string(), json!(info.created));
    obj.insert("modified".to_string(), json!(info.modified));
    obj.insert("messageCount".to_string(), json!(info.message_count));
    obj.insert("firstMessage".to_string(), json!(info.first_message));
    obj.insert("allMessagesText".to_string(), json!(info.all_messages_text));
    Value::Object(obj)
}

// ===========================================================================
// Module free functions
// ===========================================================================

/// `migrateSessionEntries` (in-place). Takes a `JSON.stringify`d `FileEntry[]`
/// and returns the migrated array as JSON; the shim splices it back into the
/// caller's array to preserve pi's mutate-in-place contract.
#[napi(js_name = "migrateSessionEntries")]
pub fn migrate_session_entries(entries_json: String) -> Result<String> {
    let mut entries: Vec<Value> = serde_json::from_str(&entries_json).map_err(je)?;
    sm::migrate_session_entries(&mut entries);
    serde_json::to_string(&entries).map_err(je)
}

/// `buildContextEntries(entries, leafId?)`. The `byId` cache third arg pi accepts
/// is an optimization only and is not threaded across the boundary.
#[napi(js_name = "buildContextEntries")]
pub fn build_context_entries(entries_json: String, leaf_id: Option<String>) -> Result<String> {
    let entries: Vec<sm::SessionEntry> = serde_json::from_str(&entries_json).map_err(je)?;
    let out = sm::build_context_entries(&entries, leaf_id.as_deref());
    serde_json::to_string(&out).map_err(je)
}

/// `buildSessionContext(entries, leafId?)`.
#[napi(js_name = "buildSessionContext")]
pub fn build_session_context(entries_json: String, leaf_id: Option<String>) -> Result<String> {
    let entries: Vec<sm::SessionEntry> = serde_json::from_str(&entries_json).map_err(je)?;
    let ctx = sm::build_session_context(&entries, leaf_id.as_deref());
    serde_json::to_string(&session_context_to_json(&ctx)).map_err(je)
}

/// `findMostRecentSession(sessionDir, cwd?)`. Returns the path or `null`.
#[napi(js_name = "findMostRecentSession")]
pub fn find_most_recent_session(session_dir: String, cwd: Option<String>) -> Option<String> {
    sm::find_most_recent_session(&session_dir, cwd.as_deref())
}

/// `loadEntriesFromFile(path)`. Returns the parsed `FileEntry[]` as JSON.
#[napi(js_name = "loadEntriesFromFile")]
pub fn load_entries_from_file(path: String) -> Result<String> {
    serde_json::to_string(&sm::load_entries_from_file(&path)).map_err(je)
}

/// `SessionManager.list(cwd, sessionDir?)`. Returns `SessionInfo[]` as JSON.
#[napi(js_name = "sessionManagerList")]
pub fn session_manager_list(cwd: String, session_dir: Option<String>) -> Result<String> {
    let infos = sm::SessionManager::list(&cwd, session_dir.as_deref());
    let arr: Vec<Value> = infos.iter().map(session_info_to_json).collect();
    serde_json::to_string(&Value::Array(arr)).map_err(je)
}

/// `SessionManager.listAll(sessionDir?)`. Returns `SessionInfo[]` as JSON.
#[napi(js_name = "sessionManagerListAll")]
pub fn session_manager_list_all(session_dir: Option<String>) -> Result<String> {
    let infos = sm::SessionManager::list_all(session_dir.as_deref());
    let arr: Vec<Value> = infos.iter().map(session_info_to_json).collect();
    serde_json::to_string(&Value::Array(arr)).map_err(je)
}

// ===========================================================================
// SessionManagerCore — the stateful class
// ===========================================================================

/// The Rust-backed coding-agent `SessionManager`, exposed to JavaScript as
/// `SessionManagerCore`. The JS shim's `SessionManager` class constructs one via
/// the factory methods and delegates every instance method here.
#[napi(js_name = "SessionManagerCore")]
pub struct SessionManagerCore {
    /// The wrapped manager. `RefCell` because the port's mutators take
    /// `&mut self` while napi hands class methods a shared `&self`.
    inner: RefCell<sm::SessionManager>,
}

#[napi]
impl SessionManagerCore {
    /// `SessionManager.inMemory(cwd, options?)`.
    #[napi(factory, js_name = "inMemory")]
    pub fn in_memory(cwd: String, options_json: Option<String>) -> Result<Self> {
        let options = parse_options(options_json)?;
        let mut manager = sm::SessionManager::in_memory(&cwd);
        // A bare in-memory session already generated an id; re-run `newSession`
        // only to honor an explicit id / parentSession, matching pi's
        // constructor calling `newSession(options)`.
        if options.id.is_some() || options.parent_session.is_some() {
            manager.new_session(options).map_err(Error::from_reason)?;
        }
        Ok(Self {
            inner: RefCell::new(manager),
        })
    }

    /// `SessionManager.create(cwd, sessionDir?, options?)`.
    #[napi(factory, js_name = "create")]
    pub fn create(
        cwd: String,
        session_dir: Option<String>,
        options_json: Option<String>,
    ) -> Result<Self> {
        let options = parse_options(options_json)?;
        let mut manager =
            sm::SessionManager::create(&cwd, session_dir.as_deref(), options.id.as_deref());
        // The Rust `create` threads only the id; re-run `newSession` to honor a
        // `parentSession` option (untested, but faithful to pi's create path).
        if options.parent_session.is_some() {
            let id = manager.get_session_id().to_string();
            manager
                .new_session(NewSessionOptions {
                    id: Some(id),
                    parent_session: options.parent_session,
                })
                .map_err(Error::from_reason)?;
        }
        Ok(Self {
            inner: RefCell::new(manager),
        })
    }

    /// `SessionManager.open(path, sessionDir?, cwdOverride?)`. The `sessionDir`
    /// override is accepted for signature parity but not threaded (the port
    /// derives the directory from the file's parent — see the module docs);
    /// `cwdOverride` IS honored, matching pi's effective-cwd resolution
    /// (`cwdOverride ?? header.cwd ?? process.cwd()`).
    #[napi(factory, js_name = "open")]
    pub fn open(
        path: String,
        _session_dir: Option<String>,
        cwd_override: Option<String>,
    ) -> Result<Self> {
        let mut manager = sm::SessionManager::open(&path).map_err(Error::from_reason)?;
        if let Some(cwd) = cwd_override {
            manager.set_cwd(&cwd);
        }
        Ok(Self {
            inner: RefCell::new(manager),
        })
    }

    /// `SessionManager.continueRecent(cwd, sessionDir?)`.
    #[napi(factory, js_name = "continueRecent")]
    pub fn continue_recent(cwd: String, session_dir: Option<String>) -> Self {
        Self {
            inner: RefCell::new(sm::SessionManager::continue_recent(
                &cwd,
                session_dir.as_deref(),
            )),
        }
    }

    /// `SessionManager.forkFrom(sourcePath, targetCwd, sessionDir?, options?)`.
    #[napi(factory, js_name = "forkFrom")]
    pub fn fork_from(
        source_path: String,
        target_cwd: String,
        session_dir: Option<String>,
        options_json: Option<String>,
    ) -> Result<Self> {
        let options = parse_options(options_json)?;
        let manager = sm::SessionManager::fork_from(
            &source_path,
            &target_cwd,
            session_dir.as_deref(),
            options,
        )
        .map_err(Error::from_reason)?;
        Ok(Self {
            inner: RefCell::new(manager),
        })
    }

    /// `newSession(options?)`: reset and start a fresh session. Returns the
    /// (would-be) session file path or `null`; throws on an invalid custom id.
    #[napi(js_name = "newSession")]
    pub fn new_session(&self, options_json: Option<String>) -> Result<Option<String>> {
        let options = parse_options(options_json)?;
        self.inner
            .borrow_mut()
            .new_session(options)
            .map_err(Error::from_reason)
    }

    // --- accessors ----------------------------------------------------------

    #[napi(js_name = "isPersisted")]
    pub fn is_persisted(&self) -> bool {
        self.inner.borrow().is_persisted()
    }

    #[napi(js_name = "getCwd")]
    pub fn get_cwd(&self) -> String {
        self.inner.borrow().get_cwd().to_string()
    }

    #[napi(js_name = "getSessionDir")]
    pub fn get_session_dir(&self) -> String {
        self.inner.borrow().get_session_dir().to_string()
    }

    #[napi(js_name = "usesDefaultSessionDir")]
    pub fn uses_default_session_dir(&self) -> bool {
        self.inner.borrow().uses_default_session_dir()
    }

    #[napi(js_name = "getSessionId")]
    pub fn get_session_id(&self) -> String {
        self.inner.borrow().get_session_id().to_string()
    }

    #[napi(js_name = "getSessionFile")]
    pub fn get_session_file(&self) -> Option<String> {
        self.inner.borrow().get_session_file().map(str::to_string)
    }

    #[napi(js_name = "getLeafId")]
    pub fn get_leaf_id(&self) -> Option<String> {
        self.inner.borrow().get_leaf_id().map(str::to_string)
    }

    #[napi(js_name = "getHeader")]
    pub fn get_header(&self) -> Result<Option<String>> {
        match self.inner.borrow().get_header() {
            Some(header) => Ok(Some(serde_json::to_string(header).map_err(je)?)),
            None => Ok(None),
        }
    }

    #[napi(js_name = "getSessionName")]
    pub fn get_session_name(&self) -> Option<String> {
        self.inner.borrow().get_session_name()
    }

    // --- append operations --------------------------------------------------

    #[napi(js_name = "appendMessage")]
    pub fn append_message(&self, message_json: String) -> Result<String> {
        let message: Value = serde_json::from_str(&message_json).map_err(je)?;
        Ok(self.inner.borrow_mut().append_message(message))
    }

    #[napi(js_name = "appendThinkingLevelChange")]
    pub fn append_thinking_level_change(&self, thinking_level: String) -> String {
        self.inner
            .borrow_mut()
            .append_thinking_level_change(&thinking_level)
    }

    #[napi(js_name = "appendModelChange")]
    pub fn append_model_change(&self, provider: String, model_id: String) -> String {
        self.inner
            .borrow_mut()
            .append_model_change(&provider, &model_id)
    }

    #[napi(js_name = "appendCompaction")]
    pub fn append_compaction(
        &self,
        summary: String,
        first_kept_entry_id: String,
        tokens_before: f64,
        details_json: Option<String>,
        from_hook: Option<bool>,
    ) -> Result<String> {
        let details = parse_value(details_json)?;
        Ok(self.inner.borrow_mut().append_compaction(
            &summary,
            &first_kept_entry_id,
            tokens_before as i64,
            details,
            from_hook,
        ))
    }

    #[napi(js_name = "appendCustomEntry")]
    pub fn append_custom_entry(
        &self,
        custom_type: String,
        data_json: Option<String>,
    ) -> Result<String> {
        let data = parse_value(data_json)?;
        Ok(self
            .inner
            .borrow_mut()
            .append_custom_entry(&custom_type, data))
    }

    #[napi(js_name = "appendSessionInfo")]
    pub fn append_session_info(&self, name: String) -> String {
        self.inner.borrow_mut().append_session_info(&name)
    }

    #[napi(js_name = "appendCustomMessageEntry")]
    pub fn append_custom_message_entry(
        &self,
        custom_type: String,
        content_json: String,
        display: bool,
        details_json: Option<String>,
    ) -> Result<String> {
        let content: Value = serde_json::from_str(&content_json).map_err(je)?;
        let details = parse_value(details_json)?;
        Ok(self.inner.borrow_mut().append_custom_message_entry(
            &custom_type,
            content,
            display,
            details,
        ))
    }

    #[napi(js_name = "appendLabelChange")]
    pub fn append_label_change(&self, target_id: String, label: Option<String>) -> Result<String> {
        self.inner
            .borrow_mut()
            .append_label_change(&target_id, label.as_deref())
            .map_err(|error| Error::from_reason(error.to_string()))
    }

    // --- tree navigation ----------------------------------------------------

    #[napi(js_name = "getLeafEntry")]
    pub fn get_leaf_entry(&self) -> Result<Option<String>> {
        match self.inner.borrow().get_leaf_entry() {
            Some(entry) => Ok(Some(serde_json::to_string(&entry).map_err(je)?)),
            None => Ok(None),
        }
    }

    #[napi(js_name = "getEntry")]
    pub fn get_entry(&self, id: String) -> Result<Option<String>> {
        match self.inner.borrow().get_entry(&id) {
            Some(entry) => Ok(Some(serde_json::to_string(&entry).map_err(je)?)),
            None => Ok(None),
        }
    }

    #[napi(js_name = "getChildren")]
    pub fn get_children(&self, parent_id: String) -> Result<String> {
        serde_json::to_string(&self.inner.borrow().get_children(&parent_id)).map_err(je)
    }

    #[napi(js_name = "getLabel")]
    pub fn get_label(&self, id: String) -> Option<String> {
        self.inner.borrow().get_label(&id)
    }

    #[napi(js_name = "getBranch")]
    pub fn get_branch(&self, from_id: Option<String>) -> Result<String> {
        serde_json::to_string(&self.inner.borrow().get_branch(from_id.as_deref())).map_err(je)
    }

    #[napi(js_name = "buildContextEntries")]
    pub fn build_context_entries(&self) -> Result<String> {
        serde_json::to_string(&self.inner.borrow().build_context_entries()).map_err(je)
    }

    #[napi(js_name = "buildSessionContext")]
    pub fn build_session_context(&self) -> Result<String> {
        let ctx = self.inner.borrow().build_session_context();
        serde_json::to_string(&session_context_to_json(&ctx)).map_err(je)
    }

    #[napi(js_name = "getEntries")]
    pub fn get_entries(&self) -> Result<String> {
        serde_json::to_string(&self.inner.borrow().get_entries()).map_err(je)
    }

    #[napi(js_name = "getTree")]
    pub fn get_tree(&self) -> Result<String> {
        let tree = self.inner.borrow().get_tree();
        let nodes = tree
            .iter()
            .map(tree_node_to_json)
            .collect::<Result<Vec<Value>>>()?;
        serde_json::to_string(&Value::Array(nodes)).map_err(je)
    }

    // --- branching ----------------------------------------------------------

    #[napi(js_name = "branch")]
    pub fn branch(&self, branch_from_id: String) -> Result<()> {
        self.inner
            .borrow_mut()
            .branch(&branch_from_id)
            .map_err(|error| Error::from_reason(error.to_string()))
    }

    #[napi(js_name = "resetLeaf")]
    pub fn reset_leaf(&self) {
        self.inner.borrow_mut().reset_leaf();
    }

    #[napi(js_name = "branchWithSummary")]
    pub fn branch_with_summary(
        &self,
        branch_from_id: Option<String>,
        summary: String,
        details_json: Option<String>,
        from_hook: Option<bool>,
    ) -> Result<String> {
        let details = parse_value(details_json)?;
        self.inner
            .borrow_mut()
            .branch_with_summary(branch_from_id.as_deref(), &summary, details, from_hook)
            .map_err(|error| Error::from_reason(error.to_string()))
    }

    #[napi(js_name = "createBranchedSession")]
    pub fn create_branched_session(&self, leaf_id: String) -> Result<Option<String>> {
        self.inner
            .borrow_mut()
            .create_branched_session(&leaf_id)
            .map_err(|error| Error::from_reason(error.to_string()))
    }
}
