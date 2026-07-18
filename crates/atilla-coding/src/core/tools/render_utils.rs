//! Pure display helpers shared by the tool renderers.
//!
//! Ported from pi's `core/tools/render-utils.ts`. The pure string helpers are
//! ported here: [`str_value`], [`replace_tabs`], [`normalize_display_text`],
//! [`shorten_path`], and [`get_text_output`]. The remaining
//! theme/terminal-dependent renderers (`linkPath`, `renderToolPath`,
//! `invalidArgText`) are deferred: they pull in the pi-tui capabilities and the
//! interactive `Theme`, neither of which exists in this crate yet.

use crate::utils::ansi::strip_ansi;
use crate::utils::shell::sanitize_binary_output;

/// Coerce a value into a display string, matching pi's `str`:
/// - a string stays as-is (`Some(string)`)
/// - `null`/`undefined` become the empty string (`Some("")`)
/// - anything else is invalid (`None`)
///
/// Rust models this over `Option<&str>`: `Some(s)` maps to `Some(s)` and `None`
/// (the absent/null case) maps to `Some("")`. A dedicated invalid case is
/// surfaced by [`str_invalid`].
pub fn str_value(value: Option<&str>) -> String {
    value.unwrap_or("").to_string()
}

/// The invalid-argument sentinel from pi's `str` (returns `null`). Callers that
/// need to distinguish "absent" from "wrong type" use this explicit marker.
pub const fn str_invalid() -> Option<String> {
    None
}

/// Replace tab characters with three spaces.
pub fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// Strip carriage returns for display normalization.
pub fn normalize_display_text(text: &str) -> String {
    text.replace('\r', "")
}

/// Materialize a tool result's text for display/model consumption, reproducing
/// pi's `render-utils.ts` `getTextOutput` transform for a single text block:
/// strip ANSI escape sequences, sanitize binary/control output, and drop
/// carriage returns. This is the point at which pi strips ANSI — callers keep
/// the raw text (ANSI intact) until they materialize it here.
pub fn get_text_output(content: &str) -> String {
    normalize_display_text(&sanitize_binary_output(&strip_ansi(content)))
}

/// Shorten a path by replacing a leading home directory with `~`.
pub fn shorten_path(path: &str, home: &str) -> String {
    if !home.is_empty() && path.starts_with(home) {
        return format!("~{}", &path[home.len()..]);
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_value_passes_through_strings() {
        assert_eq!(str_value(Some("hello")), "hello");
        assert_eq!(str_value(Some("")), "");
    }

    #[test]
    fn str_value_maps_absent_to_empty() {
        assert_eq!(str_value(None), "");
    }

    #[test]
    fn replace_tabs_expands_to_three_spaces() {
        assert_eq!(replace_tabs("a\tb"), "a   b");
        assert_eq!(replace_tabs("\t\t"), "      ");
        assert_eq!(replace_tabs("no tabs"), "no tabs");
    }

    #[test]
    fn normalize_display_text_strips_cr() {
        assert_eq!(normalize_display_text("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize_display_text("plain"), "plain");
    }

    #[test]
    fn get_text_output_strips_ansi_sanitizes_and_drops_cr() {
        // ANSI escape stripped, control char sanitized, carriage return dropped.
        assert_eq!(get_text_output("\u{1b}[31mred\u{1b}[0m\r\n"), "red\n");
        assert_eq!(get_text_output("plain"), "plain");
    }

    #[test]
    fn shorten_path_replaces_home() {
        assert_eq!(shorten_path("/home/zack/x/y", "/home/zack"), "~/x/y");
        assert_eq!(shorten_path("/home/zack", "/home/zack"), "~");
    }

    #[test]
    fn shorten_path_leaves_non_home_paths() {
        assert_eq!(shorten_path("/etc/hosts", "/home/zack"), "/etc/hosts");
        assert_eq!(shorten_path("/etc/hosts", ""), "/etc/hosts");
    }
}
