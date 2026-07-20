//! Pure display helpers shared by the tool renderers.
//!
//! Ported from pi's `core/tools/render-utils.ts`. The pure string helpers are
//! ported here: [`str_value`], [`str_json`], [`replace_tabs`],
//! [`normalize_display_text`], [`shorten_path`], and [`get_text_output`]. The
//! theme/terminal-dependent renderers [`link_path`], [`render_tool_path`], and
//! [`invalid_arg_text`] are also ported now that the interactive `Theme` and the
//! pi-tui capability/hyperlink seams are available in this crate.

// straitjacket-allow-file:duplication — [`get_text_output_from_blocks`] faithfully
// mirrors pi's `getTextOutput` (the same transform the `ToolExecution` shell's
// `text_output` performs); the shared render helpers here parallel pi's
// `render-utils.ts` by design.

use serde_json::Value;

use pidgin_ai::ContentBlock;
use pidgin_tui::keybindings::{
    KeybindingDefinition as TuiKeybindingDefinition, KeybindingsManager as TuiKeybindingsManager,
};
use pidgin_tui::{get_capabilities, hyperlink};

use crate::core::keybindings::{keybindings_for, Keys, Platform};
use crate::modes::interactive::components::{key_hint, key_text};
use crate::modes::interactive::theme::runtime::Theme;
use crate::utils::ansi::strip_ansi;
use crate::utils::paths::{resolve_path, PathInputOptions};
use crate::utils::shell::sanitize_binary_output;
use crate::utils::syntax_highlight::supports_language;

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

/// [`shorten_path`] against the process home directory, mirroring pi's
/// single-argument `shortenPath(path)` (which reads `os.homedir()` internally).
/// Used by the grep/find call renderers.
pub fn shorten_path_home(path: &str) -> String {
    shorten_path(path, &home())
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

/// Materialize a tool result's text blocks for display, mirroring pi's
/// `getTextOutput(result, showImages)` (`render-utils.ts`) for the text path:
/// each text block is ANSI-stripped, binary-sanitized, and CR-normalized (the
/// per-block [`get_text_output`]), then the blocks are joined with `\n`.
///
/// Image blocks are a deferred seam: pi appends `imageFallback` indicators when
/// image blocks are present and the terminal cannot show them, but the
/// image-fallback / dimension helpers are not ported and the byte-exact vectors
/// carry only text blocks. `show_images` is accepted to mirror pi's signature.
pub fn get_text_output_from_blocks(content: &[ContentBlock], _show_images: bool) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(get_text_output(text)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Map a file path to a highlight.js language identifier from its extension,
/// mirroring pi's `getLanguageFromPath` (`theme/theme.ts`): the last `.`-segment
/// is lowercased and looked up in pi's extension table. An empty extension (a
/// trailing dot, or an empty path) yields `None`; unmapped extensions yield
/// `None`. The whole-name-as-extension quirk for dotless names (`Makefile` ->
/// `makefile`) is preserved, matching pi's `split(".").pop()`.
pub fn get_language_from_path(file_path: &str) -> Option<&'static str> {
    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    if ext.is_empty() {
        return None;
    }
    let lang = match ext.as_str() {
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" | "bash" | "zsh" => "bash",
        "fish" => "fish",
        "ps1" => "powershell",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sass" => "sass",
        "less" => "less",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "md" | "markdown" => "markdown",
        "dockerfile" => "dockerfile",
        "makefile" => "makefile",
        "cmake" => "cmake",
        "lua" => "lua",
        "perl" => "perl",
        "r" => "r",
        "scala" => "scala",
        "clj" => "clojure",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "hs" => "haskell",
        "ml" => "ocaml",
        "vim" => "vim",
        "graphql" => "graphql",
        "proto" => "protobuf",
        "tf" | "hcl" => "hcl",
        _ => return None,
    };
    Some(lang)
}

/// Highlight `code` into themed terminal lines, mirroring pi's `highlightCode`
/// (`theme/theme.ts`).
///
/// **Documented divergence — valid-language highlighting.** pi validates the
/// language via `supportsLanguage` and, when valid, runs the highlight.js
/// grammar engine. That engine is deno-plane only (see
/// [`crate::utils::syntax_highlight`]), so [`supports_language`] is `false` on
/// this build and the no-valid-language fallback is always taken — pi's own path
/// when the engine cannot validate a language: each line is colored with
/// `theme.fg("mdCodeBlock", …)`. This matches the same divergence already
/// documented for the markdown renderer's `highlight_code`. Byte-exact vectors
/// therefore avoid valid-language content.
pub fn highlight_code(code: &str, lang: Option<&str>, theme: &Theme) -> Vec<String> {
    let valid_lang = lang.filter(|l| supports_language(l));
    // The valid-language branch (real hljs) is deno-plane only; on this build
    // `valid_lang` is always `None`, so the fallback below is pi-faithful.
    let _ = valid_lang;
    code.split('\n')
        .map(|line| fg(theme, "mdCodeBlock", line))
        .collect()
}

/// Read a numeric field off a tool result's `details` JSON as `usize`, for the
/// shared `[Truncated: …]` result footers.
pub fn detail_usize(details: &Value, key: &str) -> Option<usize> {
    details.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

/// Render a JSON numeric argument the way JS string-interpolation does, for the
/// `(limit N)` / `limit N` call suffixes: integers print without a decimal
/// point, other numbers via their default formatting.
pub fn json_number_display(value: &Value) -> String {
    if let Some(i) = value.as_i64() {
        return i.to_string();
    }
    if let Some(u) = value.as_u64() {
        return u.to_string();
    }
    if let Some(f) = value.as_f64() {
        return f.to_string();
    }
    value.as_str().map(str::to_string).unwrap_or_default()
}

/// Drop trailing empty lines from `lines`, mirroring pi's
/// `trimTrailingEmptyLines` (used by the read/write renderers before slicing to
/// the display window).
pub fn trim_trailing_empty_lines(lines: &[String]) -> &[String] {
    let mut end = lines.len();
    while end > 0 && lines[end - 1].is_empty() {
        end -= 1;
    }
    &lines[..end]
}

/// A default `pidgin_tui` [`KeybindingsManager`](TuiKeybindingsManager) built
/// from the coding-agent's platform keybinding table with no user overrides —
/// the Rust analog of pi's global `getKeybindings()` in the byte-exact setting
/// (no user config). The tool renderers use it for the `app.tools.expand` hint.
fn app_keybindings_manager() -> TuiKeybindingsManager {
    let owned: Vec<(String, TuiKeybindingDefinition)> = keybindings_for(Platform::current())
        .into_iter()
        .map(|(id, def)| {
            let keys = match def.default_keys {
                Keys::One(k) => vec![k],
                Keys::Many(v) => v,
            };
            (
                id,
                TuiKeybindingDefinition {
                    default_keys: keys,
                    description: Some(def.description.to_string()),
                },
            )
        })
        .collect();
    let refs: Vec<(&str, TuiKeybindingDefinition)> = owned
        .iter()
        .map(|(id, def)| (id.as_str(), def.clone()))
        .collect();
    TuiKeybindingsManager::new(refs, Vec::new())
}

/// The `app.tools.expand` keybinding hint (`keyHint("app.tools.expand", …)`) — a
/// dim key label plus a muted, space-prefixed `description`. Used in the
/// tool-result "… (N more lines, to expand)" truncation notices.
pub fn tools_expand_hint(theme: &Theme, description: &str) -> String {
    key_hint(
        theme,
        &app_keybindings_manager(),
        "app.tools.expand",
        description,
    )
}

/// The `app.tools.expand` key text (`keyText("app.tools.expand")`) — the
/// resolved keys, uncapitalized. Used by the read compact-call expand hint.
pub fn tools_expand_key_text() -> String {
    key_text(&app_keybindings_manager(), "app.tools.expand")
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

    #[test]
    fn get_language_from_path_maps_known_extensions() {
        assert_eq!(get_language_from_path("main.rs"), Some("rust"));
        assert_eq!(get_language_from_path("a/b/app.tsx"), Some("typescript"));
        assert_eq!(get_language_from_path("Config.YAML"), Some("yaml"));
        // Unmapped and empty extensions -> None (the byte-exact read/write path).
        assert_eq!(get_language_from_path("notes.txt"), None);
        assert_eq!(get_language_from_path("noext"), None);
        assert_eq!(get_language_from_path("trailing."), None);
    }

    #[test]
    fn trim_trailing_empty_lines_drops_only_trailing_blanks() {
        let lines = vec![
            "a".to_string(),
            String::new(),
            "b".to_string(),
            String::new(),
            String::new(),
        ];
        assert_eq!(trim_trailing_empty_lines(&lines), &lines[..3]);
        let all_blank = vec![String::new(), String::new()];
        assert!(trim_trailing_empty_lines(&all_blank).is_empty());
    }

    #[test]
    fn json_number_display_formats_like_js_interpolation() {
        use serde_json::json;
        assert_eq!(json_number_display(&json!(100)), "100");
        assert_eq!(json_number_display(&json!(0)), "0");
    }

    #[test]
    fn get_text_output_from_blocks_joins_text_blocks() {
        let blocks = vec![
            ContentBlock::Text {
                text: "one".to_string(),
                text_signature: None,
            },
            ContentBlock::Text {
                text: "two\r".to_string(),
                text_signature: None,
            },
        ];
        assert_eq!(get_text_output_from_blocks(&blocks, false), "one\ntwo");
    }

    #[test]
    fn tools_expand_key_text_uses_ctrl_o_default_binding() {
        // app.tools.expand defaults to ctrl+o in the coding-agent keybinding
        // table, so the compact/expand hints resolve to that key.
        assert_eq!(tools_expand_key_text(), "ctrl+o");
    }
}
