//! Pure display helpers shared by the tool renderers.
//!
//! Ported from pi's `core/tools/render-utils.ts`. The pure string helpers are
//! ported here: [`str_value`], [`str_json`], [`replace_tabs`],
//! [`normalize_display_text`], [`shorten_path`], and [`get_text_output`]. The
//! theme/terminal-dependent renderers [`link_path`], [`render_tool_path`], and
//! [`invalid_arg_text`] are also ported now that the interactive `Theme` and the
//! pi-tui capability/hyperlink seams are available in this crate.

use serde_json::Value;

use pidgin_tui::{get_capabilities, hyperlink};

use crate::modes::interactive::theme::runtime::Theme;
use crate::utils::ansi::strip_ansi;
use crate::utils::paths::{resolve_path, PathInputOptions};
use crate::utils::shell::sanitize_binary_output;

/// Local `theme.fg` wrapper that falls back to the unstyled text on an unknown
/// color key, matching the infallible-render convention used elsewhere (pi's
/// `theme.fg` cannot fail; the ported [`Theme::fg`] returns a `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

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

/// The user's home directory, mirroring pi's `os.homedir()` on POSIX (`$HOME`).
fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Coerce a JSON value into a display string, matching pi's `str`
/// (`render-utils.ts`):
/// - a string stays as-is (`Some(string)`)
/// - `null`/absent become the empty string (`Some("")`)
/// - any other JSON type is invalid (`None`)
///
/// This is the [`Value`]-level analog of [`str_value`]; the edit renderers read
/// path fields straight off the args object, where the wrong-type case must be
/// distinguishable from absent.
pub fn str_json(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        None | Some(Value::Null) => Some(String::new()),
        Some(_) => None,
    }
}

/// The invalid-argument marker text (pi's `invalidArgText`).
pub fn invalid_arg_text(theme: &Theme) -> String {
    fg(theme, "error", "[invalid arg]")
}

/// Node's `url.pathToFileURL(path).href` for a POSIX absolute path: percent-
/// encode the characters Node escapes (`%`, control chars, `#`, `?`) and prefix
/// `file://`. Only reachable from [`link_path`] when the terminal advertises
/// hyperlink support; the byte-exact vectors run with hyperlinks disabled, so
/// this branch is not exercised there.
fn path_to_file_url(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            '%' => encoded.push_str("%25"),
            '\n' => encoded.push_str("%0A"),
            '\r' => encoded.push_str("%0D"),
            '\t' => encoded.push_str("%09"),
            '#' => encoded.push_str("%23"),
            '?' => encoded.push_str("%3F"),
            _ => encoded.push(ch),
        }
    }
    format!("file://{encoded}")
}

/// Wrap `styled_text` in an OSC 8 hyperlink to the file at `raw_path`, gated on
/// the terminal's hyperlink capability (pi's `linkPath`). When hyperlinks are
/// unsupported the styled text is returned unchanged — the byte-exact path.
pub fn link_path(styled_text: &str, raw_path: &str, cwd: &str) -> String {
    if !get_capabilities().hyperlinks {
        return styled_text.to_string();
    }
    // pi calls `resolvePath(rawPath, cwd)`, which may throw; the port falls back
    // to the unlinked text on error rather than propagating out of a renderer.
    match resolve_path(raw_path, cwd, &PathInputOptions::default()) {
        Ok(absolute) => hyperlink(styled_text, &path_to_file_url(&absolute)),
        Err(_) => styled_text.to_string(),
    }
}

/// Render a tool's path argument: `[invalid arg]` for a non-string arg, `...`
/// for an empty path with no fallback, otherwise the home-shortened path in the
/// accent color, hyperlinked when supported (pi's `renderToolPath`).
pub fn render_tool_path(
    raw_path: Option<&str>,
    theme: &Theme,
    cwd: &str,
    empty_fallback: Option<&str>,
) -> String {
    let raw_path = match raw_path {
        None => return invalid_arg_text(theme),
        Some(v) => v,
    };
    let value = if raw_path.is_empty() {
        empty_fallback.unwrap_or("")
    } else {
        raw_path
    };
    if value.is_empty() {
        return fg(theme, "toolOutput", "...");
    }
    link_path(
        &fg(theme, "accent", &shorten_path(value, &home())),
        value,
        cwd,
    )
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

    #[test]
    fn str_json_coerces_like_pi_str() {
        use serde_json::json;
        let s = json!("hello");
        let n = json!(null);
        let num = json!(42);
        assert_eq!(str_json(Some(&s)), Some("hello".to_string()));
        assert_eq!(str_json(None), Some(String::new()));
        assert_eq!(str_json(Some(&n)), Some(String::new()));
        assert_eq!(str_json(Some(&num)), None);
    }

    #[test]
    fn path_to_file_url_percent_encodes_node_set() {
        assert_eq!(path_to_file_url("/a/b c"), "file:///a/b c");
        assert_eq!(path_to_file_url("/a/#x?y%z"), "file:///a/%23x%3Fy%25z");
    }

    #[test]
    fn link_path_returns_styled_text_when_hyperlinks_off() {
        // The byte-exact path: capabilities default to hyperlinks disabled in
        // this environment, so the styled text passes through unchanged.
        assert!(!get_capabilities().hyperlinks);
        assert_eq!(link_path("styled", "src/x.rs", "/cwd"), "styled");
    }
}
