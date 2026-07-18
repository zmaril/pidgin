//! Pure display helpers shared by the tool renderers.
//!
//! Ported from pi's `core/tools/render-utils.ts`. Only the pure string helpers
//! are ported here: [`str_value`], [`replace_tabs`], [`normalize_display_text`],
//! and [`shorten_path`]. The theme/terminal-dependent renderers (`linkPath`,
//! `getTextOutput`, `renderToolPath`, `invalidArgText`) are deferred: they pull
//! in the pi-tui capabilities, ANSI handling, and the interactive `Theme`, none
//! of which exist in this crate yet.

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
