//! Bit-exact port of pi's `keybindings.ts`
//! (`vendor/pi/packages/tui/src/keybindings.ts`).
//!
//! The keybinding registry ([`TUI_KEYBINDINGS`]) and [`KeybindingsManager`]
//! resolution: default-vs-user binding merge, conflict detection over user
//! claims, and `matches` via [`crate::keys::matches_key`]. Insertion order is
//! preserved everywhere pi relies on `Object`/`Map` order (conflicts, resolved
//! bindings), so structural results are identical.

use crate::keys::matches_key;

/// A keybinding definition: its default key(s) and an optional description.
#[derive(Debug, Clone)]
pub struct KeybindingDefinition {
    /// Default key(s) bound to this action (already in declared order).
    pub default_keys: Vec<String>,
    /// Human-readable description.
    pub description: Option<String>,
}

/// The default TUI keybinding table, in declaration order (matches pi's
/// `TUI_KEYBINDINGS`).
pub fn tui_keybindings() -> Vec<(&'static str, KeybindingDefinition)> {
    fn def(keys: &[&str], description: &str) -> KeybindingDefinition {
        KeybindingDefinition {
            default_keys: keys.iter().map(|s| s.to_string()).collect(),
            description: Some(description.to_string()),
        }
    }
    vec![
        ("tui.editor.cursorUp", def(&["up"], "Move cursor up")),
        ("tui.editor.cursorDown", def(&["down"], "Move cursor down")),
        (
            "tui.editor.cursorLeft",
            def(&["left", "ctrl+b"], "Move cursor left"),
        ),
        (
            "tui.editor.cursorRight",
            def(&["right", "ctrl+f"], "Move cursor right"),
        ),
        (
            "tui.editor.cursorWordLeft",
            def(&["alt+left", "ctrl+left", "alt+b"], "Move cursor word left"),
        ),
        (
            "tui.editor.cursorWordRight",
            def(
                &["alt+right", "ctrl+right", "alt+f"],
                "Move cursor word right",
            ),
        ),
        (
            "tui.editor.cursorLineStart",
            def(&["home", "ctrl+a"], "Move to line start"),
        ),
        (
            "tui.editor.cursorLineEnd",
            def(&["end", "ctrl+e"], "Move to line end"),
        ),
        (
            "tui.editor.jumpForward",
            def(&["ctrl+]"], "Jump forward to character"),
        ),
        (
            "tui.editor.jumpBackward",
            def(&["ctrl+alt+]"], "Jump backward to character"),
        ),
        ("tui.editor.pageUp", def(&["pageUp"], "Page up")),
        ("tui.editor.pageDown", def(&["pageDown"], "Page down")),
        (
            "tui.editor.deleteCharBackward",
            def(&["backspace"], "Delete character backward"),
        ),
        (
            "tui.editor.deleteCharForward",
            def(&["delete", "ctrl+d"], "Delete character forward"),
        ),
        (
            "tui.editor.deleteWordBackward",
            def(&["ctrl+w", "alt+backspace"], "Delete word backward"),
        ),
        (
            "tui.editor.deleteWordForward",
            def(&["alt+d", "alt+delete"], "Delete word forward"),
        ),
        (
            "tui.editor.deleteToLineStart",
            def(&["ctrl+u"], "Delete to line start"),
        ),
        (
            "tui.editor.deleteToLineEnd",
            def(&["ctrl+k"], "Delete to line end"),
        ),
        ("tui.editor.yank", def(&["ctrl+y"], "Yank")),
        ("tui.editor.yankPop", def(&["alt+y"], "Yank pop")),
        ("tui.editor.undo", def(&["ctrl+-"], "Undo")),
        (
            "tui.input.newLine",
            def(&["shift+enter", "ctrl+j"], "Insert newline"),
        ),
        ("tui.input.submit", def(&["enter"], "Submit input")),
        ("tui.input.tab", def(&["tab"], "Tab / autocomplete")),
        ("tui.input.copy", def(&["ctrl+c"], "Copy selection")),
        ("tui.select.up", def(&["up"], "Move selection up")),
        ("tui.select.down", def(&["down"], "Move selection down")),
        ("tui.select.pageUp", def(&["pageUp"], "Selection page up")),
        (
            "tui.select.pageDown",
            def(&["pageDown"], "Selection page down"),
        ),
        ("tui.select.confirm", def(&["enter"], "Confirm selection")),
        (
            "tui.select.cancel",
            def(&["escape", "ctrl+c"], "Cancel selection"),
        ),
    ]
}

/// A detected conflict: a key claimed by more than one user binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    /// The conflicting key.
    pub key: String,
    /// The keybinding ids claiming it, in insertion order.
    pub keybindings: Vec<String>,
}

// Dedup keys preserving order (pi's `normalizeKeys`).
fn normalize_keys(keys: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut result: Vec<String> = Vec::new();
    for key in keys {
        if !seen.iter().any(|k| k == key) {
            seen.push(key.clone());
            result.push(key.clone());
        }
    }
    result
}

/// Manages keybinding resolution over default definitions and user overrides.
///
/// Definitions and user bindings are stored as ordered `(id, ...)` lists to
/// mirror JavaScript object/map iteration order, which pi's conflict and
/// resolved-binding outputs depend on.
#[derive(Clone)]
pub struct KeybindingsManager {
    definitions: Vec<(String, KeybindingDefinition)>,
    // `None` value = key present but explicitly `undefined` (falls back to default).
    user_bindings: Vec<(String, Option<Vec<String>>)>,
    keys_by_id: Vec<(String, Vec<String>)>,
    conflicts: Vec<KeybindingConflict>,
}

impl KeybindingsManager {
    /// Create a manager from definitions and (optional) user bindings.
    pub fn new(
        definitions: Vec<(&str, KeybindingDefinition)>,
        user_bindings: Vec<(&str, Option<Vec<String>>)>,
    ) -> Self {
        let definitions = definitions
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let user_bindings = user_bindings
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let mut mgr = Self {
            definitions,
            user_bindings,
            keys_by_id: Vec::new(),
            conflicts: Vec::new(),
        };
        mgr.rebuild();
        mgr
    }

    fn definitions_contains(&self, id: &str) -> bool {
        self.definitions.iter().any(|(k, _)| k == id)
    }

    fn user_binding(&self, id: &str) -> Option<&Option<Vec<String>>> {
        self.user_bindings
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, v)| v)
    }

    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        // userClaims: ordered map KeyId -> ordered set of keybinding ids.
        let mut user_claims: Vec<(String, Vec<String>)> = Vec::new();
        for (keybinding, keys) in &self.user_bindings {
            if !self.definitions_contains(keybinding) {
                continue;
            }
            let normalized = match keys {
                Some(k) => normalize_keys(k),
                None => Vec::new(),
            };
            for key in normalized {
                if let Some((_, claimants)) = user_claims.iter_mut().find(|(k, _)| *k == key) {
                    if !claimants.iter().any(|c| c == keybinding) {
                        claimants.push(keybinding.clone());
                    }
                } else {
                    user_claims.push((key, vec![keybinding.clone()]));
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

        for (id, definition) in &self.definitions {
            let keys = match self.user_binding(id) {
                None => normalize_keys(&definition.default_keys),
                Some(None) => normalize_keys(&definition.default_keys),
                Some(Some(user_keys)) => normalize_keys(user_keys),
            };
            self.keys_by_id.push((id.clone(), keys));
        }
    }

    fn keys_for(&self, keybinding: &str) -> &[String] {
        self.keys_by_id
            .iter()
            .find(|(k, _)| k == keybinding)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    /// `true` if `data` matches any key bound to `keybinding`.
    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        self.keys_for(keybinding)
            .iter()
            .any(|key| matches_key(data, key))
    }

    /// The keys bound to `keybinding` (empty if unknown).
    pub fn get_keys(&self, keybinding: &str) -> Vec<String> {
        self.keys_for(keybinding).to_vec()
    }

    /// The definition for `keybinding`, if any.
    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions
            .iter()
            .find(|(k, _)| k == keybinding)
            .map(|(_, v)| v)
    }

    /// The detected conflicts, in insertion order.
    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    /// Replace user bindings and rebuild.
    pub fn set_user_bindings(&mut self, user_bindings: Vec<(&str, Option<Vec<String>>)>) {
        self.user_bindings = user_bindings
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        self.rebuild();
    }

    /// Resolved bindings: each id mapped to a single key or a list, in
    /// definition order (matches pi's `getResolvedBindings`).
    pub fn get_resolved_bindings(&self) -> Vec<(String, Vec<String>)> {
        self.definitions
            .iter()
            .map(|(id, _)| (id.clone(), self.keys_for(id).to_vec()))
            .collect()
    }
}
