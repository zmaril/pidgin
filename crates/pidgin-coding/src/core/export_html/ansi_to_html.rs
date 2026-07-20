// straitjacket-allow-file:color — ANSI color palette; hardcoded color literals are the domain of this module.
//! ANSI escape code to HTML converter.
//!
//! Converts terminal ANSI color/style codes to HTML with inline styles.
//! Supports:
//! - Standard foreground colors (30-37) and bright variants (90-97)
//! - Standard background colors (40-47) and bright variants (100-107)
//! - 256-color palette (38;5;N and 48;5;N)
//! - RGB true color (38;2;R;G;B and 48;2;R;G;B)
//! - Text styles: bold (1), dim (2), italic (3), underline (4)
//! - Reset (0)
//!
//! Ported byte-for-byte from pi's `core/export-html/ansi-to-html.ts`. This is the
//! only HTML that the server side of the export generates; markdown and syntax
//! highlighting run client-side in the reader's browser.

/// Standard ANSI color palette (0-15).
const ANSI_COLORS: [&str; 16] = [
    "#000000", // 0: black
    "#800000", // 1: red
    "#008000", // 2: green
    "#808000", // 3: yellow
    "#000080", // 4: blue
    "#800080", // 5: magenta
    "#008080", // 6: cyan
    "#c0c0c0", // 7: white
    "#808080", // 8: bright black
    "#ff0000", // 9: bright red
    "#00ff00", // 10: bright green
    "#ffff00", // 11: bright yellow
    "#0000ff", // 12: bright blue
    "#ff00ff", // 13: bright magenta
    "#00ffff", // 14: bright cyan
    "#ffffff", // 15: bright white
];

/// Convert a 256-color index to a hex string.
fn color256_to_hex(index: i64) -> String {
    // Standard colors (0-15)
    if index < 16 {
        return ANSI_COLORS[index as usize].to_string();
    }

    // Color cube (16-231): 6x6x6 = 216 colors
    if index < 232 {
        let cube_index = index - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_component = |n: i64| if n == 0 { 0 } else { 55 + n * 40 };
        let to_hex = |n: i64| format!("{:02x}", to_component(n));
        return format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b));
    }

    // Grayscale (232-255): 24 shades
    let gray = 8 + (index - 232) * 10;
    let gray_hex = format!("{gray:02x}");
    format!("#{gray_hex}{gray_hex}{gray_hex}")
}

/// Escape HTML special characters.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;")
}

#[derive(Clone)]
struct TextStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl TextStyle {
    fn empty() -> Self {
        TextStyle {
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
        }
    }

    fn reset(&mut self) {
        self.fg = None;
        self.bg = None;
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
    }
}

fn style_to_inline_css(style: &TextStyle) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(fg) = &style.fg {
        parts.push(format!("color:{fg}"));
    }
    if let Some(bg) = &style.bg {
        parts.push(format!("background-color:{bg}"));
    }
    if style.bold {
        parts.push("font-weight:bold".to_string());
    }
    if style.dim {
        parts.push("opacity:0.6".to_string());
    }
    if style.italic {
        parts.push("font-style:italic".to_string());
    }
    if style.underline {
        parts.push("text-decoration:underline".to_string());
    }
    parts.join(";")
}

fn has_style(style: &TextStyle) -> bool {
    style.fg.is_some()
        || style.bg.is_some()
        || style.bold
        || style.dim
        || style.italic
        || style.underline
}

/// Parse an extended color sequence (`38`/`48` followed by `5;N` or `2;R;G;B`).
///
/// Returns the resolved color and the number of extra parameters consumed, or
/// `None` when the parameters do not form a complete extended-color sequence (in
/// which case the caller leaves the style untouched, matching pi's behavior).
fn parse_extended_color(params: &[i64], i: usize) -> Option<(String, usize)> {
    if params.get(i + 1) == Some(&5) && params.len() > i + 2 {
        // 256-color: 5;N
        Some((color256_to_hex(params[i + 2]), 2))
    } else if params.get(i + 1) == Some(&2) && params.len() > i + 4 {
        // RGB: 2;R;G;B
        let r = params[i + 2];
        let g = params[i + 3];
        let b = params[i + 4];
        Some((format!("rgb({r},{g},{b})"), 4))
    } else {
        None
    }
}

/// Parse ANSI SGR (Select Graphic Rendition) codes and update style.
fn apply_sgr_code(params: &[i64], style: &mut TextStyle) {
    let mut i = 0;
    while i < params.len() {
        let code = params[i];

        if code == 0 {
            style.reset();
        } else if code == 1 {
            style.bold = true;
        } else if code == 2 {
            style.dim = true;
        } else if code == 3 {
            style.italic = true;
        } else if code == 4 {
            style.underline = true;
        } else if code == 22 {
            // Reset bold/dim
            style.bold = false;
            style.dim = false;
        } else if code == 23 {
            style.italic = false;
        } else if code == 24 {
            style.underline = false;
        } else if (30..=37).contains(&code) {
            // Standard foreground colors
            style.fg = Some(ANSI_COLORS[(code - 30) as usize].to_string());
        } else if code == 38 {
            // Extended foreground color
            if let Some((color, consumed)) = parse_extended_color(params, i) {
                style.fg = Some(color);
                i += consumed;
            }
        } else if code == 39 {
            // Default foreground
            style.fg = None;
        } else if (40..=47).contains(&code) {
            // Standard background colors
            style.bg = Some(ANSI_COLORS[(code - 40) as usize].to_string());
        } else if code == 48 {
            // Extended background color
            if let Some((color, consumed)) = parse_extended_color(params, i) {
                style.bg = Some(color);
                i += consumed;
            }
        } else if code == 49 {
            // Default background
            style.bg = None;
        } else if (90..=97).contains(&code) {
            // Bright foreground colors
            style.fg = Some(ANSI_COLORS[(code - 90 + 8) as usize].to_string());
        } else if (100..=107).contains(&code) {
            // Bright background colors
            style.bg = Some(ANSI_COLORS[(code - 100 + 8) as usize].to_string());
        }
        // Ignore unrecognized codes

        i += 1;
    }
}

/// Split a captured SGR parameter string into numeric codes.
///
/// Mirrors pi's `paramStr ? paramStr.split(";").map(p => parseInt(p, 10) || 0) : [0]`:
/// an empty parameter string yields a single reset code, and any non-numeric part
/// becomes `0`.
fn parse_params(param_str: &str) -> Vec<i64> {
    if param_str.is_empty() {
        return vec![0];
    }
    param_str
        .split(';')
        .map(|p| parse_int_prefix(p).unwrap_or(0))
        .collect()
}

/// Emulate JavaScript `parseInt(s, 10)`: read an optional leading run of digits
/// and ignore any trailing characters. The SGR capture only ever contains digits
/// and semicolons, so this reduces to a plain decimal parse of each segment.
fn parse_int_prefix(s: &str) -> Option<i64> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<i64>().ok()
}

/// If an ANSI SGR escape (`\x1b[` followed by `[0-9;]*` and a terminating `m`)
/// begins at byte `i`, return the byte index of its terminating `m`. Returns
/// `None` otherwise.
///
/// Mirrors the regex `/\x1b\[([\d;]*)m/g`. All bytes of a sequence are ASCII, so
/// byte indices stay on UTF-8 boundaries. Shared by the HTML converter and the
/// blank-line detector in `tool_renderer` so the scan lives in one place.
pub(super) fn ansi_escape_end(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes[i] != 0x1b || i + 1 >= bytes.len() || bytes[i + 1] != b'[' {
        return None;
    }
    let mut j = i + 2;
    while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'm' {
        Some(j)
    } else {
        None
    }
}

/// Convert ANSI-escaped text to HTML with inline styles.
pub fn ansi_to_html(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut style = TextStyle::empty();
    let mut result = String::new();
    let mut last_index = 0usize;
    let mut in_span = false;

    let mut i = 0usize;
    while i < bytes.len() {
        let Some(j) = ansi_escape_end(bytes, i) else {
            i += 1;
            continue;
        };

        // Add text before this escape sequence.
        let before = &text[last_index..i];
        if !before.is_empty() {
            result.push_str(&escape_html(before));
        }

        // Parse SGR parameters.
        let params = parse_params(&text[i + 2..j]);

        // Close existing span if we have one.
        if in_span {
            result.push_str("</span>");
            in_span = false;
        }

        // Apply the codes.
        apply_sgr_code(&params, &mut style);

        // Open new span if we have any styling.
        if has_style(&style) {
            result.push_str(&format!("<span style=\"{}\">", style_to_inline_css(&style)));
            in_span = true;
        }

        last_index = j + 1;
        i = j + 1;
    }

    // Add remaining text.
    let remaining = &text[last_index..];
    if !remaining.is_empty() {
        result.push_str(&escape_html(remaining));
    }

    // Close any open span.
    if in_span {
        result.push_str("</span>");
    }

    result
}

/// Convert an array of ANSI-escaped lines to HTML.
///
/// Each line is wrapped in a `div` element; empty lines render a non-breaking
/// space so they keep their height.
pub fn ansi_lines_to_html<S: AsRef<str>>(lines: &[S]) -> String {
    lines
        .iter()
        .map(|line| {
            let inner = ansi_to_html(line.as_ref());
            let inner = if inner.is_empty() {
                "&nbsp;".to_string()
            } else {
                inner
            };
            format!("<div class=\"ansi-line\">{inner}</div>")
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_lines_without_source_whitespace() {
        // Ported from export-html-whitespace.test.ts: no source whitespace is
        // inserted between ANSI-rendered lines.
        assert_eq!(
            ansi_lines_to_html(&["one", "two"]),
            r#"<div class="ansi-line">one</div><div class="ansi-line">two</div>"#
        );
    }

    #[test]
    fn renders_red_foreground_as_span() {
        assert_eq!(
            ansi_to_html("\u{1b}[31mone\u{1b}[0m"),
            r#"<span style="color:#800000">one</span>"#
        );
    }

    #[test]
    fn empty_line_renders_nbsp() {
        assert_eq!(
            ansi_lines_to_html(&[""]),
            r#"<div class="ansi-line">&nbsp;</div>"#
        );
    }

    #[test]
    fn escapes_html_special_characters() {
        assert_eq!(
            ansi_to_html("<a> & \"b\" 'c'"),
            "&lt;a&gt; &amp; &quot;b&quot; &#039;c&#039;"
        );
    }

    #[test]
    fn resets_close_the_open_span() {
        assert_eq!(
            ansi_to_html("\u{1b}[1;31mbold\u{1b}[0m plain"),
            r#"<span style="color:#800000;font-weight:bold">bold</span> plain"#
        );
    }

    #[test]
    fn parses_256_color_foreground() {
        // 38;5;24 maps to the color cube: #005f87.
        assert_eq!(
            ansi_to_html("\u{1b}[38;5;24mx\u{1b}[0m"),
            r#"<span style="color:#005f87">x</span>"#
        );
    }

    #[test]
    fn parses_rgb_truecolor_background() {
        assert_eq!(
            ansi_to_html("\u{1b}[48;2;10;20;30mx\u{1b}[0m"),
            r#"<span style="background-color:rgb(10,20,30)">x</span>"#
        );
    }
}
