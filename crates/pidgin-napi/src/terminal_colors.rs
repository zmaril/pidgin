//! Node-API surface for pi's terminal color parsers (`terminal-colors.ts`).
//!
//! These are the pure parsers pi uses to read the two terminal color signals it
//! understands — the OSC 11 background-color *response* and the DEC private mode
//! 2031 color-scheme *report*. Each is a faithful port living in
//! [`pidgin_tui::terminal_colors`] (`crates/pidgin-tui/src/terminal_colors.rs`,
//! itself a byte-exact port of `vendor/pi/packages/tui/src/terminal-colors.ts`),
//! and is exposed here under pi's own export names so the native `terminal-colors`
//! shim can re-export them verbatim.
//!
//! # Marshaling
//!
//! Every value crosses the boundary as a string, number, boolean, or a plain
//! `{ r, g, b }` object. Inputs are whole strings (a complete escape-sequence
//! frame); [`parse_osc11_background_color`] returns an [`RgbColorJs`] (or
//! `undefined` via `Option`), [`parse_terminal_color_scheme_report`] returns the
//! `"dark"`/`"light"` tag string (or `undefined`), and
//! [`is_osc11_background_color_response`] returns a boolean. No JS closures,
//! streams, stable object identity, or byte-boundary slicing is required — the
//! parsers are pure functions of their input string.

use napi_derive::napi;

use pidgin_tui::terminal_colors as tc;

/// An RGB color with 8-bit channels, mirroring pi's `RgbColor` interface
/// (`{ r, g, b }`). The channel values are always in `0..=255`; they cross to JS
/// as plain numbers.
#[napi(object)]
pub struct RgbColorJs {
    /// Red channel (0-255).
    pub r: u32,
    /// Green channel (0-255).
    pub g: u32,
    /// Blue channel (0-255).
    pub b: u32,
}

impl From<tc::RgbColor> for RgbColorJs {
    fn from(color: tc::RgbColor) -> Self {
        Self {
            r: color.r as u32,
            g: color.g as u32,
            b: color.b as u32,
        }
    }
}

/// pi's `isOsc11BackgroundColorResponse`: whether `data` is a well-formed OSC 11
/// background-color response frame.
#[napi(js_name = "isOsc11BackgroundColorResponse")]
pub fn is_osc11_background_color_response(data: String) -> bool {
    tc::is_osc11_background_color_response(&data)
}

/// pi's `parseOsc11BackgroundColor`: extract the RGB color from an OSC 11
/// background-color response, accepting the `#rrggbb`, `#rrrrggggbbbb`, and
/// `rgb:`/`rgba:` `rrrr/gggg/bbbb` forms. Returns `undefined` for anything else.
#[napi(js_name = "parseOsc11BackgroundColor")]
pub fn parse_osc11_background_color(data: String) -> Option<RgbColorJs> {
    tc::parse_osc11_background_color(&data).map(RgbColorJs::from)
}

/// pi's `parseTerminalColorSchemeReport`: parse a DEC private mode 2031
/// color-scheme report (`CSI ? 997 ; Ps n`) into `"light"` (`Ps == 2`) or
/// `"dark"` (any other matching value). Returns `undefined` when `data` is not a
/// report.
#[napi(js_name = "parseTerminalColorSchemeReport")]
pub fn parse_terminal_color_scheme_report(data: String) -> Option<String> {
    tc::parse_terminal_color_scheme_report(&data).map(|scheme| match scheme {
        tc::TerminalColorScheme::Dark => "dark".to_string(),
        tc::TerminalColorScheme::Light => "light".to_string(),
    })
}
