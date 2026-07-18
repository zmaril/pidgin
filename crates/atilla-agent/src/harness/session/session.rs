//! Session tree operations and context reconstruction, mirroring
//! `packages/agent/src/harness/session/session.ts`.

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;

use super::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};
use super::storage::{now_iso, SessionStorage};
use crate::harness::types::{
    ActiveToolsChangeEntry, AgentMessage, BranchSummaryEntry, CompactionEntry, CustomEntry,
    CustomMessageEntry, LabelEntry, MessageEntry, ModelChangeEntry, ModelRef, SessionContext,
    SessionError, SessionErrorCode, SessionInfoEntry, SessionTreeEntry, ThinkingLevelChangeEntry,
};

/// A transform applied to the context entry list after default compaction
/// selection. Mirrors pi's `ContextEntryTransform`.
pub type ContextEntryTransform = Box<dyn Fn(Vec<SessionTreeEntry>) -> Vec<SessionTreeEntry>>;

/// Projects a `custom` entry into zero or more context messages. Mirrors pi's
/// `CustomEntryContextMessageProjector`.
pub type CustomEntryProjector =
    Box<dyn Fn(&CustomEntry, usize, &[SessionTreeEntry]) -> Vec<AgentMessage>>;

/// Options controlling context construction. Mirrors
/// `SessionContextBuildOptions`.
#[derive(Default)]
pub struct SessionContextBuildOptions {
    pub entry_transforms: Vec<ContextEntryTransform>,
    pub entry_projectors: HashMap<String, CustomEntryProjector>,
}

/// A branch-summary request passed to [`Session::move_to`].
pub struct MoveSummary {
    pub summary: String,
    pub details: Option<Value>,
    pub from_hook: Option<bool>,
}

fn derive_session_context_state(
    path_entries: &[SessionTreeEntry],
) -> (String, Option<ModelRef>, Option<Vec<String>>) {
    let mut thinking_level = "off".to_string();
    let mut model: Option<ModelRef> = None;
    let mut active_tool_names: Option<Vec<String>> = None;

    for entry in path_entries {
        match entry {
            SessionTreeEntry::ThinkingLevelChange(e) => thinking_level = e.thinking_level.clone(),
            SessionTreeEntry::ModelChange(e) => {
                model = Some(ModelRef {
                    provider: e.provider.clone(),
                    model_id: e.model_id.clone(),
                });
            }
            SessionTreeEntry::Message(e)
                if e.message.get("role").and_then(Value::as_str) == Some("assistant") =>
            {
                let provider = e
                    .message
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let model_id = e
                    .message
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                model = Some(ModelRef { provider, model_id });
            }
            SessionTreeEntry::ActiveToolsChange(e) => {
                active_tool_names = Some(e.active_tool_names.clone());
            }
            _ => {}
        }
    }

    (thinking_level, model, active_tool_names)
}

/// Select context entries honoring the latest compaction. Mirrors
/// `defaultContextEntryTransform`.
pub fn default_context_entry_transform(path_entries: &[SessionTreeEntry]) -> Vec<SessionTreeEntry> {
    let mut compaction: Option<&CompactionEntry> = None;
    for entry in path_entries {
        if let SessionTreeEntry::Compaction(e) = entry {
            compaction = Some(e);
        }
    }
    let Some(compaction) = compaction else {
        return path_entries.to_vec();
    };

    let mut entries = vec![SessionTreeEntry::Compaction(compaction.clone())];
    let compaction_idx = path_entries
        .iter()
        .position(|entry| matches!(entry, SessionTreeEntry::Compaction(e) if e.id == compaction.id))
        .expect("compaction entry present");
    let mut found_first_kept = false;
    for entry in &path_entries[..compaction_idx] {
        if entry.id() == compaction.first_kept_entry_id {
            found_first_kept = true;
        }
        if found_first_kept {
            entries.push(entry.clone());
        }
    }
    for entry in &path_entries[compaction_idx + 1..] {
        entries.push(entry.clone());
    }
    entries
}

/// Apply the default compaction transform, then any user transforms in order.
pub fn build_context_entries(
    path_entries: &[SessionTreeEntry],
    options: &SessionContextBuildOptions,
) -> Vec<SessionTreeEntry> {
    let mut entries = default_context_entry_transform(path_entries);
    for transform in &options.entry_transforms {
        entries = transform(entries);
    }
    entries
}

/// Project one entry to context messages. Mirrors
/// `sessionEntryToContextMessages`.
pub fn session_entry_to_context_messages(
    entry: &SessionTreeEntry,
    index: usize,
    entries: &[SessionTreeEntry],
    options: &SessionContextBuildOptions,
) -> Vec<AgentMessage> {
    match entry {
        SessionTreeEntry::Message(e) => vec![e.message.clone()],
        SessionTreeEntry::CustomMessage(e) => vec![create_custom_message(
            &e.custom_type,
            &e.content,
            e.display,
            e.details.as_ref(),
            &e.timestamp,
        )],
        SessionTreeEntry::Compaction(e) => {
            vec![create_compaction_summary_message(
                &e.summary,
                e.tokens_before,
                &e.timestamp,
            )]
        }
        SessionTreeEntry::BranchSummary(e) if !e.summary.is_empty() => {
            vec![create_branch_summary_message(
                &e.summary,
                &e.from_id,
                &e.timestamp,
            )]
        }
        SessionTreeEntry::Custom(e) => options
            .entry_projectors
            .get(&e.custom_type)
            .map(|projector| projector(e, index, entries))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Rebuild a session context from a root-to-leaf path. Mirrors
/// `buildSessionContext`.
pub fn build_session_context(
    path_entries: &[SessionTreeEntry],
    options: &SessionContextBuildOptions,
) -> SessionContext {
    let (thinking_level, model, active_tool_names) = derive_session_context_state(path_entries);
    let context_entries = build_context_entries(path_entries, options);
    let mut messages = Vec::new();
    for (index, entry) in context_entries.iter().enumerate() {
        messages.extend(session_entry_to_context_messages(
            entry,
            index,
            &context_entries,
            options,
        ));
    }
    SessionContext {
        messages,
        thinking_level,
        model,
        active_tool_names,
    }
}

/// A conversation session over a storage backend. Mirrors pi's `Session` class.
pub struct Session {
    storage: Rc<dyn SessionStorage>,
    context_build_options: SessionContextBuildOptions,
}

impl Session {
    /// Wrap a storage handle with default context options.
    pub fn new(storage: Rc<dyn SessionStorage>) -> Self {
        Self {
            storage,
            context_build_options: SessionContextBuildOptions::default(),
        }
    }

    /// Wrap a storage handle with explicit context build options.
    pub fn with_options(
        storage: Rc<dyn SessionStorage>,
        context_build_options: SessionContextBuildOptions,
    ) -> Self {
        Self {
            storage,
            context_build_options,
        }
    }

    pub fn get_metadata(&self) -> crate::harness::types::SessionMetadata {
        self.storage.get_metadata()
    }

    pub fn get_storage(&self) -> Rc<dyn SessionStorage> {
        self.storage.clone()
    }

    pub fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.storage.get_leaf_id()
    }

    pub fn get_entry(&self, id: &str) -> Option<SessionTreeEntry> {
        self.storage.get_entry(id)
    }

    pub fn get_entries(&self) -> Vec<SessionTreeEntry> {
        self.storage.get_entries()
    }

    pub fn get_branch(&self, from_id: Option<&str>) -> Result<Vec<SessionTreeEntry>, SessionError> {
        let leaf_id = match from_id {
            Some(id) => Some(id.to_string()),
            None => self.storage.get_leaf_id()?,
        };
        self.storage.get_path_to_root(leaf_id.as_deref())
    }

    pub fn build_context_entries(&self) -> Result<Vec<SessionTreeEntry>, SessionError> {
        Ok(build_context_entries(
            &self.get_branch(None)?,
            &self.context_build_options,
        ))
    }

    pub fn build_context(&self) -> Result<SessionContext, SessionError> {
        Ok(build_session_context(
            &self.get_branch(None)?,
            &self.context_build_options,
        ))
    }

    pub fn get_label(&self, id: &str) -> Option<String> {
        self.storage.get_label(id)
    }

    pub fn get_session_name(&self) -> Option<String> {
        let entries = self.storage.find_entries("session_info");
        let last = entries.last()?;
        let SessionTreeEntry::SessionInfo(info) = last else {
            return None;
        };
        let trimmed = info.name.as_deref().unwrap_or("").trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn append_typed_entry(&self, entry: SessionTreeEntry) -> Result<String, SessionError> {
        let id = entry.id().to_string();
        self.storage.append_entry(entry)?;
        Ok(id)
    }

    pub fn append_message(&self, message: AgentMessage) -> Result<String, SessionError> {
        self.append_typed_entry(SessionTreeEntry::Message(MessageEntry {
            id: self.storage.create_entry_id(),
            parent_id: self.storage.get_leaf_id()?,
            timestamp: now_iso(),
            message,
        }))
    }

    pub fn append_thinking_level_change(
        &self,
        thinking_level: &str,
    ) -> Result<String, SessionError> {
        self.append_typed_entry(SessionTreeEntry::ThinkingLevelChange(
            ThinkingLevelChangeEntry {
                id: self.storage.create_entry_id(),
                parent_id: self.storage.get_leaf_id()?,
                timestamp: now_iso(),
                thinking_level: thinking_level.to_string(),
            },
        ))
    }

    pub fn append_model_change(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, SessionError> {
        self.append_typed_entry(SessionTreeEntry::ModelChange(ModelChangeEntry {
            id: self.storage.create_entry_id(),
            parent_id: self.storage.get_leaf_id()?,
            timestamp: now_iso(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        }))
    }

    pub fn append_active_tools_change(
        &self,
        active_tool_names: Vec<String>,
    ) -> Result<String, SessionError> {
        self.append_typed_entry(SessionTreeEntry::ActiveToolsChange(
            ActiveToolsChangeEntry {
                id: self.storage.create_entry_id(),
                parent_id: self.storage.get_leaf_id()?,
                timestamp: now_iso(),
                active_tool_names,
            },
        ))
    }

    pub fn append_compaction(
        &self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: i64,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String, SessionError> {
        self.append_typed_entry(SessionTreeEntry::Compaction(CompactionEntry {
            id: self.storage.create_entry_id(),
            parent_id: self.storage.get_leaf_id()?,
            timestamp: now_iso(),
            summary: summary.to_string(),
            first_kept_entry_id: first_kept_entry_id.to_string(),
            tokens_before,
            details,
            from_hook,
        }))
    }

    pub fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        self.append_typed_entry(SessionTreeEntry::Custom(CustomEntry {
            id: self.storage.create_entry_id(),
            parent_id: self.storage.get_leaf_id()?,
            timestamp: now_iso(),
            custom_type: custom_type.to_string(),
            data,
        }))
    }

    pub fn append_custom_message_entry(
        &self,
        custom_type: &str,
        content: Value,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, SessionError> {
        self.append_typed_entry(SessionTreeEntry::CustomMessage(CustomMessageEntry {
            id: self.storage.create_entry_id(),
            parent_id: self.storage.get_leaf_id()?,
            timestamp: now_iso(),
            custom_type: custom_type.to_string(),
            content,
            display,
            details,
        }))
    }

    pub fn append_label(
        &self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, SessionError> {
        if self.storage.get_entry(target_id).is_none() {
            return Err(SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {target_id} not found"),
            ));
        }
        self.append_typed_entry(SessionTreeEntry::Label(LabelEntry {
            id: self.storage.create_entry_id(),
            parent_id: self.storage.get_leaf_id()?,
            timestamp: now_iso(),
            target_id: target_id.to_string(),
            label: label.map(str::to_string),
        }))
    }

    pub fn append_session_name(&self, name: &str) -> Result<String, SessionError> {
        let sanitized = sanitize_session_name(name);
        self.append_typed_entry(SessionTreeEntry::SessionInfo(SessionInfoEntry {
            id: self.storage.create_entry_id(),
            parent_id: self.storage.get_leaf_id()?,
            timestamp: now_iso(),
            name: Some(sanitized),
        }))
    }

    /// Move the active leaf, optionally recording a branch summary. Returns the
    /// summary entry id when a summary is written. Mirrors `moveTo`.
    pub fn move_to(
        &self,
        entry_id: Option<&str>,
        summary: Option<MoveSummary>,
    ) -> Result<Option<String>, SessionError> {
        if let Some(id) = entry_id {
            if self.storage.get_entry(id).is_none() {
                return Err(SessionError::new(
                    SessionErrorCode::NotFound,
                    format!("Entry {id} not found"),
                ));
            }
        }
        self.storage.set_leaf_id(entry_id)?;
        let Some(summary) = summary else {
            return Ok(None);
        };
        let id = self.append_typed_entry(SessionTreeEntry::BranchSummary(BranchSummaryEntry {
            id: self.storage.create_entry_id(),
            parent_id: entry_id.map(str::to_string),
            timestamp: now_iso(),
            from_id: entry_id.unwrap_or("root").to_string(),
            summary: summary.summary,
            details: summary.details,
            from_hook: summary.from_hook,
        }))?;
        Ok(Some(id))
    }
}

/// Normalize a session name: collapse CR/LF runs to a single space, then trim.
/// Mirrors `name.replace(/[\r\n]+/g, " ").trim()`.
fn sanitize_session_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut in_break = false;
    for ch in name.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_break {
                result.push(' ');
                in_break = true;
            }
        } else {
            result.push(ch);
            in_break = false;
        }
    }
    result.trim().to_string()
}
