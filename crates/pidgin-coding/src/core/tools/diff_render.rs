//! Diff formatter ported from pi's
//! `modes/interactive/components/diff.ts` (`parseDiffLine`,
//! `renderIntraLineDiff`, `renderDiff`).
//!
//! This is the pure formatter half of pi's `diff.ts`: it turns the
//! display-diff string produced by `edit-diff.ts::generate_diff_string` into a
//! colored, line-numbered block. The component wrapper and its final home under
//! `modes/interactive/components/` are handled separately by the TUI lane; this
//! module owns only the string logic.
//!
//! # Intra-line highlighting deferral
//!
//! pi's `renderIntraLineDiff` uses the npm `diff` library's `diffWords` to
//! inverse-highlight the changed tokens within a single modified line. A
//! byte-exact port of `diffWords` (its tokenizer + LCS + whitespace merging) is
//! its own slice; until it lands, [`render_intra_line_diff`] is a faithful
//! passthrough (no inverse SGR). The single-removed/single-added branch of
//! [`render_diff`] therefore renders identically to the multi-line hunk branch
//! — byte-exact for every case **except** the per-token inverse highlighting on
//! a single modified line, which is deferred.

// straitjacket-allow-file:duplication

use std::sync::OnceLock;

use regex::Regex;

use crate::modes::interactive::theme::runtime::Theme;

/// Local `theme.fg` wrapper falling back to unstyled text on an unknown color
/// key (pi's `theme.fg` is infallible; the ported [`Theme::fg`] returns a
/// `Result`).
fn fg(theme: &Theme, color: &str, text: &str) -> String {
    theme.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// Replace tabs with spaces for consistent rendering (pi's local `replaceTabs`).
fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// A parsed diff line: prefix (`+`/`-`/space), line-number field, and content.
struct ParsedDiffLine {
    prefix: char,
    line_num: String,
    content: String,
}

/// Parse a diff line to extract prefix, line number, and content.
/// Format: `"+123 content"` or `"-123 content"` or `" 123 content"` or
/// `"     ..."`. Mirrors pi's `parseDiffLine` regex `^([+-\s])(\s*\d*)\s(.*)$`.
fn parse_diff_line(line: &str) -> Option<ParsedDiffLine> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^([-+\s])(\s*\d*)\s(.*)$").unwrap());
    let caps = re.captures(line)?;
    Some(ParsedDiffLine {
        prefix: caps[1].chars().next().unwrap(),
        line_num: caps[2].to_string(),
        content: caps[3].to_string(),
    })
}

/// Compute word-level intra-line highlighting for a single modified line.
///
/// **Deferred:** pi runs `diffWords` and wraps changed tokens in
/// `theme.inverse`. Pending a byte-exact `diffWords` port this returns the
/// inputs unchanged, so the caller emits the same bytes as the multi-line hunk
/// path. See the module-level deferral note.
fn render_intra_line_diff(old_content: &str, new_content: &str) -> (String, String) {
    (old_content.to_string(), new_content.to_string())
}

/// Render a diff string with colored lines and (deferred) intra-line change
/// highlighting. Mirrors pi's `renderDiff`:
/// - Context lines: dim/gray (`toolDiffContext`)
/// - Removed lines: red (`toolDiffRemoved`)
/// - Added lines: green (`toolDiffAdded`)
///
/// Returns the joined multi-line string pi returns (`result.join("\n")`).
pub fn render_diff(diff_text: &str, theme: &Theme) -> String {
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let parsed = parse_diff_line(line);

        let Some(parsed) = parsed else {
            result.push(fg(theme, "toolDiffContext", line));
            i += 1;
            continue;
        };

        if parsed.prefix == '-' {
            // Collect consecutive removed lines.
            let mut removed_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                match parse_diff_line(lines[i]) {
                    Some(p) if p.prefix == '-' => {
                        removed_lines.push((p.line_num, p.content));
                        i += 1;
                    }
                    _ => break,
                }
            }

            // Collect consecutive added lines.
            let mut added_lines: Vec<(String, String)> = Vec::new();
            while i < lines.len() {
                match parse_diff_line(lines[i]) {
                    Some(p) if p.prefix == '+' => {
                        added_lines.push((p.line_num, p.content));
                        i += 1;
                    }
                    _ => break,
                }
            }

            // Only do intra-line diffing when there's exactly one removed and
            // one added line (a single-line modification). Otherwise, show
            // lines as-is.
            if removed_lines.len() == 1 && added_lines.len() == 1 {
                let (removed_num, removed_content) = &removed_lines[0];
                let (added_num, added_content) = &added_lines[0];

                let (removed_line, added_line) = render_intra_line_diff(
                    &replace_tabs(removed_content),
                    &replace_tabs(added_content),
                );

                result.push(fg(
                    theme,
                    "toolDiffRemoved",
                    &format!("-{removed_num} {removed_line}"),
                ));
                result.push(fg(
                    theme,
                    "toolDiffAdded",
                    &format!("+{added_num} {added_line}"),
                ));
            } else {
                // Show all removed lines first, then all added lines.
                for (num, content) in &removed_lines {
                    result.push(fg(
                        theme,
                        "toolDiffRemoved",
                        &format!("-{num} {}", replace_tabs(content)),
                    ));
                }
                for (num, content) in &added_lines {
                    result.push(fg(
                        theme,
                        "toolDiffAdded",
                        &format!("+{num} {}", replace_tabs(content)),
                    ));
                }
            }
        } else if parsed.prefix == '+' {
            // Standalone added line.
            result.push(fg(
                theme,
                "toolDiffAdded",
                &format!("+{} {}", parsed.line_num, replace_tabs(&parsed.content)),
            ));
            i += 1;
        } else {
            // Context line.
            result.push(fg(
                theme,
                "toolDiffContext",
                &format!(" {} {}", parsed.line_num, replace_tabs(&parsed.content)),
            ));
            i += 1;
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode};
    use std::path::PathBuf;

    /// The 256-color dark theme, loaded the same way the interactive vector
    /// tests build it.
    fn dark_theme() -> Theme {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/modes/interactive/theme/dark.json");
        let content = std::fs::read_to_string(&path).expect("read dark.json");
        let json = parse_theme_json(&content).expect("parse dark.json");
        create_theme(&json, Some(ColorMode::Color256), None).expect("create dark theme")
    }

    #[test]
    fn parse_diff_line_splits_prefix_num_content() {
        let p = parse_diff_line("-12 old text").unwrap();
        assert_eq!(p.prefix, '-');
        assert_eq!(p.line_num, "12");
        assert_eq!(p.content, "old text");

        let c = parse_diff_line(" 3 context").unwrap();
        assert_eq!(c.prefix, ' ');
        assert_eq!(c.line_num, "3");
        assert_eq!(c.content, "context");
    }

    #[test]
    fn parse_diff_line_rejects_unparseable() {
        // No leading prefix + whitespace-separated field.
        assert!(parse_diff_line("...").is_none());
        assert!(parse_diff_line("").is_none());
    }

    #[test]
    fn render_diff_colors_context_removed_added_byte_exact() {
        let theme = dark_theme();
        // A context line, then a single-line modification (intra-line branch,
        // highlighting deferred), then a multi-line hunk.
        let diff = " 1 unchanged\n-2 old line\n+2 new line\n-3 removed a\n-4 removed b\n+3 added a";
        let out = render_diff(diff, &theme);
        let expected = concat!(
            "\u{1b}[38;5;244m 1 unchanged\u{1b}[39m\n",
            "\u{1b}[38;5;167m-2 old line\u{1b}[39m\n",
            "\u{1b}[38;5;143m+2 new line\u{1b}[39m\n",
            "\u{1b}[38;5;167m-3 removed a\u{1b}[39m\n",
            "\u{1b}[38;5;167m-4 removed b\u{1b}[39m\n",
            "\u{1b}[38;5;143m+3 added a\u{1b}[39m",
        );
        assert_eq!(out, expected);
    }

    #[test]
    fn render_diff_standalone_add_and_tab_expansion() {
        let theme = dark_theme();
        // A standalone added line whose content carries a tab (→ three spaces).
        let out = render_diff("+7 \tindented", &theme);
        assert_eq!(out, "\u{1b}[38;5;143m+7    indented\u{1b}[39m");
    }

    #[test]
    fn render_diff_unparseable_line_is_context_colored() {
        let theme = dark_theme();
        let out = render_diff("...", &theme);
        assert_eq!(out, "\u{1b}[38;5;244m...\u{1b}[39m");
    }
}
