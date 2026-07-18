//! Layered user/project settings, ported from
//! `packages/coding-agent/src/core/settings-manager.ts`.
//!
//! # Representation
//!
//! pi's `SettingsManager` operates on `Settings` as a `Record<string, unknown>`
//! throughout: [`deep_merge_settings`] iterates `Object.keys`, [`migrate_settings`]
//! mutates arbitrary keys, and the persist path does `{ ...currentFileSettings }`
//! to preserve keys the running process never knew about. That dynamic,
//! unknown-key-preserving behavior is load-bearing — it is exactly what the
//! `settings-manager-bug.test.ts` regression pins (an external edit to a
//! `packages` array must survive a save that touches an unrelated field). A
//! fixed Rust struct would silently drop unknown keys on re-serialization, so
//! [`Settings`] is a newtype over `serde_json::Map<String, Value>`, mirroring
//! pi's bag directly. Typed accessors live on [`SettingsManager`].
//!
//! # Seams
//!
//! * The agent/project settings directories are injected via
//!   [`FileSettingsStorage::new`] (pi threads `cwd` + `agentDir`); the port never
//!   pulls in the unported `config.ts`. [`CONFIG_DIR_NAME`] is duplicated as a
//!   local constant, matching the sibling `trust_manager`/`package_manager` ports.
//! * [`SettingsStorage`] abstracts the file backend; [`InMemorySettingsStorage`]
//!   backs [`SettingsManager::in_memory`] with no filesystem I/O.
//! * Editor resolution's environment/platform reads are isolated in the pure
//!   [`resolve_external_editor`] so the precedence and platform-fallback logic is
//!   unit-testable without mutating process env or `cfg!(windows)`.
//!
//! # Deviations from pi
//!
//! * `reload`/`flush` are synchronous. pi serializes writes through an async
//!   `writeQueue` purely to avoid making callers async around blocking `fs`
//!   calls; the Rust port does the same blocking I/O directly, so the queue
//!   collapses to a straight-line call. Observable behavior (writes land, load
//!   errors are captured for `drain_errors`) is preserved.
//! * `set_enable_analytics` mints a tracking id via agent-core's `uuidv7`
//!   instead of Node's `randomUUID` (v4). No test pins the id's version and the
//!   crate already depends on `atilla_agent`; pulling a fresh uuid dependency for
//!   one call site would be gratuitous.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::http_dispatcher::{parse_http_idle_timeout_ms, parse_http_idle_timeout_num};

use atilla_agent::harness::events::Transport;

/// pi's `CONFIG_DIR_NAME` (`pkg.piConfig?.configDir || ".pi"`), duplicated here
/// because `config.ts` is outside this port's scope (see the sibling
/// `trust_manager`/`package_manager` ports, which do the same).
pub const CONFIG_DIR_NAME: &str = ".pi";

/// Preferred transport setting alias, matching pi's `TransportSetting`.
pub type TransportSetting = Transport;

/// A package source: either a bare `string` or a filtering object. Mirrors pi's
/// `PackageSource` union; kept as a raw [`Value`] so string and object forms
/// round-trip losslessly.
pub type PackageSource = Value;

/// The scope a settings value is loaded from / persisted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsScope {
    Global,
    Project,
}

/// Global-only default project-trust preference. Mirrors pi's
/// `DefaultProjectTrust` union (`"ask" | "always" | "never"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultProjectTrust {
    Ask,
    Always,
    Never,
}

/// A load error recorded against a scope. Mirrors pi's `SettingsError`; the
/// underlying `Error` is flattened to its message string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    pub scope: SettingsScope,
    pub message: String,
}

/// Options for [`SettingsManager::from_storage`] / [`SettingsManager::create`].
#[derive(Debug, Clone, Default)]
pub struct SettingsManagerCreateOptions {
    /// Whether the project scope is trusted. pi defaults this to `true`.
    pub project_trusted: Option<bool>,
}

/// Custom token budgets for thinking levels. Mirrors pi's
/// `ThinkingBudgetsSettings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgetsSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimal: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medium: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high: Option<i64>,
}

/// Warning toggles. Mirrors pi's `WarningSettings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarningSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_extra_usage: Option<bool>,
}

/// Resolved compaction settings (defaults applied). Mirrors pi's
/// `getCompactionSettings` return shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionResolved {
    pub enabled: bool,
    pub reserve_tokens: i64,
    pub keep_recent_tokens: i64,
}

/// Resolved branch-summary settings. Mirrors pi's `getBranchSummarySettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchSummaryResolved {
    pub reserve_tokens: i64,
    pub skip_prompt: bool,
}

/// Resolved retry settings. Mirrors pi's `getRetrySettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryResolved {
    pub enabled: bool,
    pub max_retries: i64,
    pub base_delay_ms: i64,
}

/// Resolved provider-retry settings. Mirrors pi's `getProviderRetrySettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRetryResolved {
    pub timeout_ms: Option<i64>,
    pub max_retries: Option<i64>,
    pub max_retry_delay_ms: i64,
}

// ---------------------------------------------------------------------------
// Settings bag
// ---------------------------------------------------------------------------

/// The settings bag: a JSON object preserving arbitrary/unknown keys. Newtype
/// over `serde_json::Map` so unknown keys survive load → merge → persist exactly
/// as pi's `Record<string, unknown>` does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings(Map<String, Value>);

impl Settings {
    /// An empty settings bag.
    pub fn empty() -> Self {
        Settings(Map::new())
    }

    /// Wrap an existing JSON object.
    pub fn from_map(map: Map<String, Value>) -> Self {
        Settings(map)
    }

    /// Borrow the underlying JSON object.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// Convert into the underlying JSON object.
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    fn get_str(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_string)
    }

    fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    /// Read `key` as an array of strings (non-string elements dropped, matching
    /// how pi's typed getters treat these fields).
    fn get_str_array(&self, key: &str) -> Option<Vec<String>> {
        self.get(key).and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
    }

    /// Read a nested object's field as a bool.
    fn get_nested_bool(&self, key: &str, nested: &str) -> Option<bool> {
        self.get(key)
            .and_then(Value::as_object)
            .and_then(|o| o.get(nested))
            .and_then(Value::as_bool)
    }

    /// Read a nested object's field as an f64.
    fn get_nested_f64(&self, key: &str, nested: &str) -> Option<f64> {
        self.get(key)
            .and_then(Value::as_object)
            .and_then(|o| o.get(nested))
            .and_then(Value::as_f64)
    }

    fn set(&mut self, key: &str, value: Value) {
        self.0.insert(key.to_string(), value);
    }

    /// Set `key` to `value`, or remove it when `value` is `None` (pi assigns
    /// `undefined`, which `JSON.stringify` drops).
    fn set_opt(&mut self, key: &str, value: Option<Value>) {
        match value {
            Some(v) => {
                self.0.insert(key.to_string(), v);
            }
            None => {
                self.0.remove(key);
            }
        }
    }

    /// Ensure `key` holds an object and return a mutable handle to it.
    fn nested_mut(&mut self, key: &str) -> &mut Map<String, Value> {
        let entry = self
            .0
            .entry(key.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        entry.as_object_mut().expect("just ensured object")
    }
}

// ---------------------------------------------------------------------------
// Pure merge / migration helpers
// ---------------------------------------------------------------------------

/// Deep merge: `overrides` wins; nested objects shallow-merge one level.
/// Faithful to pi's `deepMergeSettings` (whose "recursive" comment is aspirational
/// — the implementation is a one-level spread).
pub fn deep_merge_settings(base: &Settings, overrides: &Settings) -> Settings {
    let mut result = base.0.clone();

    for (key, override_value) in &overrides.0 {
        let base_value = base.0.get(key);
        // For two plain objects, shallow-merge; otherwise the override wins.
        if override_value.is_object() && base_value.map(Value::is_object).unwrap_or(false) {
            let mut merged = base_value
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (k, v) in override_value.as_object().expect("checked is_object") {
                merged.insert(k.clone(), v.clone());
            }
            result.insert(key.clone(), Value::Object(merged));
        } else {
            result.insert(key.clone(), override_value.clone());
        }
    }

    Settings(result)
}

/// Migrate legacy settings shapes in place. Faithful to pi's `migrateSettings`.
pub fn migrate_settings(mut map: Map<String, Value>) -> Settings {
    // queueMode -> steeringMode
    if map.contains_key("queueMode") && !map.contains_key("steeringMode") {
        if let Some(v) = map.remove("queueMode") {
            map.insert("steeringMode".to_string(), v);
        }
    }

    // legacy websockets boolean -> transport enum
    if !map.contains_key("transport") {
        if let Some(Value::Bool(ws)) = map.get("websockets").cloned() {
            let transport = if ws { "websocket" } else { "sse" };
            map.insert(
                "transport".to_string(),
                Value::String(transport.to_string()),
            );
            map.remove("websockets");
        }
    }

    // skills object -> array
    if let Some(Value::Object(skills_obj)) = map.get("skills").cloned() {
        if skills_obj.contains_key("enableSkillCommands")
            && !map.contains_key("enableSkillCommands")
        {
            if let Some(v) = skills_obj.get("enableSkillCommands") {
                map.insert("enableSkillCommands".to_string(), v.clone());
            }
        }
        match skills_obj.get("customDirectories") {
            Some(Value::Array(dirs)) if !dirs.is_empty() => {
                map.insert("skills".to_string(), Value::Array(dirs.clone()));
            }
            _ => {
                map.remove("skills");
            }
        }
    }

    // retry.maxDelayMs -> retry.provider.maxRetryDelayMs
    if let Some(Value::Object(mut retry)) = map.get("retry").cloned() {
        let max_delay = retry.get("maxDelayMs").cloned();
        let is_number = max_delay.as_ref().map(Value::is_number).unwrap_or(false);
        if is_number {
            let provider = retry.get("provider").and_then(Value::as_object).cloned();
            let provider_missing_delay = provider
                .as_ref()
                .and_then(|p| p.get("maxRetryDelayMs"))
                .map(Value::is_null)
                .unwrap_or(true);
            if provider_missing_delay {
                let mut new_provider = provider.unwrap_or_default();
                new_provider.insert(
                    "maxRetryDelayMs".to_string(),
                    max_delay.clone().expect("checked is_number"),
                );
                retry.insert("provider".to_string(), Value::Object(new_provider));
            }
        }
        retry.remove("maxDelayMs");
        map.insert("retry".to_string(), Value::Object(retry));
    }

    Settings(map)
}

/// Parse a timeout setting value. `Ok(None)` when absent; `Err` when present but
/// invalid. Mirrors pi's `parseTimeoutSetting`.
fn parse_timeout_setting(value: Option<&Value>, setting_name: &str) -> Result<Option<u64>, String> {
    let value = match value {
        None => return Ok(None),
        Some(v) => v,
    };
    let parsed = if let Some(n) = value.as_f64() {
        parse_http_idle_timeout_num(n)
    } else if let Some(s) = value.as_str() {
        parse_http_idle_timeout_ms(s)
    } else {
        None
    };
    match parsed {
        Some(ms) => Ok(Some(ms)),
        None => Err(format!("Invalid {setting_name} setting: {value}")),
    }
}

/// Resolve the external editor command. Pure so the precedence and
/// platform-fallback logic is testable without touching process env or `cfg`.
fn resolve_external_editor(
    configured: Option<&str>,
    visual: Option<&str>,
    editor: Option<&str>,
    is_windows: bool,
) -> String {
    if let Some(c) = configured {
        if !c.trim().is_empty() {
            return c.to_string();
        }
    }
    for env in [visual, editor].into_iter().flatten() {
        if !env.is_empty() {
            return env.to_string();
        }
    }
    if is_windows {
        "notepad".to_string()
    } else {
        "nano".to_string()
    }
}

mod storage;
pub use storage::{FileSettingsStorage, InMemorySettingsStorage, SettingsStorage};

mod manager;
pub use manager::SettingsManager;

#[cfg(test)]
mod tests;
