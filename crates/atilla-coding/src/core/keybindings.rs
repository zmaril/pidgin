//! Keybinding configuration, defaults, and legacy-name migration.
//!
//! Ported from pi-coding-agent's `core/keybindings.ts`. It owns three things:
//!
//! - the **default keybinding table** (`KEYBINDINGS`), which is pi-tui's base
//!   `TUI_KEYBINDINGS` followed by the coding-agent's own `app.*` actions, some
//!   of them platform-dependent;
//! - **migration** of legacy flat key names (`cursorUp`, `expandTools`, ...) to
//!   the namespaced ids (`tui.editor.cursorUp`, `app.tools.expand`, ...), with
//!   the exact precedence pi uses when both an old and new name are present;
//! - a [`KeybindingsManager`] that loads a user config file, applies the
//!   migration in memory, and resolves user overrides against the defaults.
//!
//! # Seams
//!
//! pi's `KeybindingsManager` `extends` pi-tui's `KeybindingsManager`; pi-tui is
//! not ported yet, so the base resolution logic (`rebuild`/`getResolvedBindings`
//! and the `TUI_KEYBINDINGS` defaults) is inlined here. `matches()` is
//! intentionally omitted: matching a real terminal key event requires pi-tui's
//! `matchesKey` key parser, which lands with the tui port.
//!
//! pi reads the target platform from `process.platform` at runtime; this port
//! reads `std::env::consts::OS` (also a runtime value) and exposes [`Platform`]
//! so the platform-dependent defaults stay testable.

// straitjacket-allow-file:duplication — faithful parallel-structure mirror of
// pi-tui's rebuild()/conflict-detection, inlined here while the tui base was
// unported; the pi-tui port now lives at crates/atilla-tui/src/keybindings.rs.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single key identifier such as `"ctrl+p"` or `"shift+enter"`.
///
/// Mirrors pi-tui's `KeyId`. This port treats it as an opaque string; parsing
/// and event matching belong to the (not-yet-ported) tui layer.
pub type KeyId = String;

/// One or more keys bound to an action.
///
/// Mirrors the `KeyId | KeyId[]` shape of pi's config values: a bare string
/// serializes as a JSON string, a list as a JSON array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Keys {
    /// Exactly one key.
    One(KeyId),
    /// Zero or more keys (an empty list means "unbound").
    Many(Vec<KeyId>),
}

/// A default definition for one keybinding: its keys plus a human description.
///
/// Mirrors pi-tui's `KeybindingDefinition`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingDefinition {
    /// The keys bound by default.
    pub default_keys: Keys,
    /// A short human-readable description.
    pub description: &'static str,
}

/// User-facing keybinding config: action id -> keys. Order is preserved.
///
/// Mirrors pi-tui's `KeybindingsConfig` (`Record<string, KeyId | KeyId[]>`).
pub type KeybindingsConfig = IndexMap<String, Keys>;

/// The ordered table of default definitions, keyed by action id.
type Definitions = IndexMap<String, KeybindingDefinition>;

/// Target platform, mirroring the branches pi takes on `process.platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Windows (`process.platform === "win32"`).
    Windows,
    /// macOS (`process.platform === "darwin"`).
    Macos,
    /// Any other platform (Linux, BSD, ...).
    Other,
}

impl Platform {
    /// The platform this binary is running on.
    pub fn current() -> Self {
        match std::env::consts::OS {
            "windows" => Platform::Windows,
            "macos" => Platform::Macos,
            _ => Platform::Other,
        }
    }
}

fn one(key: &str) -> Keys {
    Keys::One(key.to_string())
}

fn many(keys: &[&str]) -> Keys {
    Keys::Many(keys.iter().map(|k| (*k).to_string()).collect())
}

/// An empty (unbound) default, matching pi's `defaultKeys: []`.
fn unbound() -> Keys {
    Keys::Many(Vec::new())
}

/// Build the ordered default keybinding table for a given platform.
///
/// pi-tui's `TUI_KEYBINDINGS` come first (matching the `...TUI_KEYBINDINGS`
/// spread), then the coding-agent's `app.*` actions, preserving source order so
/// [`order_keybindings_config`] stays faithful.
pub fn keybindings_for(platform: Platform) -> Definitions {
    let win = platform == Platform::Windows;
    let mac = platform == Platform::Macos;

    // A flat table keeps each binding auditable against the two source files
    // (pi-tui `keybindings.ts` and coding-agent `keybindings.ts`) line for line.
    let entries: Vec<(&str, Keys, &str)> = vec![
        // --- pi-tui TUI_KEYBINDINGS ---
        ("tui.editor.cursorUp", one("up"), "Move cursor up"),
        ("tui.editor.cursorDown", one("down"), "Move cursor down"),
        (
            "tui.editor.cursorLeft",
            many(&["left", "ctrl+b"]),
            "Move cursor left",
        ),
        (
            "tui.editor.cursorRight",
            many(&["right", "ctrl+f"]),
            "Move cursor right",
        ),
        (
            "tui.editor.cursorWordLeft",
            many(&["alt+left", "ctrl+left", "alt+b"]),
            "Move cursor word left",
        ),
        (
            "tui.editor.cursorWordRight",
            many(&["alt+right", "ctrl+right", "alt+f"]),
            "Move cursor word right",
        ),
        (
            "tui.editor.cursorLineStart",
            many(&["home", "ctrl+a"]),
            "Move to line start",
        ),
        (
            "tui.editor.cursorLineEnd",
            many(&["end", "ctrl+e"]),
            "Move to line end",
        ),
        (
            "tui.editor.jumpForward",
            one("ctrl+]"),
            "Jump forward to character",
        ),
        (
            "tui.editor.jumpBackward",
            one("ctrl+alt+]"),
            "Jump backward to character",
        ),
        ("tui.editor.pageUp", one("pageUp"), "Page up"),
        ("tui.editor.pageDown", one("pageDown"), "Page down"),
        (
            "tui.editor.deleteCharBackward",
            one("backspace"),
            "Delete character backward",
        ),
        (
            "tui.editor.deleteCharForward",
            many(&["delete", "ctrl+d"]),
            "Delete character forward",
        ),
        (
            "tui.editor.deleteWordBackward",
            many(&["ctrl+w", "alt+backspace"]),
            "Delete word backward",
        ),
        (
            "tui.editor.deleteWordForward",
            many(&["alt+d", "alt+delete"]),
            "Delete word forward",
        ),
        (
            "tui.editor.deleteToLineStart",
            one("ctrl+u"),
            "Delete to line start",
        ),
        (
            "tui.editor.deleteToLineEnd",
            one("ctrl+k"),
            "Delete to line end",
        ),
        ("tui.editor.yank", one("ctrl+y"), "Yank"),
        ("tui.editor.yankPop", one("alt+y"), "Yank pop"),
        ("tui.editor.undo", one("ctrl+-"), "Undo"),
        (
            "tui.input.newLine",
            many(&["shift+enter", "ctrl+j"]),
            "Insert newline",
        ),
        ("tui.input.submit", one("enter"), "Submit input"),
        ("tui.input.tab", one("tab"), "Tab / autocomplete"),
        ("tui.input.copy", one("ctrl+c"), "Copy selection"),
        ("tui.select.up", one("up"), "Move selection up"),
        ("tui.select.down", one("down"), "Move selection down"),
        ("tui.select.pageUp", one("pageUp"), "Selection page up"),
        (
            "tui.select.pageDown",
            one("pageDown"),
            "Selection page down",
        ),
        ("tui.select.confirm", one("enter"), "Confirm selection"),
        (
            "tui.select.cancel",
            many(&["escape", "ctrl+c"]),
            "Cancel selection",
        ),
        // --- coding-agent app.* keybindings ---
        ("app.interrupt", one("escape"), "Cancel or abort"),
        ("app.clear", one("ctrl+c"), "Clear editor"),
        ("app.exit", one("ctrl+d"), "Exit when editor is empty"),
        (
            "app.suspend",
            if win { unbound() } else { one("ctrl+z") },
            "Suspend to background",
        ),
        (
            "app.thinking.cycle",
            one("shift+tab"),
            "Cycle thinking level",
        ),
        (
            "app.model.cycleForward",
            one("ctrl+p"),
            "Cycle to next model",
        ),
        (
            "app.model.cycleBackward",
            one("shift+ctrl+p"),
            "Cycle to previous model",
        ),
        ("app.model.select", one("ctrl+l"), "Open model selector"),
        ("app.tools.expand", one("ctrl+o"), "Toggle tool output"),
        (
            "app.thinking.toggle",
            one("ctrl+t"),
            "Toggle thinking blocks",
        ),
        (
            "app.session.toggleNamedFilter",
            one("ctrl+n"),
            "Toggle named session filter",
        ),
        ("app.editor.external", one("ctrl+g"), "Open external editor"),
        (
            "app.message.copy",
            one("ctrl+x"),
            "Copy message to clipboard",
        ),
        (
            "app.message.followUp",
            one("alt+enter"),
            "Queue follow-up message",
        ),
        (
            "app.message.dequeue",
            one("alt+up"),
            "Restore queued messages",
        ),
        (
            "app.clipboard.pasteImage",
            if win { one("alt+v") } else { one("ctrl+v") },
            "Paste image from clipboard (text fallback)",
        ),
        ("app.session.new", unbound(), "Start a new session"),
        ("app.session.tree", unbound(), "Open session tree"),
        ("app.session.fork", unbound(), "Fork current session"),
        ("app.session.resume", unbound(), "Resume a session"),
        (
            "app.tree.foldOrUp",
            if mac {
                many(&["alt+left", "ctrl+left"])
            } else {
                many(&["ctrl+left", "alt+left"])
            },
            "Fold tree branch or move up",
        ),
        (
            "app.tree.unfoldOrDown",
            if mac {
                many(&["alt+right", "ctrl+right"])
            } else {
                many(&["ctrl+right", "alt+right"])
            },
            "Unfold tree branch or move down",
        ),
        ("app.tree.editLabel", one("shift+l"), "Edit tree label"),
        (
            "app.tree.toggleLabelTimestamp",
            one("shift+t"),
            "Toggle tree label timestamps",
        ),
        (
            "app.session.togglePath",
            one("ctrl+p"),
            "Toggle session path display",
        ),
        (
            "app.session.toggleSort",
            one("ctrl+s"),
            "Toggle session sort mode",
        ),
        ("app.session.rename", one("ctrl+r"), "Rename session"),
        ("app.session.delete", one("ctrl+d"), "Delete session"),
        (
            "app.session.deleteNoninvasive",
            one("ctrl+backspace"),
            "Delete session when query is empty",
        ),
        ("app.models.save", one("ctrl+s"), "Save model selection"),
        ("app.models.enableAll", one("ctrl+a"), "Enable all models"),
        ("app.models.clearAll", one("ctrl+x"), "Clear all models"),
        (
            "app.models.toggleProvider",
            one("ctrl+p"),
            "Toggle all models for provider",
        ),
        (
            "app.models.reorderUp",
            one("alt+up"),
            "Move model up in order",
        ),
        (
            "app.models.reorderDown",
            one("alt+down"),
            "Move model down in order",
        ),
        (
            "app.tree.filter.default",
            one("ctrl+d"),
            "Tree filter: default view",
        ),
        (
            "app.tree.filter.noTools",
            one("ctrl+t"),
            "Tree filter: hide tool results",
        ),
        (
            "app.tree.filter.userOnly",
            one("ctrl+u"),
            "Tree filter: user messages only",
        ),
        (
            "app.tree.filter.labeledOnly",
            one("ctrl+l"),
            "Tree filter: labeled entries only",
        ),
        (
            "app.tree.filter.all",
            one("ctrl+a"),
            "Tree filter: show all entries",
        ),
        (
            "app.tree.filter.cycleForward",
            one("ctrl+o"),
            "Tree filter: cycle forward",
        ),
        (
            "app.tree.filter.cycleBackward",
            one("shift+ctrl+o"),
            "Tree filter: cycle backward",
        ),
    ];

    entries
        .into_iter()
        .map(|(id, default_keys, description)| {
            (
                id.to_string(),
                KeybindingDefinition {
                    default_keys,
                    description,
                },
            )
        })
        .collect()
}

/// The default keybinding table for the current platform.
pub fn keybindings() -> Definitions {
    keybindings_for(Platform::current())
}

/// Map a legacy flat keybinding name to its namespaced id, if it is legacy.
///
/// Mirrors `KEYBINDING_NAME_MIGRATIONS` plus `isLegacyKeybindingName`.
fn migrate_key_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "cursorUp" => "tui.editor.cursorUp",
        "cursorDown" => "tui.editor.cursorDown",
        "cursorLeft" => "tui.editor.cursorLeft",
        "cursorRight" => "tui.editor.cursorRight",
        "cursorWordLeft" => "tui.editor.cursorWordLeft",
        "cursorWordRight" => "tui.editor.cursorWordRight",
        "cursorLineStart" => "tui.editor.cursorLineStart",
        "cursorLineEnd" => "tui.editor.cursorLineEnd",
        "jumpForward" => "tui.editor.jumpForward",
        "jumpBackward" => "tui.editor.jumpBackward",
        "pageUp" => "tui.editor.pageUp",
        "pageDown" => "tui.editor.pageDown",
        "deleteCharBackward" => "tui.editor.deleteCharBackward",
        "deleteCharForward" => "tui.editor.deleteCharForward",
        "deleteWordBackward" => "tui.editor.deleteWordBackward",
        "deleteWordForward" => "tui.editor.deleteWordForward",
        "deleteToLineStart" => "tui.editor.deleteToLineStart",
        "deleteToLineEnd" => "tui.editor.deleteToLineEnd",
        "yank" => "tui.editor.yank",
        "yankPop" => "tui.editor.yankPop",
        "undo" => "tui.editor.undo",
        "newLine" => "tui.input.newLine",
        "submit" => "tui.input.submit",
        "tab" => "tui.input.tab",
        "copy" => "tui.input.copy",
        "selectUp" => "tui.select.up",
        "selectDown" => "tui.select.down",
        "selectPageUp" => "tui.select.pageUp",
        "selectPageDown" => "tui.select.pageDown",
        "selectConfirm" => "tui.select.confirm",
        "selectCancel" => "tui.select.cancel",
        "interrupt" => "app.interrupt",
        "clear" => "app.clear",
        "exit" => "app.exit",
        "suspend" => "app.suspend",
        "cycleThinkingLevel" => "app.thinking.cycle",
        "cycleModelForward" => "app.model.cycleForward",
        "cycleModelBackward" => "app.model.cycleBackward",
        "selectModel" => "app.model.select",
        "expandTools" => "app.tools.expand",
        "toggleThinking" => "app.thinking.toggle",
        "toggleSessionNamedFilter" => "app.session.toggleNamedFilter",
        "externalEditor" => "app.editor.external",
        "followUp" => "app.message.followUp",
        "dequeue" => "app.message.dequeue",
        "pasteImage" => "app.clipboard.pasteImage",
        "newSession" => "app.session.new",
        "tree" => "app.session.tree",
        "fork" => "app.session.fork",
        "resume" => "app.session.resume",
        "treeFoldOrUp" => "app.tree.foldOrUp",
        "treeUnfoldOrDown" => "app.tree.unfoldOrDown",
        "treeEditLabel" => "app.tree.editLabel",
        "treeToggleLabelTimestamp" => "app.tree.toggleLabelTimestamp",
        "toggleSessionPath" => "app.session.togglePath",
        "toggleSessionSort" => "app.session.toggleSort",
        "renameSession" => "app.session.rename",
        "deleteSession" => "app.session.delete",
        "deleteSessionNoninvasive" => "app.session.deleteNoninvasive",
        _ => return None,
    })
}

/// Migrate legacy keybinding names to namespaced ids.
///
/// Mirrors `migrateKeybindingsConfig`. Only keys are rewritten; values pass
/// through untouched (as arbitrary JSON). When a legacy name and its namespaced
/// target both appear, the namespaced entry wins and the legacy one is dropped
/// (still flagging `migrated`). The result is ordered by [`order_keybindings_config`].
///
/// Returns the migrated config and whether any migration occurred.
pub fn migrate_keybindings_config(
    raw: &IndexMap<String, Value>,
) -> (IndexMap<String, Value>, bool) {
    let mut config: IndexMap<String, Value> = IndexMap::new();
    let mut migrated = false;

    for (key, value) in raw {
        let next_key = migrate_key_name(key).unwrap_or(key.as_str());
        if next_key != key {
            migrated = true;
        }
        if key != next_key && raw.contains_key(next_key) {
            migrated = true;
            continue;
        }
        config.insert(next_key.to_string(), value.clone());
    }

    (order_keybindings_config(config), migrated)
}

/// Reorder a config so known ids follow the default table's order, and unknown
/// ("extra") ids are appended in sorted order.
///
/// Mirrors `orderKeybindingsConfig`. The set of default ids is
/// platform-independent, so ordering is stable across platforms.
fn order_keybindings_config(config: IndexMap<String, Value>) -> IndexMap<String, Value> {
    let mut ordered: IndexMap<String, Value> = IndexMap::new();
    for id in keybinding_ids() {
        if let Some(value) = config.get(&id) {
            ordered.insert(id, value.clone());
        }
    }

    let mut extras: Vec<&String> = config
        .keys()
        .filter(|key| !ordered.contains_key(*key))
        .collect();
    extras.sort();
    for key in extras {
        ordered.insert(key.clone(), config[key].clone());
    }

    ordered
}

/// The ordered list of default keybinding ids (platform-independent).
fn keybinding_ids() -> Vec<String> {
    // The id set does not depend on platform; only some default values do.
    keybindings_for(Platform::Other).into_keys().collect()
}

/// Coerce raw JSON values into a typed [`KeybindingsConfig`], keeping only
/// string and string-array values.
///
/// Mirrors `toKeybindingsConfig`: entries whose value is not a string or an
/// all-string array are dropped.
fn to_keybindings_config(value: &IndexMap<String, Value>) -> KeybindingsConfig {
    let mut config = KeybindingsConfig::new();
    for (key, binding) in value {
        match binding {
            Value::String(text) => {
                config.insert(key.clone(), Keys::One(text.clone()));
            }
            Value::Array(items) if items.iter().all(Value::is_string) => {
                let keys = items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect();
                config.insert(key.clone(), Keys::Many(keys));
            }
            _ => {}
        }
    }
    config
}

/// Read and parse a config file into a raw JSON object.
///
/// Mirrors `loadRawConfig`: returns `None` when the file is missing, unreadable,
/// malformed, or not a JSON object.
///
/// NOTE: pi's `loadRawConfig` returns JSON arrays too (they are `typeof
/// "object"`); this port treats a top-level array as "no config", matching the
/// stricter check pi's file-rewrite migration already applies.
fn load_raw_config(path: &Path) -> Option<IndexMap<String, Value>> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    match value {
        Value::Object(map) => Some(map.into_iter().collect()),
        _ => None,
    }
}

/// Deduplicate keys while preserving order. Mirrors pi-tui's `normalizeKeys`.
fn normalize_keys(keys: &Keys) -> Vec<KeyId> {
    let list: &[KeyId] = match keys {
        Keys::One(key) => std::slice::from_ref(key),
        Keys::Many(items) => items,
    };
    let mut seen: HashSet<&KeyId> = HashSet::new();
    let mut result = Vec::new();
    for key in list {
        if seen.insert(key) {
            result.push(key.clone());
        }
    }
    result
}

/// A key claimed by more than one binding. Mirrors pi-tui's `KeybindingConflict`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    /// The contested key.
    pub key: KeyId,
    /// The ids of the bindings that claim it.
    pub keybindings: Vec<String>,
}

/// Resolves user keybinding overrides against the default table.
///
/// Mirrors pi-coding-agent's `KeybindingsManager`, which `extends` pi-tui's base
/// manager. The base resolution logic is inlined here (see the module seam
/// note). It loads a config file, migrates legacy names in memory, and computes
/// the effective keys for every action.
pub struct KeybindingsManager {
    definitions: Definitions,
    user_bindings: KeybindingsConfig,
    keys_by_id: IndexMap<String, Vec<KeyId>>,
    conflicts: Vec<KeybindingConflict>,
    config_path: Option<PathBuf>,
}

impl KeybindingsManager {
    /// Create a manager from explicit user bindings and an optional config path.
    ///
    /// Mirrors the pi constructor `new KeybindingsManager(userBindings, configPath)`.
    pub fn new(user_bindings: KeybindingsConfig, config_path: Option<PathBuf>) -> Self {
        let mut manager = Self {
            definitions: keybindings(),
            user_bindings,
            keys_by_id: IndexMap::new(),
            conflicts: Vec::new(),
            config_path,
        };
        manager.rebuild();
        manager
    }

    /// Load `<agent_dir>/keybindings.json`, migrating legacy names in memory.
    ///
    /// Mirrors `KeybindingsManager.create`. pi defaults `agent_dir` to
    /// `getAgentDir()`; that config seam is not ported, so the caller supplies it.
    pub fn create(agent_dir: &Path) -> Self {
        let config_path = agent_dir.join("keybindings.json");
        let user_bindings = Self::load_from_file(&config_path);
        Self::new(user_bindings, Some(config_path))
    }

    /// Re-read the backing config file, if any. Mirrors `reload`.
    pub fn reload(&mut self) {
        if let Some(path) = self.config_path.clone() {
            let user_bindings = Self::load_from_file(&path);
            self.set_user_bindings(user_bindings);
        }
    }

    /// The effective (resolved) config. Mirrors `getEffectiveConfig`.
    pub fn get_effective_config(&self) -> KeybindingsConfig {
        self.get_resolved_bindings()
    }

    /// The raw user overrides (after migration). Mirrors `getUserBindings`.
    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        self.user_bindings.clone()
    }

    /// Replace the user overrides and recompute. Mirrors `setUserBindings`.
    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    /// The resolved keys for one action. Mirrors `getKeys`.
    pub fn get_keys(&self, keybinding: &str) -> Vec<KeyId> {
        self.keys_by_id.get(keybinding).cloned().unwrap_or_default()
    }

    /// The default definition for one action. Mirrors `getDefinition`.
    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions.get(keybinding)
    }

    /// Keys claimed by more than one binding. Mirrors `getConflicts`.
    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    /// Resolve every action to its effective keys. Mirrors `getResolvedBindings`.
    fn get_resolved_bindings(&self) -> KeybindingsConfig {
        let mut resolved = KeybindingsConfig::new();
        for id in self.definitions.keys() {
            let keys = self.keys_by_id.get(id).cloned().unwrap_or_default();
            let value = if keys.len() == 1 {
                Keys::One(keys[0].clone())
            } else {
                Keys::Many(keys)
            };
            resolved.insert(id.clone(), value);
        }
        resolved
    }

    /// Recompute resolved keys and conflicts. Mirrors pi-tui's private `rebuild`.
    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        // Detect keys claimed by more than one user-overridden binding.
        let mut user_claims: IndexMap<KeyId, Vec<String>> = IndexMap::new();
        for (keybinding, keys) in &self.user_bindings {
            if !self.definitions.contains_key(keybinding) {
                continue;
            }
            for key in normalize_keys(keys) {
                let claimants = user_claims.entry(key).or_default();
                if !claimants.contains(keybinding) {
                    claimants.push(keybinding.clone());
                }
            }
        }
        for (key, keybindings) in &user_claims {
            if keybindings.len() > 1 {
                self.conflicts.push(KeybindingConflict {
                    key: key.clone(),
                    keybindings: keybindings.clone(),
                });
            }
        }

        // A user override replaces the default entirely; otherwise use the default.
        for (id, definition) in &self.definitions {
            let keys = match self.user_bindings.get(id) {
                Some(user_keys) => normalize_keys(user_keys),
                None => normalize_keys(&definition.default_keys),
            };
            self.keys_by_id.insert(id.clone(), keys);
        }
    }

    /// Load, then migrate-in-memory, the config at `path`. Mirrors `loadFromFile`.
    fn load_from_file(path: &Path) -> KeybindingsConfig {
        match load_raw_config(path) {
            None => KeybindingsConfig::new(),
            Some(raw) => to_keybindings_config(&migrate_keybindings_config(&raw).0),
        }
    }
}

/// Rewrite a keybindings config file in place if it contains legacy names.
///
/// NOTE: pi houses this in `migrations.ts` (`migrateKeybindingsConfigFile`), run
/// once on startup. It lives here so the keybindings migration is self-contained
/// and directly testable. Malformed or non-object files are left untouched; a
/// file that needs no migration is not rewritten.
pub fn migrate_keybindings_file(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Ok(()),
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };
    let Value::Object(map) = parsed else {
        return Ok(());
    };

    let raw: IndexMap<String, Value> = map.into_iter().collect();
    let (config, migrated) = migrate_keybindings_config(&raw);
    if !migrated {
        return Ok(());
    }

    let mut serialized =
        serde_json::to_string_pretty(&config).expect("keybindings config is serializable");
    serialized.push('\n');
    fs::write(path, serialized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A throwaway agent directory holding a `keybindings.json`, cleaned on drop.
    struct AgentDir(PathBuf);

    impl AgentDir {
        fn with_config(config: &str) -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("atilla-kb-{}-{unique}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("keybindings.json"), config).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("keybindings.json")
        }

        fn read_config(&self) -> Value {
            serde_json::from_str(&fs::read_to_string(self.config_path()).unwrap()).unwrap()
        }
    }

    impl Drop for AgentDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn config(pairs: &[(&str, Keys)]) -> KeybindingsConfig {
        pairs
            .iter()
            .map(|(id, keys)| ((*id).to_string(), keys.clone()))
            .collect()
    }

    // --- Translated from pi's keybindings-migration.test.ts ---

    #[test]
    fn rewrites_old_key_names_to_namespaced_ids() {
        let agent = AgentDir::with_config(
            &serde_json::to_string_pretty(&json!({
                "cursorUp": ["up", "ctrl+p"],
                "expandTools": "ctrl+x",
            }))
            .unwrap(),
        );

        migrate_keybindings_file(&agent.config_path()).unwrap();

        assert_eq!(
            agent.read_config(),
            json!({
                "tui.editor.cursorUp": ["up", "ctrl+p"],
                "app.tools.expand": "ctrl+x",
            })
        );
    }

    #[test]
    fn keeps_namespaced_value_when_old_and_new_names_both_exist() {
        let agent = AgentDir::with_config(
            &serde_json::to_string_pretty(&json!({
                "expandTools": "ctrl+x",
                "app.tools.expand": "ctrl+y",
            }))
            .unwrap(),
        );

        migrate_keybindings_file(&agent.config_path()).unwrap();

        assert_eq!(agent.read_config(), json!({ "app.tools.expand": "ctrl+y" }));
    }

    #[test]
    fn loads_old_key_names_in_memory_before_the_file_is_rewritten() {
        let agent = AgentDir::with_config(
            &serde_json::to_string_pretty(&json!({
                "selectConfirm": "enter",
                "interrupt": "ctrl+x",
            }))
            .unwrap(),
        );

        let keybindings = KeybindingsManager::create(agent.path());

        assert_eq!(
            keybindings.get_user_bindings(),
            config(&[
                ("tui.select.confirm", Keys::One("enter".to_string())),
                ("app.interrupt", Keys::One("ctrl+x".to_string())),
            ])
        );

        let effective = keybindings.get_effective_config();
        assert_eq!(
            effective.get("tui.select.confirm"),
            Some(&Keys::One("enter".to_string()))
        );
        assert_eq!(
            effective.get("app.interrupt"),
            Some(&Keys::One("ctrl+x".to_string()))
        );

        // The in-memory load must not rewrite the file (pi loads before migrating it).
        assert_eq!(
            agent.read_config(),
            json!({ "selectConfirm": "enter", "interrupt": "ctrl+x" })
        );
    }

    // --- Additional coverage for the ported semantics ---

    #[test]
    fn migration_is_a_noop_for_already_namespaced_config() {
        let raw: IndexMap<String, Value> = [("app.tools.expand".to_string(), json!("ctrl+x"))]
            .into_iter()
            .collect();
        let (migrated_config, migrated) = migrate_keybindings_config(&raw);
        assert!(!migrated);
        assert_eq!(
            migrated_config.get("app.tools.expand"),
            Some(&json!("ctrl+x"))
        );
    }

    #[test]
    fn already_namespaced_file_is_left_untouched() {
        let original = "{\n  \"app.tools.expand\": \"ctrl+x\"\n}\n";
        let agent = AgentDir::with_config(original);
        migrate_keybindings_file(&agent.config_path()).unwrap();
        assert_eq!(fs::read_to_string(agent.config_path()).unwrap(), original);
    }

    #[test]
    fn ordering_puts_known_ids_first_then_sorted_extras() {
        // Two legacy names that migrate, plus two unknown extras.
        let raw: IndexMap<String, Value> = [
            ("expandTools".to_string(), json!("ctrl+x")),
            ("cursorUp".to_string(), json!("up")),
            ("zeta.custom".to_string(), json!("z")),
            ("alpha.custom".to_string(), json!("a")),
        ]
        .into_iter()
        .collect();

        let (ordered, migrated) = migrate_keybindings_config(&raw);
        assert!(migrated);

        let ids: Vec<&str> = ordered.keys().map(String::as_str).collect();
        // Known ids follow the default table order (cursorUp before tools.expand),
        // then extras appear sorted.
        assert_eq!(
            ids,
            vec![
                "tui.editor.cursorUp",
                "app.tools.expand",
                "alpha.custom",
                "zeta.custom"
            ]
        );
    }

    #[test]
    fn non_string_values_are_dropped_when_typing_config() {
        let raw: IndexMap<String, Value> = [
            ("app.tools.expand".to_string(), json!("ctrl+x")),
            ("app.model.select".to_string(), json!(["ctrl+l", "ctrl+m"])),
            ("app.session.new".to_string(), json!(42)),
            ("app.session.tree".to_string(), json!(["ok", 3])),
        ]
        .into_iter()
        .collect();

        let typed = to_keybindings_config(&raw);
        assert_eq!(
            typed.get("app.tools.expand"),
            Some(&Keys::One("ctrl+x".to_string()))
        );
        assert_eq!(
            typed.get("app.model.select"),
            Some(&Keys::Many(vec![
                "ctrl+l".to_string(),
                "ctrl+m".to_string()
            ]))
        );
        assert!(!typed.contains_key("app.session.new"));
        assert!(!typed.contains_key("app.session.tree"));
    }

    #[test]
    fn defaults_resolve_when_no_user_overrides() {
        let manager = KeybindingsManager::new(KeybindingsConfig::new(), None);
        let effective = manager.get_effective_config();
        assert_eq!(
            effective.get("app.interrupt"),
            Some(&Keys::One("escape".to_string()))
        );
        assert_eq!(
            effective.get("app.clear"),
            Some(&Keys::One("ctrl+c".to_string()))
        );
        // An empty default resolves to an empty list, not a single key.
        assert_eq!(effective.get("app.session.new"), Some(&Keys::Many(vec![])));
    }

    #[test]
    fn user_override_replaces_default_and_dedupes() {
        let user = config(&[(
            "app.tools.expand",
            Keys::Many(vec!["ctrl+z".to_string(), "ctrl+z".to_string()]),
        )]);
        let manager = KeybindingsManager::new(user, None);
        assert_eq!(
            manager.get_keys("app.tools.expand"),
            vec!["ctrl+z".to_string()]
        );
    }

    #[test]
    fn conflicting_user_bindings_are_reported() {
        let user = config(&[
            ("app.tools.expand", Keys::One("ctrl+q".to_string())),
            ("app.model.select", Keys::One("ctrl+q".to_string())),
        ]);
        let manager = KeybindingsManager::new(user, None);
        let conflicts = manager.get_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, "ctrl+q");
        assert_eq!(conflicts[0].keybindings.len(), 2);
        assert!(conflicts[0]
            .keybindings
            .contains(&"app.tools.expand".to_string()));
        assert!(conflicts[0]
            .keybindings
            .contains(&"app.model.select".to_string()));
    }

    #[test]
    fn platform_dependent_defaults_differ() {
        let mac = keybindings_for(Platform::Macos);
        let other = keybindings_for(Platform::Other);
        let windows = keybindings_for(Platform::Windows);

        assert_eq!(
            mac.get("app.tree.foldOrUp").unwrap().default_keys,
            Keys::Many(vec!["alt+left".to_string(), "ctrl+left".to_string()])
        );
        assert_eq!(
            other.get("app.tree.foldOrUp").unwrap().default_keys,
            Keys::Many(vec!["ctrl+left".to_string(), "alt+left".to_string()])
        );
        assert_eq!(
            windows.get("app.suspend").unwrap().default_keys,
            Keys::Many(vec![])
        );
        assert_eq!(
            other.get("app.suspend").unwrap().default_keys,
            Keys::One("ctrl+z".to_string())
        );
        assert_eq!(
            windows
                .get("app.clipboard.pasteImage")
                .unwrap()
                .default_keys,
            Keys::One("alt+v".to_string())
        );
        assert_eq!(
            other.get("app.clipboard.pasteImage").unwrap().default_keys,
            Keys::One("ctrl+v".to_string())
        );
    }
}
