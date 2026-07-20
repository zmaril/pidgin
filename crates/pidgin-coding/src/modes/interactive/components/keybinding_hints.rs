//! Byte-exact port of pi's interactive-mode keybinding-hint helpers
//! (`modes/interactive/components/keybinding-hints.ts`): the `keyText` /
//! `keyHint` / `rawKeyHint` formatting utilities used to render `<key> action`
//! footer hints (the llama UI footers, and — on the deferred status-indicator
//! path — retry/compaction hints).
//!
//! ## Divergences from the TS source
//!
//! * **No globals.** pi reads the module-global `theme` and `getKeybindings()`.
//!   The Rust port threads the [`Theme`] and [`KeybindingsManager`] explicitly
//!   (the llama UI holds both), matching the rest of the interactive port.
//! * **Platform check.** pi's `process.platform === "darwin"` (which relabels the
//!   `alt` modifier to `option`) becomes `cfg!(target_os = "macos")`, evaluated at
//!   compile time for the target — the same per-platform behaviour.

use pidgin_tui::keybindings::KeybindingsManager;

use crate::modes::interactive::theme::Theme;

/// `formatKeyPart(part, options)`. On macOS an `alt` modifier is relabelled to
/// `option`; `capitalize` upper-cases the first character (JS
/// `charAt(0).toUpperCase() + slice(1)`, which is a no-op on the empty string).
fn format_key_part(part: &str, capitalize: bool) -> String {
    let display_part = if cfg!(target_os = "macos") && part.to_lowercase() == "alt" {
        "option"
    } else {
        part
    };
    if !capitalize {
        return display_part.to_string();
    }
    let mut chars = display_part.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `formatKeyText(key, options)`. Splits on `/` (alternatives) then `+`
/// (modifiers), formats each part, and rejoins with the same separators.
pub fn format_key_text(key: &str, capitalize: bool) -> String {
    key.split('/')
        .map(|k| {
            k.split('+')
                .map(|part| format_key_part(part, capitalize))
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// `formatKeys(keys, options)`. Empty key lists format to the empty string;
/// otherwise the keys are joined with `/` and passed through [`format_key_text`].
fn format_keys(keys: &[String], capitalize: bool) -> String {
    if keys.is_empty() {
        return String::new();
    }
    format_key_text(&keys.join("/"), capitalize)
}

/// `keyText(keybinding)` — the resolved keys for `keybinding`, formatted without
/// capitalisation.
pub fn key_text(keybindings: &KeybindingsManager, keybinding: &str) -> String {
    format_keys(&keybindings.get_keys(keybinding), false)
}

/// `keyDisplayText(keybinding)` — like [`key_text`] but with each part
/// capitalised.
pub fn key_display_text(keybindings: &KeybindingsManager, keybinding: &str) -> String {
    format_keys(&keybindings.get_keys(keybinding), true)
}

/// `keyHint(keybinding, description)` — a dim key label followed by a muted,
/// space-prefixed description. The `dim`/`muted` theme colours are always baked
/// into the interactive themes, so a lookup miss is a programmer error.
pub fn key_hint(
    theme: &Theme,
    keybindings: &KeybindingsManager,
    keybinding: &str,
    description: &str,
) -> String {
    let key = theme
        .fg("dim", &key_text(keybindings, keybinding))
        .expect("keybinding-hint theme colour is present");
    let desc = theme
        .fg("muted", &format!(" {description}"))
        .expect("keybinding-hint theme colour is present");
    format!("{key}{desc}")
}

/// `rawKeyHint(key, description)` — like [`key_hint`] but for a literal key
/// string (not a keybinding id).
pub fn raw_key_hint(theme: &Theme, key: &str, description: &str) -> String {
    let key = theme
        .fg("dim", &format_key_text(key, false))
        .expect("keybinding-hint theme colour is present");
    let desc = theme
        .fg("muted", &format!(" {description}"))
        .expect("keybinding-hint theme colour is present");
    format!("{key}{desc}")
}
