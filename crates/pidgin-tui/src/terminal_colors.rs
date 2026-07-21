//! Byte-exact port of pi's `vendor/pi/packages/tui/src/terminal-colors.ts`.
//!
//! Pure parsers for the two terminal color signals pi understands:
//!
//! * the DEC private mode 2031 color-scheme *report* — `CSI ? 997 ; Ps n`, where
//!   `Ps` is `1` (dark) or `2` (light) — parsed by
//!   [`parse_terminal_color_scheme_report`]; and
//! * the OSC 11 background-color *response* — `OSC 11 ; <color> ST` — recognized
//!   by [`is_osc11_background_color_response`] and parsed by
//!   [`parse_osc11_background_color`] (which accepts `#rrggbb`, `#rrrrggggbbbb`,
//!   and `rgb:rrrr/gggg/bbbb` color forms via [`parse_osc_hex_channel`]).
//!
//! Every function here is a pure function of its input string; the regexes match
//! pi's exactly, including anchoring semantics (fancy-regex `$`, like JavaScript
//! `$` without the multiline flag, matches only at the very end of the string —
//! not before a trailing newline — so behavior is byte-identical).

use fancy_regex::Regex;
use std::sync::LazyLock;

/// An RGB color with 8-bit channels, mirroring pi's `RgbColor` interface
/// (`{ r, g, b }`). Channel values are always in the `0..=255` range because
/// every producer here scales into that range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    /// Red channel (0-255).
    pub r: u8,
    /// Green channel (0-255).
    pub g: u8,
    /// Blue channel (0-255).
    pub b: u8,
}

/// The terminal's reported color scheme, mirroring pi's
/// `TerminalColorScheme = "dark" | "light"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColorScheme {
    /// A dark background (`CSI ? 997 ; 1 n`, or any non-`2` report value).
    Dark,
    /// A light background (`CSI ? 997 ; 2 n`).
    Light,
}

/// Port of pi's private `hexToRgb`: strip a leading `#`, then read three 8-bit
/// hex channels. Callers gate this behind a `^[0-9a-f]{6}$` check, so the slices
/// are always valid hex.
fn hex_to_rgb(hex: &str) -> RgbColor {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    let r = u8::from_str_radix(&normalized[0..2], 16).unwrap();
    let g = u8::from_str_radix(&normalized[2..4], 16).unwrap();
    let b = u8::from_str_radix(&normalized[4..6], 16).unwrap();
    RgbColor { r, g, b }
}

/// Port of pi's private `parseOscHexChannel`: an OSC 11 color channel is a
/// variable-width hex string; scale it from its own maximum (`16^len - 1`) onto
/// the `0..=255` range. Returns `None` for non-hex or degenerate input.
pub fn parse_osc_hex_channel(channel: &str) -> Option<u8> {
    if !OSC_HEX_CHANNEL_PATTERN.is_match(channel).unwrap_or(false) {
        return None;
    }
    // JavaScript computes `16 ** channel.length - 1` as a double; mirror that in
    // f64 so the rounding matches bit-for-bit for realistic channel widths.
    let max = 16f64.powi(channel.len() as i32) - 1.0;
    if max <= 0.0 {
        return None;
    }
    // The pattern guarantees `channel` is pure hex; parse as u128 (ample for any
    // realistic width) before promoting to f64, matching JS `parseInt(_, 16)`.
    let value = u128::from_str_radix(channel, 16).ok()? as f64;
    // `Math.round` and Rust `f64::round` agree for non-negative values.
    Some(((value / max) * 255.0).round() as u8)
}

/// `OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN` from pi.
static OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\x1b\]11;([^\x07\x1b]*)(?:\x07|\x1b\\)$").unwrap());
/// `COLOR_SCHEME_REPORT_PATTERN` from pi.
static COLOR_SCHEME_REPORT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[\?997;(1|2)n$").unwrap());
/// Validates a single OSC 11 hex channel (pi's inline `/^[0-9a-f]+$/i`).
static OSC_HEX_CHANNEL_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]+$").unwrap());
/// `#rrggbb` validator (pi's inline `/^[0-9a-f]{6}$/i`).
static HEX6_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{6}$").unwrap());
/// `#rrrrggggbbbb` validator (pi's inline `/^[0-9a-f]{12}$/i`).
static HEX12_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{12}$").unwrap());
/// Leading `rgb:`/`rgba:` prefix (pi's inline `/^rgba?:/i`).
static RGB_PREFIX_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^rgba?:").unwrap());

/// Port of pi's `isOsc11BackgroundColorResponse`: whether `data` is a
/// well-formed OSC 11 background-color response frame.
pub fn is_osc11_background_color_response(data: &str) -> bool {
    OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN
        .is_match(data)
        .unwrap_or(false)
}

/// Port of pi's `parseOsc11BackgroundColor`: extract the RGB color from an
/// OSC 11 background-color response. Accepts the `#rrggbb`, `#rrrrggggbbbb`, and
/// `rgb:`/`rgba:` `rrrr/gggg/bbbb` forms; returns `None` for anything else.
pub fn parse_osc11_background_color(data: &str) -> Option<RgbColor> {
    let captures = OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN
        .captures(data)
        .ok()
        .flatten()?;
    let value = captures.get(1).map(|m| m.as_str()).unwrap_or("");
    let value = value.trim();

    if let Some(hex) = value.strip_prefix('#') {
        if HEX6_PATTERN.is_match(hex).unwrap_or(false) {
            return Some(hex_to_rgb(value));
        }
        if HEX12_PATTERN.is_match(hex).unwrap_or(false) {
            let r = parse_osc_hex_channel(&hex[0..4]);
            let g = parse_osc_hex_channel(&hex[4..8]);
            let b = parse_osc_hex_channel(&hex[8..12]);
            return match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Some(RgbColor { r, g, b }),
                _ => None,
            };
        }
        return None;
    }

    let rgb_value = RGB_PREFIX_PATTERN.replace(value, "");
    let mut parts = rgb_value.split('/');
    let red = parts.next();
    let green = parts.next();
    let blue = parts.next();
    let (red, green, blue) = match (red, green, blue) {
        (Some(red), Some(green), Some(blue)) => (red, green, blue),
        _ => return None,
    };
    let r = parse_osc_hex_channel(red);
    let g = parse_osc_hex_channel(green);
    let b = parse_osc_hex_channel(blue);
    match (r, g, b) {
        (Some(r), Some(g), Some(b)) => Some(RgbColor { r, g, b }),
        _ => None,
    }
}

/// Port of pi's `parseTerminalColorSchemeReport`: parse a DEC private mode 2031
/// color-scheme report (`CSI ? 997 ; Ps n`). `Ps == "2"` is light; anything else
/// that matches the pattern is dark. Returns `None` if `data` is not a report.
pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    let captures = COLOR_SCHEME_REPORT_PATTERN.captures(data).ok().flatten()?;
    let ps = captures.get(1).map(|m| m.as_str()).unwrap_or("");
    Some(if ps == "2" {
        TerminalColorScheme::Light
    } else {
        TerminalColorScheme::Dark
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_scheme_report_dark() {
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;1n"),
            Some(TerminalColorScheme::Dark)
        );
    }

    #[test]
    fn color_scheme_report_light() {
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;2n"),
            Some(TerminalColorScheme::Light)
        );
    }

    #[test]
    fn color_scheme_report_negatives() {
        // Wrong number, missing pieces, trailing junk, and a trailing newline
        // (JS `$` semantics: only the very end matches) must all be rejected.
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;3n"), None);
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?996;1n"), None);
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;1n\n"), None);
        assert_eq!(parse_terminal_color_scheme_report(" \x1b[?997;1n"), None);
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;12n"), None);
        assert_eq!(parse_terminal_color_scheme_report(""), None);
    }

    #[test]
    fn osc11_response_recognition() {
        assert!(is_osc11_background_color_response(
            "\x1b]11;rgb:1111/2222/3333\x07"
        ));
        assert!(is_osc11_background_color_response(
            "\x1b]11;rgb:1111/2222/3333\x1b\\"
        ));
        assert!(!is_osc11_background_color_response(
            "\x1b]11;rgb:1111/2222/3333"
        ));
        assert!(!is_osc11_background_color_response(
            "\x1b]11;rgb:1111/2222/3333\x07\n"
        ));
        assert!(!is_osc11_background_color_response("not an osc response"));
    }

    #[test]
    fn osc11_hash_rrggbb() {
        assert_eq!(
            parse_osc11_background_color("\x1b]11;#ff8040\x07"),
            Some(RgbColor {
                r: 0xff,
                g: 0x80,
                b: 0x40
            })
        );
        // Case-insensitive hex, ST terminator.
        assert_eq!(
            parse_osc11_background_color("\x1b]11;#FF8040\x1b\\"),
            Some(RgbColor {
                r: 0xff,
                g: 0x80,
                b: 0x40
            })
        );
    }

    #[test]
    fn osc11_hash_rrrrggggbbbb() {
        // Each 16-bit channel scaled down to 8 bits: 0xffff -> 255, 0x0000 -> 0,
        // 0x8080 -> round(0x8080/0xffff * 255) = 128.
        assert_eq!(
            parse_osc11_background_color("\x1b]11;#ffff00008080\x07"),
            Some(RgbColor {
                r: 255,
                g: 0,
                b: 128
            })
        );
    }

    #[test]
    fn osc11_rgb_slash_form() {
        assert_eq!(
            parse_osc11_background_color("\x1b]11;rgb:ffff/0000/8080\x07"),
            Some(RgbColor {
                r: 255,
                g: 0,
                b: 128
            })
        );
        // rgba: prefix and short (single-hex-digit) channels.
        assert_eq!(
            parse_osc11_background_color("\x1b]11;rgba:f/0/8\x07"),
            Some(RgbColor {
                r: 255,
                g: 0,
                // round(0x8/0xf * 255) = round(136.0) = 136
                b: 136
            })
        );
    }

    #[test]
    fn osc11_negatives() {
        // Not an OSC 11 frame at all.
        assert_eq!(parse_osc11_background_color("\x1b[?997;1n"), None);
        // `#` form with an invalid length (5 hex digits).
        assert_eq!(parse_osc11_background_color("\x1b]11;#ff804\x07"), None);
        // Non-hex in a `#` channel.
        assert_eq!(parse_osc11_background_color("\x1b]11;#gg8040\x07"), None);
        // Missing a channel in the slash form.
        assert_eq!(
            parse_osc11_background_color("\x1b]11;rgb:ffff/0000\x07"),
            None
        );
        // Non-hex channel in the slash form.
        assert_eq!(
            parse_osc11_background_color("\x1b]11;rgb:zzzz/0000/8080\x07"),
            None
        );
    }

    #[test]
    fn osc_hex_channel_width_scaling() {
        // Full-scale at every width maps to 255.
        assert_eq!(parse_osc_hex_channel("f"), Some(255));
        assert_eq!(parse_osc_hex_channel("ff"), Some(255));
        assert_eq!(parse_osc_hex_channel("fff"), Some(255));
        assert_eq!(parse_osc_hex_channel("ffff"), Some(255));
        // Zero at every width maps to 0.
        assert_eq!(parse_osc_hex_channel("0"), Some(0));
        assert_eq!(parse_osc_hex_channel("0000"), Some(0));
        // Midpoints: round(0x80/0xff * 255) = 128; round(0x8000/0xffff * 255) = 128.
        assert_eq!(parse_osc_hex_channel("80"), Some(128));
        assert_eq!(parse_osc_hex_channel("8000"), Some(128));
        // round(0x8/0xf * 255) = round(136.0) = 136.
        assert_eq!(parse_osc_hex_channel("8"), Some(136));
        // Case-insensitive.
        assert_eq!(parse_osc_hex_channel("FF"), Some(255));
        // Non-hex and empty are rejected.
        assert_eq!(parse_osc_hex_channel("gg"), None);
        assert_eq!(parse_osc_hex_channel(""), None);
    }
}
