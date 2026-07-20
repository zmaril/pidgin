//! The [`SettingsManager`] itself: load/merge/precedence, typed accessors, the
//! modified-field-tracked persist path, and reload/trust transitions. Ported
//! from the `SettingsManager` class in
//! `packages/coding-agent/src/core/settings-manager.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use pidgin_agent::harness::events::Transport;
use pidgin_agent::harness::session::uuidv7;
use pidgin_agent::types::ThinkingLevel;

use crate::core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS;
use crate::utils::paths::{normalize_path, PathInputOptions};

use super::*;

/// Layered settings manager. Merges a global scope over a project scope
/// (project wins), tracks which fields were modified in-session so a save only
/// overrides those (preserving external edits to untouched keys), and records
/// load errors for [`SettingsManager::drain_errors`].
pub struct SettingsManager {
    storage: Box<dyn SettingsStorage>,
    global_settings: Settings,
    project_settings: Settings,
    settings: Settings,
    project_trusted: bool,
    modified_fields: HashSet<String>,
    modified_nested_fields: HashMap<String, HashSet<String>>,
    modified_project_fields: HashSet<String>,
    modified_project_nested_fields: HashMap<String, HashSet<String>>,
    global_settings_load_error: Option<String>,
    project_settings_load_error: Option<String>,
    errors: Vec<SettingsError>,
    /// A `Send + Sync` live mirror of [`SettingsManager::get_block_images`],
    /// kept in sync at every merge-affecting mutation. The `!Send`
    /// [`SettingsManager`] cannot be captured by the Agent's `Send + Sync`
    /// `convertToLlm` closure, so the block-images filter reads this atomic
    /// instead — see [`crate::core::block_images`].
    block_images: Arc<AtomicBool>,
}

impl SettingsManager {
    // -- construction -------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn new(
        storage: Box<dyn SettingsStorage>,
        initial_global: Settings,
        initial_project: Settings,
        global_load_error: Option<String>,
        project_load_error: Option<String>,
        initial_errors: Vec<SettingsError>,
        project_trusted: bool,
    ) -> Self {
        let settings = deep_merge_settings(&initial_global, &initial_project);
        let block_images = settings
            .get_nested_bool("images", "blockImages")
            .unwrap_or(false);
        Self {
            storage,
            global_settings: initial_global,
            project_settings: initial_project,
            settings,
            project_trusted,
            modified_fields: HashSet::new(),
            modified_nested_fields: HashMap::new(),
            modified_project_fields: HashSet::new(),
            modified_project_nested_fields: HashMap::new(),
            global_settings_load_error: global_load_error,
            project_settings_load_error: project_load_error,
            errors: initial_errors,
            block_images: Arc::new(AtomicBool::new(block_images)),
        }
    }

    /// Create a file-backed manager for `cwd` / `agent_dir` (project trusted).
    pub fn create(cwd: &str, agent_dir: &str) -> Self {
        Self::create_with_options(cwd, agent_dir, SettingsManagerCreateOptions::default())
    }

    /// Create a file-backed manager with explicit options.
    pub fn create_with_options(
        cwd: &str,
        agent_dir: &str,
        options: SettingsManagerCreateOptions,
    ) -> Self {
        let storage = FileSettingsStorage::new(cwd, agent_dir);
        Self::from_storage(Box::new(storage), options)
    }

    /// Create a manager from an arbitrary storage backend.
    pub fn from_storage(
        storage: Box<dyn SettingsStorage>,
        options: SettingsManagerCreateOptions,
    ) -> Self {
        let project_trusted = options.project_trusted.unwrap_or(true);
        let (global_settings, global_error) =
            Self::try_load(storage.as_ref(), SettingsScope::Global, true);
        let (project_settings, project_error) =
            Self::try_load(storage.as_ref(), SettingsScope::Project, project_trusted);

        let mut initial_errors = Vec::new();
        if let Some(err) = &global_error {
            initial_errors.push(SettingsError {
                scope: SettingsScope::Global,
                message: err.clone(),
            });
        }
        if let Some(err) = &project_error {
            initial_errors.push(SettingsError {
                scope: SettingsScope::Project,
                message: err.clone(),
            });
        }

        Self::new(
            storage,
            global_settings,
            project_settings,
            global_error,
            project_error,
            initial_errors,
            project_trusted,
        )
    }

    /// Create an in-memory manager seeded with `settings` in the global scope.
    pub fn in_memory(settings: Settings, options: SettingsManagerCreateOptions) -> Self {
        let storage = InMemorySettingsStorage::new();
        let initial = migrate_settings(settings.into_map());
        let json = serde_json::to_string_pretty(&Value::Object(initial.into_map()))
            .unwrap_or_else(|_| "{}".to_string());
        storage.with_lock(SettingsScope::Global, &mut |_| Some(json.clone()));
        Self::from_storage(Box::new(storage), options)
    }

    // -- load helpers -------------------------------------------------------

    fn load_from_storage(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        project_trusted: bool,
    ) -> Result<Settings, String> {
        if scope == SettingsScope::Project && !project_trusted {
            return Ok(Settings::empty());
        }

        let mut content: Option<String> = None;
        storage.with_lock(scope, &mut |current| {
            content = current.map(str::to_string);
            None
        });

        match content {
            Some(c) if !c.is_empty() => {
                let value: Value = serde_json::from_str(&c).map_err(|e| e.to_string())?;
                let map = value.as_object().cloned().unwrap_or_default();
                Ok(migrate_settings(map))
            }
            _ => Ok(Settings::empty()),
        }
    }

    fn try_load(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        project_trusted: bool,
    ) -> (Settings, Option<String>) {
        match Self::load_from_storage(storage, scope, project_trusted) {
            Ok(settings) => (settings, None),
            Err(err) => (Settings::empty(), Some(err)),
        }
    }

    // -- reads --------------------------------------------------------------

    /// A clone of the global scope.
    pub fn get_global_settings(&self) -> Settings {
        self.global_settings.clone()
    }

    /// A clone of the project scope.
    pub fn get_project_settings(&self) -> Settings {
        self.project_settings.clone()
    }

    /// Whether the project scope is trusted.
    pub fn is_project_trusted(&self) -> bool {
        self.project_trusted
    }

    /// The merged (effective) settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    // -- trust / reload -----------------------------------------------------

    /// Toggle project trust, reloading (or clearing) the project scope.
    pub fn set_project_trusted(&mut self, trusted: bool) {
        if self.project_trusted == trusted {
            return;
        }

        self.project_trusted = trusted;
        self.modified_project_fields.clear();
        self.modified_project_nested_fields.clear();

        if !trusted {
            self.project_settings = Settings::empty();
            self.project_settings_load_error = None;
            self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);
            return;
        }

        let (project_settings, project_error) =
            Self::try_load(self.storage.as_ref(), SettingsScope::Project, trusted);
        self.project_settings = project_settings;
        self.project_settings_load_error = project_error.clone();
        if let Some(err) = project_error {
            self.record_error(SettingsScope::Project, err);
        }
        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);
    }

    /// Reload both scopes from storage, keeping previous values on parse error.
    pub fn reload(&mut self) {
        let (global_settings, global_error) =
            Self::try_load(self.storage.as_ref(), SettingsScope::Global, true);
        match global_error {
            None => {
                self.global_settings = global_settings;
                self.global_settings_load_error = None;
            }
            Some(err) => {
                self.global_settings_load_error = Some(err.clone());
                self.record_error(SettingsScope::Global, err);
            }
        }

        self.modified_fields.clear();
        self.modified_nested_fields.clear();
        self.modified_project_fields.clear();
        self.modified_project_nested_fields.clear();

        let (project_settings, project_error) = Self::try_load(
            self.storage.as_ref(),
            SettingsScope::Project,
            self.project_trusted,
        );
        match project_error {
            None => {
                self.project_settings = project_settings;
                self.project_settings_load_error = None;
            }
            Some(err) => {
                self.project_settings_load_error = Some(err.clone());
                self.record_error(SettingsScope::Project, err);
            }
        }

        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);
        self.sync_block_images_flag();
    }

    /// Apply additional overrides on top of the current merged settings.
    pub fn apply_overrides(&mut self, overrides: Settings) {
        self.settings = deep_merge_settings(&self.settings, &overrides);
        self.sync_block_images_flag();
    }

    /// Refresh the [`SettingsManager::block_images`] atomic from the merged
    /// settings so the shared handle stays a live mirror of
    /// [`SettingsManager::get_block_images`] after any merge-affecting mutation.
    fn sync_block_images_flag(&self) {
        self.block_images
            .store(self.get_block_images(), Ordering::Relaxed);
    }

    /// A shared, `Send + Sync` handle that tracks
    /// [`SettingsManager::get_block_images`] live. Handed to the Agent's
    /// `convertToLlm` closure, which cannot capture this `!Send` manager — see
    /// [`crate::core::block_images`].
    pub fn block_images_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.block_images)
    }

    /// Await pending writes. Writes are synchronous in this port, so this is a
    /// no-op kept for API parity with pi's async `flush`.
    pub fn flush(&self) {}

    /// Drain and clear the collected load/write errors.
    pub fn drain_errors(&mut self) -> Vec<SettingsError> {
        std::mem::take(&mut self.errors)
    }

    // -- modification tracking / persistence --------------------------------

    fn mark_modified(&mut self, field: &str, nested_key: Option<&str>) {
        self.modified_fields.insert(field.to_string());
        if let Some(nested) = nested_key {
            self.modified_nested_fields
                .entry(field.to_string())
                .or_default()
                .insert(nested.to_string());
        }
    }

    fn mark_project_modified(&mut self, field: &str, nested_key: Option<&str>) {
        self.modified_project_fields.insert(field.to_string());
        if let Some(nested) = nested_key {
            self.modified_project_nested_fields
                .entry(field.to_string())
                .or_default()
                .insert(nested.to_string());
        }
    }

    fn assert_project_trusted_for_write(&self) -> anyhow::Result<()> {
        if !self.project_trusted {
            anyhow::bail!("Project is not trusted; refusing to write project settings");
        }
        Ok(())
    }

    fn record_error(&mut self, scope: SettingsScope, message: String) {
        self.errors.push(SettingsError { scope, message });
    }

    fn clear_modified_scope(&mut self, scope: SettingsScope) {
        match scope {
            SettingsScope::Global => {
                self.modified_fields.clear();
                self.modified_nested_fields.clear();
            }
            SettingsScope::Project => {
                self.modified_project_fields.clear();
                self.modified_project_nested_fields.clear();
            }
        }
    }

    /// Read the current file, migrate it, and overlay only the modified fields
    /// (nested fields overlay only their modified sub-keys). Faithful to pi's
    /// `persistScopedSettings` — this is what preserves external edits.
    fn persist_scoped_settings(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        snapshot: &Settings,
        modified_fields: &HashSet<String>,
        modified_nested_fields: &HashMap<String, HashSet<String>>,
    ) -> Result<(), String> {
        let mut parse_err: Option<String> = None;

        storage.with_lock(scope, &mut |current| {
            let current_file: Map<String, Value> = match current {
                Some(c) if !c.is_empty() => match serde_json::from_str::<Value>(c) {
                    Ok(value) => {
                        migrate_settings(value.as_object().cloned().unwrap_or_default()).into_map()
                    }
                    Err(err) => {
                        parse_err = Some(err.to_string());
                        return None;
                    }
                },
                _ => Map::new(),
            };

            let mut merged = current_file.clone();
            for field in modified_fields {
                let value = snapshot.get(field);
                let nested = modified_nested_fields.get(field);
                match (nested, value) {
                    (Some(nested_set), Some(v)) if v.is_object() => {
                        let mut merged_nested = current_file
                            .get(field)
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default();
                        let in_memory_nested = v.as_object().expect("checked is_object");
                        for nested_key in nested_set {
                            match in_memory_nested.get(nested_key) {
                                Some(nested_value) => {
                                    merged_nested.insert(nested_key.clone(), nested_value.clone());
                                }
                                None => {
                                    merged_nested.remove(nested_key);
                                }
                            }
                        }
                        merged.insert(field.clone(), Value::Object(merged_nested));
                    }
                    _ => match value {
                        Some(v) => {
                            merged.insert(field.clone(), v.clone());
                        }
                        None => {
                            merged.remove(field);
                        }
                    },
                }
            }

            Some(
                serde_json::to_string_pretty(&Value::Object(merged))
                    .unwrap_or_else(|_| "{}".to_string()),
            )
        });

        match parse_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn save(&mut self) {
        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);

        if self.global_settings_load_error.is_some() {
            return;
        }

        let snapshot = self.global_settings.clone();
        let fields = self.modified_fields.clone();
        let nested = self.modified_nested_fields.clone();
        match Self::persist_scoped_settings(
            self.storage.as_ref(),
            SettingsScope::Global,
            &snapshot,
            &fields,
            &nested,
        ) {
            Ok(()) => self.clear_modified_scope(SettingsScope::Global),
            Err(err) => self.record_error(SettingsScope::Global, err),
        }
    }

    fn save_project_settings(&mut self, settings: Settings) -> anyhow::Result<()> {
        self.assert_project_trusted_for_write()?;
        self.project_settings = settings;
        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);

        if self.project_settings_load_error.is_some() {
            return Ok(());
        }

        let snapshot = self.project_settings.clone();
        let fields = self.modified_project_fields.clone();
        let nested = self.modified_project_nested_fields.clone();
        match Self::persist_scoped_settings(
            self.storage.as_ref(),
            SettingsScope::Project,
            &snapshot,
            &fields,
            &nested,
        ) {
            Ok(()) => self.clear_modified_scope(SettingsScope::Project),
            Err(err) => self.record_error(SettingsScope::Project, err),
        }
        Ok(())
    }

    fn update_project_settings(
        &mut self,
        field: &str,
        update: impl FnOnce(&mut Settings),
    ) -> anyhow::Result<()> {
        self.assert_project_trusted_for_write()?;
        let mut project_settings = self.project_settings.clone();
        update(&mut project_settings);
        self.mark_project_modified(field, None);
        self.save_project_settings(project_settings)
    }

    /// Set a global scalar field (or remove it when `value` is `None`) and save.
    fn set_global(&mut self, field: &str, value: Option<Value>) {
        self.global_settings.set_opt(field, value);
        self.mark_modified(field, None);
        self.save();
    }

    /// Set a nested key under a global object field and save.
    fn set_global_nested(&mut self, field: &str, nested: &str, value: Value) {
        self.global_settings
            .nested_mut(field)
            .insert(nested.to_string(), value);
        self.mark_modified(field, Some(nested));
        self.save();
    }

    // -- typed accessors ----------------------------------------------------

    pub fn get_last_changelog_version(&self) -> Option<String> {
        self.settings.get_str("lastChangelogVersion")
    }

    pub fn set_last_changelog_version(&mut self, version: &str) {
        self.set_global("lastChangelogVersion", Some(json!(version)));
    }

    pub fn get_session_dir(&self) -> Option<String> {
        let session_dir = self.settings.get_str("sessionDir")?;
        Some(normalize_path(&session_dir, &PathInputOptions::default()).unwrap_or(session_dir))
    }

    pub fn get_default_provider(&self) -> Option<String> {
        self.settings.get_str("defaultProvider")
    }

    pub fn get_default_model(&self) -> Option<String> {
        self.settings.get_str("defaultModel")
    }

    pub fn set_default_provider(&mut self, provider: &str) {
        self.set_global("defaultProvider", Some(json!(provider)));
    }

    pub fn set_default_model(&mut self, model_id: &str) {
        self.set_global("defaultModel", Some(json!(model_id)));
    }

    pub fn set_default_model_and_provider(&mut self, provider: &str, model_id: &str) {
        self.global_settings.set("defaultProvider", json!(provider));
        self.global_settings.set("defaultModel", json!(model_id));
        self.mark_modified("defaultProvider", None);
        self.mark_modified("defaultModel", None);
        self.save();
    }

    pub fn get_steering_mode(&self) -> String {
        self.settings
            .get_str("steeringMode")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "one-at-a-time".to_string())
    }

    pub fn set_steering_mode(&mut self, mode: &str) {
        self.set_global("steeringMode", Some(json!(mode)));
    }

    pub fn get_follow_up_mode(&self) -> String {
        self.settings
            .get_str("followUpMode")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "one-at-a-time".to_string())
    }

    pub fn set_follow_up_mode(&mut self, mode: &str) {
        self.set_global("followUpMode", Some(json!(mode)));
    }

    pub fn get_theme_setting(&self) -> Option<String> {
        self.settings.get_str("theme")
    }

    pub fn get_theme(&self) -> Option<String> {
        self.get_theme_setting().filter(|t| !t.contains('/'))
    }

    pub fn set_theme(&mut self, theme: &str) {
        self.set_global("theme", Some(json!(theme)));
    }

    pub fn get_default_thinking_level(&self) -> Option<ThinkingLevel> {
        self.settings
            .get("defaultThinkingLevel")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn set_default_thinking_level(&mut self, level: ThinkingLevel) {
        self.set_global(
            "defaultThinkingLevel",
            Some(serde_json::to_value(level).unwrap_or(Value::Null)),
        );
    }

    pub fn get_transport(&self) -> TransportSetting {
        self.settings
            .get("transport")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(Transport::Auto)
    }

    pub fn set_transport(&mut self, transport: TransportSetting) {
        self.set_global(
            "transport",
            Some(serde_json::to_value(transport).unwrap_or(Value::Null)),
        );
    }

    pub fn get_compaction_enabled(&self) -> bool {
        self.settings
            .get_nested_bool("compaction", "enabled")
            .unwrap_or(true)
    }

    pub fn set_compaction_enabled(&mut self, enabled: bool) {
        self.set_global_nested("compaction", "enabled", json!(enabled));
    }

    pub fn get_compaction_reserve_tokens(&self) -> i64 {
        self.settings
            .get_nested_f64("compaction", "reserveTokens")
            .map(|n| n as i64)
            .unwrap_or(16384)
    }

    pub fn get_compaction_keep_recent_tokens(&self) -> i64 {
        self.settings
            .get_nested_f64("compaction", "keepRecentTokens")
            .map(|n| n as i64)
            .unwrap_or(20000)
    }

    pub fn get_compaction_settings(&self) -> CompactionResolved {
        CompactionResolved {
            enabled: self.get_compaction_enabled(),
            reserve_tokens: self.get_compaction_reserve_tokens(),
            keep_recent_tokens: self.get_compaction_keep_recent_tokens(),
        }
    }

    pub fn get_branch_summary_settings(&self) -> BranchSummaryResolved {
        BranchSummaryResolved {
            reserve_tokens: self
                .settings
                .get_nested_f64("branchSummary", "reserveTokens")
                .map(|n| n as i64)
                .unwrap_or(16384),
            skip_prompt: self.get_branch_summary_skip_prompt(),
        }
    }

    pub fn get_branch_summary_skip_prompt(&self) -> bool {
        self.settings
            .get_nested_bool("branchSummary", "skipPrompt")
            .unwrap_or(false)
    }

    pub fn get_retry_enabled(&self) -> bool {
        self.settings
            .get_nested_bool("retry", "enabled")
            .unwrap_or(true)
    }

    pub fn set_retry_enabled(&mut self, enabled: bool) {
        self.set_global_nested("retry", "enabled", json!(enabled));
    }

    pub fn get_retry_settings(&self) -> RetryResolved {
        RetryResolved {
            enabled: self.get_retry_enabled(),
            max_retries: self
                .settings
                .get_nested_f64("retry", "maxRetries")
                .map(|n| n as i64)
                .unwrap_or(3),
            base_delay_ms: self
                .settings
                .get_nested_f64("retry", "baseDelayMs")
                .map(|n| n as i64)
                .unwrap_or(2000),
        }
    }

    pub fn get_http_idle_timeout_ms(&self) -> anyhow::Result<u64> {
        let parsed =
            parse_timeout_setting(self.settings.get("httpIdleTimeoutMs"), "httpIdleTimeoutMs")
                .map_err(anyhow::Error::msg)?;
        Ok(parsed.unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS))
    }

    pub fn set_http_idle_timeout_ms(&mut self, timeout_ms: f64) -> anyhow::Result<()> {
        if !timeout_ms.is_finite() || timeout_ms < 0.0 {
            anyhow::bail!("Invalid httpIdleTimeoutMs setting: {timeout_ms}");
        }
        self.set_global("httpIdleTimeoutMs", Some(json!(timeout_ms.floor() as i64)));
        Ok(())
    }

    pub fn get_provider_retry_settings(&self) -> ProviderRetryResolved {
        let provider = self
            .settings
            .get("retry")
            .and_then(Value::as_object)
            .and_then(|retry| retry.get("provider"))
            .and_then(Value::as_object);
        let field = |name: &str| provider.and_then(|p| p.get(name)).and_then(Value::as_f64);
        ProviderRetryResolved {
            timeout_ms: field("timeoutMs").map(|n| n as i64),
            max_retries: field("maxRetries").map(|n| n as i64),
            max_retry_delay_ms: field("maxRetryDelayMs").map(|n| n as i64).unwrap_or(60000),
        }
    }

    pub fn get_websocket_connect_timeout_ms(&self) -> anyhow::Result<Option<u64>> {
        parse_timeout_setting(
            self.settings.get("websocketConnectTimeoutMs"),
            "websocketConnectTimeoutMs",
        )
        .map_err(anyhow::Error::msg)
    }

    pub fn get_hide_thinking_block(&self) -> bool {
        self.settings.get_bool("hideThinkingBlock").unwrap_or(false)
    }

    pub fn set_hide_thinking_block(&mut self, hide: bool) {
        self.set_global("hideThinkingBlock", Some(json!(hide)));
    }

    pub fn get_show_cache_miss_notices(&self) -> bool {
        self.settings
            .get_bool("showCacheMissNotices")
            .unwrap_or(false)
    }

    pub fn set_show_cache_miss_notices(&mut self, show: bool) {
        self.set_global("showCacheMissNotices", Some(json!(show)));
    }

    pub fn get_external_editor_command(&self) -> String {
        let configured = self.settings.get_str("externalEditor");
        let visual = std::env::var("VISUAL").ok();
        let editor = std::env::var("EDITOR").ok();
        resolve_external_editor(
            configured.as_deref(),
            visual.as_deref(),
            editor.as_deref(),
            cfg!(windows),
        )
    }

    pub fn get_shell_path(&self) -> Option<String> {
        let shell_path = self.settings.get_str("shellPath")?;
        Some(normalize_path(&shell_path, &PathInputOptions::default()).unwrap_or(shell_path))
    }

    pub fn set_shell_path(&mut self, path: Option<&str>) {
        self.set_global("shellPath", path.map(|p| json!(p)));
    }

    pub fn get_quiet_startup(&self) -> bool {
        self.settings.get_bool("quietStartup").unwrap_or(false)
    }

    pub fn set_quiet_startup(&mut self, quiet: bool) {
        self.set_global("quietStartup", Some(json!(quiet)));
    }

    pub fn get_default_project_trust(&self) -> DefaultProjectTrust {
        match self
            .global_settings
            .get_str("defaultProjectTrust")
            .as_deref()
        {
            Some("always") => DefaultProjectTrust::Always,
            Some("never") => DefaultProjectTrust::Never,
            _ => DefaultProjectTrust::Ask,
        }
    }

    pub fn set_default_project_trust(&mut self, trust: DefaultProjectTrust) {
        let value = match trust {
            DefaultProjectTrust::Ask => "ask",
            DefaultProjectTrust::Always => "always",
            DefaultProjectTrust::Never => "never",
        };
        self.set_global("defaultProjectTrust", Some(json!(value)));
    }

    pub fn get_shell_command_prefix(&self) -> Option<String> {
        self.settings.get_str("shellCommandPrefix")
    }

    pub fn set_shell_command_prefix(&mut self, prefix: Option<&str>) {
        self.set_global("shellCommandPrefix", prefix.map(|p| json!(p)));
    }

    pub fn get_npm_command(&self) -> Option<Vec<String>> {
        self.settings.get_str_array("npmCommand")
    }

    pub fn set_npm_command(&mut self, command: Option<Vec<String>>) {
        self.set_global("npmCommand", command.map(|c| json!(c)));
    }

    pub fn get_collapse_changelog(&self) -> bool {
        self.settings.get_bool("collapseChangelog").unwrap_or(false)
    }

    pub fn set_collapse_changelog(&mut self, collapse: bool) {
        self.set_global("collapseChangelog", Some(json!(collapse)));
    }

    pub fn get_enable_install_telemetry(&self) -> bool {
        self.settings
            .get_bool("enableInstallTelemetry")
            .unwrap_or(true)
    }

    pub fn set_enable_install_telemetry(&mut self, enabled: bool) {
        self.set_global("enableInstallTelemetry", Some(json!(enabled)));
    }

    pub fn get_enable_analytics(&self) -> bool {
        self.settings.get_bool("enableAnalytics").unwrap_or(false)
    }

    pub fn get_tracking_id(&self) -> Option<String> {
        self.settings.get_str("trackingId")
    }

    /// Set the analytics opt-in; mint a tracking id on first opt-in.
    pub fn set_enable_analytics(&mut self, enabled: bool) {
        self.global_settings.set("enableAnalytics", json!(enabled));
        self.mark_modified("enableAnalytics", None);
        if enabled && self.global_settings.get("trackingId").is_none() {
            self.global_settings.set("trackingId", json!(uuidv7()));
            self.mark_modified("trackingId", None);
        }
        self.save();
    }

    pub fn get_packages(&self) -> Vec<PackageSource> {
        self.settings
            .get("packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_packages(&mut self, packages: Vec<PackageSource>) {
        self.set_global("packages", Some(Value::Array(packages)));
    }

    pub fn set_project_packages(&mut self, packages: Vec<PackageSource>) -> anyhow::Result<()> {
        self.update_project_settings("packages", |settings| {
            settings.set("packages", Value::Array(packages));
        })
    }

    pub fn get_extension_paths(&self) -> Vec<String> {
        self.settings
            .get_str_array("extensions")
            .unwrap_or_default()
    }

    pub fn set_extension_paths(&mut self, paths: Vec<String>) {
        self.set_global("extensions", Some(json!(paths)));
    }

    pub fn set_project_extension_paths(&mut self, paths: Vec<String>) -> anyhow::Result<()> {
        self.update_project_settings("extensions", |settings| {
            settings.set("extensions", json!(paths));
        })
    }

    pub fn get_skill_paths(&self) -> Vec<String> {
        self.settings.get_str_array("skills").unwrap_or_default()
    }

    pub fn set_skill_paths(&mut self, paths: Vec<String>) {
        self.set_global("skills", Some(json!(paths)));
    }

    pub fn set_project_skill_paths(&mut self, paths: Vec<String>) -> anyhow::Result<()> {
        self.update_project_settings("skills", |settings| {
            settings.set("skills", json!(paths));
        })
    }

    pub fn get_prompt_template_paths(&self) -> Vec<String> {
        self.settings.get_str_array("prompts").unwrap_or_default()
    }

    pub fn set_prompt_template_paths(&mut self, paths: Vec<String>) {
        self.set_global("prompts", Some(json!(paths)));
    }

    pub fn set_project_prompt_template_paths(&mut self, paths: Vec<String>) -> anyhow::Result<()> {
        self.update_project_settings("prompts", |settings| {
            settings.set("prompts", json!(paths));
        })
    }

    pub fn get_theme_paths(&self) -> Vec<String> {
        self.settings.get_str_array("themes").unwrap_or_default()
    }

    pub fn set_theme_paths(&mut self, paths: Vec<String>) {
        self.set_global("themes", Some(json!(paths)));
    }

    pub fn set_project_theme_paths(&mut self, paths: Vec<String>) -> anyhow::Result<()> {
        self.update_project_settings("themes", |settings| {
            settings.set("themes", json!(paths));
        })
    }

    pub fn get_enable_skill_commands(&self) -> bool {
        self.settings
            .get_bool("enableSkillCommands")
            .unwrap_or(true)
    }

    pub fn set_enable_skill_commands(&mut self, enabled: bool) {
        self.set_global("enableSkillCommands", Some(json!(enabled)));
    }

    pub fn get_thinking_budgets(&self) -> Option<ThinkingBudgetsSettings> {
        self.settings
            .get("thinkingBudgets")
            .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
    }

    pub fn get_show_images(&self) -> bool {
        self.settings
            .get_nested_bool("terminal", "showImages")
            .unwrap_or(true)
    }

    pub fn set_show_images(&mut self, show: bool) {
        self.set_global_nested("terminal", "showImages", json!(show));
    }

    pub fn get_image_width_cells(&self) -> i64 {
        match self.settings.get_nested_f64("terminal", "imageWidthCells") {
            Some(width) if width.is_finite() => (width.floor() as i64).max(1),
            _ => 60,
        }
    }

    pub fn set_image_width_cells(&mut self, width: f64) {
        let value = (width.floor() as i64).max(1);
        self.set_global_nested("terminal", "imageWidthCells", json!(value));
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        // Settings take precedence, then env var, then default false.
        if let Some(value) = self.settings.get_nested_bool("terminal", "clearOnShrink") {
            return value;
        }
        std::env::var("PI_CLEAR_ON_SHRINK").ok().as_deref() == Some("1")
    }

    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.set_global_nested("terminal", "clearOnShrink", json!(enabled));
    }

    pub fn get_show_terminal_progress(&self) -> bool {
        self.settings
            .get_nested_bool("terminal", "showTerminalProgress")
            .unwrap_or(false)
    }

    pub fn set_show_terminal_progress(&mut self, enabled: bool) {
        self.set_global_nested("terminal", "showTerminalProgress", json!(enabled));
    }

    pub fn get_image_auto_resize(&self) -> bool {
        self.settings
            .get_nested_bool("images", "autoResize")
            .unwrap_or(true)
    }

    pub fn set_image_auto_resize(&mut self, enabled: bool) {
        self.set_global_nested("images", "autoResize", json!(enabled));
    }

    pub fn get_block_images(&self) -> bool {
        self.settings
            .get_nested_bool("images", "blockImages")
            .unwrap_or(false)
    }

    pub fn set_block_images(&mut self, blocked: bool) {
        self.set_global_nested("images", "blockImages", json!(blocked));
        // Keep the shared live mirror in step so the Agent's `convertToLlm`
        // closure observes the toggle on its next call (pi reads
        // `getBlockImages()` live per call).
        self.sync_block_images_flag();
    }

    pub fn get_enabled_models(&self) -> Option<Vec<String>> {
        self.settings.get_str_array("enabledModels")
    }

    pub fn set_enabled_models(&mut self, patterns: Option<Vec<String>>) {
        self.set_global("enabledModels", patterns.map(|p| json!(p)));
    }

    pub fn get_double_escape_action(&self) -> String {
        self.settings
            .get_str("doubleEscapeAction")
            .unwrap_or_else(|| "tree".to_string())
    }

    pub fn set_double_escape_action(&mut self, action: &str) {
        self.set_global("doubleEscapeAction", Some(json!(action)));
    }

    pub fn get_tree_filter_mode(&self) -> String {
        const VALID: [&str; 5] = ["default", "no-tools", "user-only", "labeled-only", "all"];
        match self.settings.get_str("treeFilterMode") {
            Some(mode) if VALID.contains(&mode.as_str()) => mode,
            _ => "default".to_string(),
        }
    }

    pub fn set_tree_filter_mode(&mut self, mode: &str) {
        self.set_global("treeFilterMode", Some(json!(mode)));
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        if let Some(value) = self.settings.get_bool("showHardwareCursor") {
            return value;
        }
        std::env::var("PI_HARDWARE_CURSOR").ok().as_deref() == Some("1")
    }

    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        self.set_global("showHardwareCursor", Some(json!(enabled)));
    }

    pub fn get_editor_padding_x(&self) -> i64 {
        self.settings
            .get_f64("editorPaddingX")
            .map(|n| n as i64)
            .unwrap_or(0)
    }

    pub fn set_editor_padding_x(&mut self, padding: f64) {
        let value = (padding.floor() as i64).clamp(0, 3);
        self.set_global("editorPaddingX", Some(json!(value)));
    }

    pub fn get_output_pad(&self) -> i64 {
        if self.settings.get_f64("outputPad") == Some(0.0) {
            0
        } else {
            1
        }
    }

    pub fn set_output_pad(&mut self, padding: i64) {
        self.set_global("outputPad", Some(json!(padding)));
    }

    pub fn get_autocomplete_max_visible(&self) -> i64 {
        self.settings
            .get_f64("autocompleteMaxVisible")
            .map(|n| n as i64)
            .unwrap_or(5)
    }

    pub fn set_autocomplete_max_visible(&mut self, max_visible: f64) {
        let value = (max_visible.floor() as i64).clamp(3, 20);
        self.set_global("autocompleteMaxVisible", Some(json!(value)));
    }

    pub fn get_code_block_indent(&self) -> String {
        self.settings
            .get("markdown")
            .and_then(Value::as_object)
            .and_then(|m| m.get("codeBlockIndent"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "  ".to_string())
    }

    pub fn get_warnings(&self) -> WarningSettings {
        self.settings
            .get("warnings")
            .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn set_warnings(&mut self, warnings: WarningSettings) {
        self.set_global(
            "warnings",
            Some(serde_json::to_value(warnings).unwrap_or(Value::Null)),
        );
    }
}
