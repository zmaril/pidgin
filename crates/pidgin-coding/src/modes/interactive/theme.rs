//! Theme data model and color math, ported from pi's
//! `modes/interactive/theme/theme.ts`.
//!
//! This is the **pure subset** of pi's theme module: the JSON data model, the
//! variable-reference resolver, the 256-color→hex converter, the resolved-color
//! and export-color helpers consumed by the HTML export, and the pure terminal
//! theme-detection helpers (COLORFGBG parsing and WCAG-luminance classification).
//!
//! Deliberately **out of scope** (see the task spec and pi's source): the
//! `Theme` ANSI class (`fg`/`bg`/`getFgAnsi`), the `rgbTo256` quantizer
//! (`findClosestCube`/`colorDistance`), the global-instance `Proxy` plus
//! `initTheme`/`setTheme`/file watchers, and the TUI helpers
//! (`highlightCode`/`getMarkdownTheme`/`getSelectListTheme`). Those are ANSI- or
//! runtime-shaped and belong with the interactive UI, not this data layer.
//!
//! ## Naming note for the napi shim
//!
//! pi's exports are camelCase (`getThemeExportColors`, `getResolvedThemeColors`,
//! `isLightTheme`, `parseAutoThemeSetting`, `resolveThemeSetting`,
//! `getThemeForRgbColor`, `detectTerminalBackgroundFromEnv`). The Rust names are
//! snake_case equivalents; a shim maps one to the other 1:1.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use indexmap::IndexMap;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};

// ============================================================================
// Data model
// ============================================================================

/// A single color slot in a theme.
///
/// Mirrors pi's `ColorValue = string | number`:
/// - [`ColorValue::Hex`] holds any non-empty string — either a literal hex color
///   (`"#ff0000"`) **or** a variable reference (`"accent"`). The two are told
///   apart at resolution time by the leading `#`, exactly as pi does.
/// - [`ColorValue::Ansi256`] holds a 256-color palette index (0-255).
/// - [`ColorValue::Empty`] is the empty string `""`, meaning "terminal default".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorValue {
    /// A literal hex color or a variable reference (disambiguated by a leading `#`).
    Hex(String),
    /// A 256-color palette index (0-255).
    Ansi256(u8),
    /// The empty string — terminal-default color.
    Empty,
}

impl<'de> Deserialize<'de> for ColorValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorValueVisitor;

        impl Visitor<'_> for ColorValueVisitor {
            type Value = ColorValue;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a hex/variable string or a 256-color integer (0-255)")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<ColorValue, E> {
                if value.is_empty() {
                    Ok(ColorValue::Empty)
                } else {
                    Ok(ColorValue::Hex(value.to_string()))
                }
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<ColorValue, E> {
                u8::try_from(value).map(ColorValue::Ansi256).map_err(|_| {
                    E::custom(format!("256-color index out of range (0-255): {value}"))
                })
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<ColorValue, E> {
                u8::try_from(value).map(ColorValue::Ansi256).map_err(|_| {
                    E::custom(format!("256-color index out of range (0-255): {value}"))
                })
            }
        }

        deserializer.deserialize_any(ColorValueVisitor)
    }
}

/// Optional `export` block of a theme, giving explicit HTML-export backgrounds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ThemeExportSection {
    #[serde(rename = "pageBg", default)]
    pub page_bg: Option<ColorValue>,
    #[serde(rename = "cardBg", default)]
    pub card_bg: Option<ColorValue>,
    #[serde(rename = "infoBg", default)]
    pub info_bg: Option<ColorValue>,
}

/// A parsed theme file.
///
/// `vars` and `colors` are [`IndexMap`]s so JSON insertion order is preserved —
/// [`get_resolved_theme_colors`] emits colors in that order, matching pi's
/// `Object.entries` iteration that the HTML export depends on.
#[derive(Clone, Debug, Deserialize)]
pub struct ThemeJson {
    pub name: String,
    #[serde(default)]
    pub vars: IndexMap<String, ColorValue>,
    pub colors: IndexMap<String, ColorValue>,
    #[serde(default)]
    pub export: Option<ThemeExportSection>,
}

/// Explicit HTML-export background colors resolved from a theme's `export` block.
///
/// The canonical home for this type. `core::export_html::theme` currently defines
/// its own placeholder `ThemeExportColors`; that one can later be repointed to
/// reuse this one (a follow-up for whoever owns `export_html`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeExportColors {
    pub page_bg: Option<String>,
    pub card_bg: Option<String>,
    pub info_bg: Option<String>,
}

// ============================================================================
// Errors
// ============================================================================

/// Failures raised while resolving or loading themes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeError {
    /// A variable reference forms a cycle (`a -> b -> a`).
    CircularReference(String),
    /// A variable reference points at a name that does not exist in `vars`.
    VariableNotFound(String),
    /// A theme could not be located by name.
    ThemeNotFound(String),
    /// A theme file could not be read.
    Io(String),
    /// A theme file could not be parsed as JSON.
    Parse(String),
    /// A hex color string was not a valid `#rrggbb` value. Mirrors pi's
    /// `hexToRgb` throw ("Invalid hex color").
    InvalidHexColor(String),
    /// A resolved color value was neither empty, a 256-index, nor a `#hex`
    /// literal. Mirrors pi's `fgAnsi`/`bgAnsi` throw ("Invalid color value").
    InvalidColorValue(String),
    /// A theme name contained a reserved `/`. Mirrors pi's
    /// `assertThemeNameIsValid` throw.
    InvalidThemeName(String),
    /// A [`Theme::fg`] lookup referenced a foreground key that was not baked
    /// into the theme. Mirrors pi's "Unknown theme color".
    UnknownThemeColor(String),
    /// A [`Theme::bg`] lookup referenced a background key that was not baked
    /// into the theme. Mirrors pi's "Unknown theme background color".
    UnknownThemeBg(String),
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ThemeError::CircularReference(name) => {
                write!(f, "Circular variable reference detected: {name}")
            }
            ThemeError::VariableNotFound(name) => {
                write!(f, "Variable reference not found: {name}")
            }
            ThemeError::ThemeNotFound(name) => write!(f, "Theme not found: {name}"),
            ThemeError::Io(msg) => write!(f, "Failed to read theme: {msg}"),
            ThemeError::Parse(msg) => write!(f, "Failed to parse theme: {msg}"),
            ThemeError::InvalidHexColor(hex) => write!(f, "Invalid hex color: {hex}"),
            ThemeError::InvalidColorValue(color) => write!(f, "Invalid color value: {color}"),
            ThemeError::InvalidThemeName(name) => write!(
                f,
                "Invalid theme name \"{name}\": theme names cannot contain \"/\" because it is \
                 reserved for automatic light/dark theme settings."
            ),
            ThemeError::UnknownThemeColor(color) => write!(f, "Unknown theme color: {color}"),
            ThemeError::UnknownThemeBg(color) => {
                write!(f, "Unknown theme background color: {color}")
            }
        }
    }
}

impl std::error::Error for ThemeError {}

// ============================================================================
// Variable resolution
// ============================================================================

/// Recursively resolve a [`ColorValue`] through `vars`, following variable
/// references until a concrete value is reached.
///
/// The returned value is guaranteed to be reference-free: a literal
/// [`ColorValue::Hex`] (leading `#`), an [`ColorValue::Ansi256`], or
/// [`ColorValue::Empty`]. Mirrors pi's `resolveVarRefs`, including cycle
/// detection and the "variable not found" error.
pub fn resolve_var_refs(
    value: &ColorValue,
    vars: &IndexMap<String, ColorValue>,
) -> Result<ColorValue, ThemeError> {
    resolve_var_refs_inner(value, vars, &mut Vec::new())
}

fn resolve_var_refs_inner(
    value: &ColorValue,
    vars: &IndexMap<String, ColorValue>,
    visited: &mut Vec<String>,
) -> Result<ColorValue, ThemeError> {
    let name = match value {
        ColorValue::Ansi256(_) | ColorValue::Empty => return Ok(value.clone()),
        ColorValue::Hex(s) if s.starts_with('#') => return Ok(value.clone()),
        ColorValue::Hex(s) => s,
    };

    if visited.iter().any(|seen| seen == name) {
        return Err(ThemeError::CircularReference(name.clone()));
    }
    let next = vars
        .get(name)
        .ok_or_else(|| ThemeError::VariableNotFound(name.clone()))?;
    visited.push(name.clone());
    resolve_var_refs_inner(next, vars, visited)
}

/// Apply pi's `withThemeColorFallbacks`: `thinkingMax` defaults to `thinkingXhigh`
/// when absent. Returns a new ordered map so callers keep insertion order.
pub fn with_theme_color_fallbacks(
    colors: &IndexMap<String, ColorValue>,
) -> IndexMap<String, ColorValue> {
    let mut result = colors.clone();
    if !result.contains_key("thinkingMax") {
        if let Some(xhigh) = result.get("thinkingXhigh").cloned() {
            result.insert("thinkingMax".to_string(), xhigh);
        }
    }
    result
}

// ============================================================================
// 256-color → hex
// ============================================================================

/// Basic 16-color palette approximations (indices 0-15), matching common
/// terminal defaults.
const BASIC_ANSI_COLORS: [&str; 16] = [
    "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
    "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
];

/// Convert a 256-color index to a `#rrggbb` hex string.
///
/// - `0-15`: basic terminal colors (approximate)
/// - `16-231`: the 6×6×6 color cube
/// - `232-255`: the 24-step grayscale ramp
///
/// Mirrors pi's `ansi256ToHex`.
pub fn ansi256_to_hex(index: u8) -> String {
    if index < 16 {
        return BASIC_ANSI_COLORS[index as usize].to_string();
    }

    if index < 232 {
        let cube_index = u32::from(index) - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let channel = |n: u32| -> u32 {
            if n == 0 {
                0
            } else {
                55 + n * 40
            }
        };
        return format!("#{:02x}{:02x}{:02x}", channel(r), channel(g), channel(b));
    }

    let gray = 8 + (u32::from(index) - 232) * 10;
    format!("#{gray:02x}{gray:02x}{gray:02x}")
}

// ============================================================================
// Resolved / export colors
// ============================================================================

/// Default foreground for a light theme when a color is the terminal default.
const DEFAULT_LIGHT_TEXT: &str = "#000000";
/// Default foreground for a dark theme when a color is the terminal default.
const DEFAULT_DARK_TEXT: &str = "#e5e5e7";

/// Resolve every color in a theme to a CSS-ready hex string, in insertion order.
///
/// Mirrors pi's `getResolvedThemeColors`:
/// - variable references are resolved through `vars`;
/// - [`ColorValue::Ansi256`] indices become hex via [`ansi256_to_hex`];
/// - [`ColorValue::Empty`] becomes the light/dark default text color;
/// - literal hex passes through unchanged.
///
/// pi throws on an unresolvable reference; this returns [`ThemeError`] instead
/// (the napi shim can surface it as a thrown error).
pub fn get_resolved_theme_colors(
    theme: &ThemeJson,
    is_light: bool,
) -> Result<Vec<(String, String)>, ThemeError> {
    let default_text = if is_light {
        DEFAULT_LIGHT_TEXT
    } else {
        DEFAULT_DARK_TEXT
    };

    let colors = with_theme_color_fallbacks(&theme.colors);
    let mut resolved = Vec::with_capacity(colors.len());
    for (key, value) in &colors {
        let css = match resolve_var_refs(value, &theme.vars)? {
            ColorValue::Ansi256(index) => ansi256_to_hex(index),
            ColorValue::Empty => default_text.to_string(),
            ColorValue::Hex(hex) => hex,
        };
        resolved.push((key.clone(), css));
    }
    Ok(resolved)
}

/// Resolve a theme's explicit `export` block to concrete hex colors.
///
/// Mirrors pi's `getThemeExportColors`: each of `pageBg`/`cardBg`/`infoBg` is
/// resolved through `vars`; a 256-color index becomes hex; an empty value (or a
/// missing field) becomes `None`. **Any** resolution error yields the default
/// empty result, matching pi's `try/catch` returning `{}`.
pub fn get_theme_export_colors(theme: &ThemeJson) -> ThemeExportColors {
    resolve_export_colors(theme).unwrap_or_default()
}

fn resolve_export_colors(theme: &ThemeJson) -> Result<ThemeExportColors, ThemeError> {
    let Some(section) = theme.export.as_ref() else {
        return Ok(ThemeExportColors::default());
    };
    let resolve = |value: &Option<ColorValue>| -> Result<Option<String>, ThemeError> {
        let Some(value) = value else {
            return Ok(None);
        };
        Ok(match resolve_var_refs(value, &theme.vars)? {
            ColorValue::Ansi256(index) => Some(ansi256_to_hex(index)),
            ColorValue::Empty => None,
            ColorValue::Hex(hex) => Some(hex),
        })
    };
    Ok(ThemeExportColors {
        page_bg: resolve(&section.page_bg)?,
        card_bg: resolve(&section.card_bg)?,
        info_bg: resolve(&section.info_bg)?,
    })
}

/// Whether a theme name denotes a light theme. Mirrors pi's `isLightTheme`
/// (a plain name check today).
pub fn is_light_theme(name: &str) -> bool {
    name == "light"
}

// ============================================================================
// Theme loading
// ============================================================================

const DARK_THEME_JSON: &str = include_str!("theme/dark.json");
const LIGHT_THEME_JSON: &str = include_str!("theme/light.json");

/// Directories used to locate themes on disk. Built-ins are embedded and need no
/// path; `custom_themes_dir` is where custom `<name>.json` themes are read from.
#[derive(Clone, Debug, Default)]
pub struct ThemeDirs {
    /// Directory holding custom `<name>.json` theme files.
    pub custom_themes_dir: PathBuf,
}

/// Load a theme by name. Built-in `dark`/`light` are resolved from embedded JSON
/// with no filesystem access; any other name is read from
/// `dirs.custom_themes_dir/<name>.json`. Mirrors pi's `loadThemeJson`.
pub fn load_theme_json(name: &str, dirs: &ThemeDirs) -> Result<ThemeJson, ThemeError> {
    match name {
        "dark" => return parse_theme_json(DARK_THEME_JSON),
        "light" => return parse_theme_json(LIGHT_THEME_JSON),
        _ => {}
    }

    let path = dirs.custom_themes_dir.join(format!("{name}.json"));
    if !path.exists() {
        return Err(ThemeError::ThemeNotFound(name.to_string()));
    }
    let content = std::fs::read_to_string(&path).map_err(|e| ThemeError::Io(e.to_string()))?;
    parse_theme_json(&content)
}

/// Parse a theme from JSON text.
pub fn parse_theme_json(content: &str) -> Result<ThemeJson, ThemeError> {
    serde_json::from_str(content).map_err(|e| ThemeError::Parse(e.to_string()))
}

// ============================================================================
// Runtime theme loader (ANSI-baked)
// ============================================================================

pub mod runtime;

pub use runtime::{
    bg_ansi, create_theme, fg_ansi, hex_to_256, load_theme_from_path, parse_theme_json_content,
    rgb_to_256, ColorMode, Theme, ThinkingLevel,
};

// ============================================================================
// Active-theme singleton runtime (interactive layer)
// ============================================================================

pub mod active;

pub use active::{ActiveTheme, SetThemeResult, ThemeSource};

// ============================================================================
// Terminal theme detection (pure subset)
// ============================================================================

/// Which of the two built-in variants a terminal background implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalTheme {
    /// A dark terminal background.
    Dark,
    /// A light terminal background.
    Light,
}

/// An RGB color with 8-bit channels. Mirrors pi-tui's `RgbColor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Where a terminal-theme decision came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectionSource {
    /// Parsed from the `COLORFGBG` environment variable.
    ColorFgBg,
    /// No hint was found; the default was used.
    Fallback,
}

/// How confident a detection result is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    High,
    Low,
}

/// The outcome of environment-based terminal theme detection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalThemeDetection {
    pub theme: TerminalTheme,
    pub source: DetectionSource,
    pub detail: String,
    pub confidence: Confidence,
}

/// A parsed `light/dark` automatic-theme setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoThemeSetting {
    pub light_theme: String,
    pub dark_theme: String,
}

/// WCAG relative luminance of an RGB color, in `0.0..=1.0`. Mirrors pi's
/// `getRgbColorLuminance`.
fn rgb_luminance(color: RgbColor) -> f64 {
    let to_linear = |channel: u8| -> f64 {
        let value = f64::from(channel) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * to_linear(color.r) + 0.7152 * to_linear(color.g) + 0.0722 * to_linear(color.b)
}

/// Classify an RGB terminal background as light or dark by luminance
/// (`>= 0.5` → light). Mirrors pi's `getThemeForRgbColor`.
pub fn get_theme_for_rgb_color(rgb: RgbColor) -> TerminalTheme {
    if rgb_luminance(rgb) >= 0.5 {
        TerminalTheme::Light
    } else {
        TerminalTheme::Dark
    }
}

/// Luminance of a 256-color palette index, via its hex approximation.
fn ansi_color_luminance(index: u8) -> f64 {
    rgb_luminance(hex_to_rgb(&ansi256_to_hex(index)))
}

/// Parse a `#rrggbb` string produced by [`ansi256_to_hex`] into [`RgbColor`].
/// The input is always well-formed here, so malformed channels fall back to 0.
fn hex_to_rgb(hex: &str) -> RgbColor {
    let cleaned = hex.trim_start_matches('#');
    let channel = |range: std::ops::Range<usize>| -> u8 {
        cleaned
            .get(range)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .unwrap_or(0)
    };
    RgbColor {
        r: channel(0..2),
        g: channel(2..4),
        b: channel(4..6),
    }
}

/// Extract the background color index from a `COLORFGBG` value. The background is
/// the last field that parses as an integer in `0..=255`. Mirrors pi's
/// `getColorFgBgBackgroundIndex`.
fn colorfgbg_background_index(colorfgbg: &str) -> Option<u8> {
    colorfgbg
        .split(';')
        .rev()
        .find_map(|part| part.trim().parse::<u8>().ok())
}

/// Detect the terminal theme purely from an injected environment map, using the
/// `COLORFGBG` hint. Mirrors pi's `detectTerminalBackgroundFromEnv` (the pure,
/// env-only path — no terminal queries).
pub fn detect_terminal_background_from_env(
    env: &HashMap<String, String>,
) -> TerminalThemeDetection {
    let colorfgbg = env.get("COLORFGBG").map(String::as_str).unwrap_or("");
    if let Some(bg) = colorfgbg_background_index(colorfgbg) {
        let theme = if ansi_color_luminance(bg) >= 0.5 {
            TerminalTheme::Light
        } else {
            TerminalTheme::Dark
        };
        return TerminalThemeDetection {
            theme,
            source: DetectionSource::ColorFgBg,
            detail: format!("background color index {bg}"),
            confidence: Confidence::High,
        };
    }

    TerminalThemeDetection {
        theme: TerminalTheme::Dark,
        source: DetectionSource::Fallback,
        detail: "no terminal background hint found".to_string(),
        confidence: Confidence::Low,
    }
}

// ============================================================================
// Theme setting helpers
// ============================================================================

/// Parse a `light/dark` automatic-theme setting. Returns `None` unless the value
/// contains exactly one `/` with non-empty sides. Mirrors pi's
/// `parseAutoThemeSetting`.
pub fn parse_auto_theme_setting(setting: Option<&str>) -> Option<AutoThemeSetting> {
    let setting = setting?;
    let slash = setting.find('/')?;
    if setting[slash + 1..].contains('/') {
        return None;
    }
    let light_theme = setting[..slash].trim();
    let dark_theme = setting[slash + 1..].trim();
    if light_theme.is_empty() || dark_theme.is_empty() {
        return None;
    }
    Some(AutoThemeSetting {
        light_theme: light_theme.to_string(),
        dark_theme: dark_theme.to_string(),
    })
}

/// Resolve a theme setting against a detected terminal theme. An automatic
/// `light/dark` setting picks the matching side; a plain name is returned as-is;
/// a malformed slash setting yields `None`. Mirrors pi's `resolveThemeSetting`.
pub fn resolve_theme_setting(
    setting: Option<&str>,
    terminal_theme: TerminalTheme,
) -> Option<String> {
    if let Some(auto) = parse_auto_theme_setting(setting) {
        return Some(match terminal_theme {
            TerminalTheme::Light => auto.light_theme,
            TerminalTheme::Dark => auto.dark_theme,
        });
    }
    let setting = setting?;
    if setting.contains('/') {
        return None;
    }
    Some(setting.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the embedded `dark` theme as a self-contained base for building
    /// custom themes, avoiding any dependency on the vendor/pi runtime.
    fn base_dark_theme() -> ThemeJson {
        parse_theme_json(DARK_THEME_JSON).expect("embedded dark theme parses")
    }

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // --- ported from theme-export.test.ts -----------------------------------

    #[test]
    fn resolves_export_variable_references() {
        // "resolves export variable references using the same syntax as colors"
        let mut theme = base_dark_theme();
        theme.name = "custom-export-vars".to_string();
        theme.vars.insert(
            "pageBgVar".to_string(),
            ColorValue::Hex("#112233".to_string()),
        );
        theme.vars.insert(
            "pageBgAlias".to_string(),
            ColorValue::Hex("pageBgVar".to_string()),
        );
        theme.vars.insert(
            "infoBgVar".to_string(),
            ColorValue::Hex("#445566".to_string()),
        );
        theme.vars.insert(
            "cardBgVar".to_string(),
            ColorValue::Hex("#223344".to_string()),
        );
        theme.export = Some(ThemeExportSection {
            page_bg: Some(ColorValue::Hex("pageBgAlias".to_string())),
            card_bg: Some(ColorValue::Hex("cardBgVar".to_string())),
            info_bg: Some(ColorValue::Hex("infoBgVar".to_string())),
        });

        assert_eq!(
            get_theme_export_colors(&theme),
            ThemeExportColors {
                page_bg: Some("#112233".to_string()),
                card_bg: Some("#223344".to_string()),
                info_bg: Some("#445566".to_string()),
            }
        );
    }

    #[test]
    fn resolves_recursive_vars_and_converts_ansi256_export_values() {
        // "resolves recursive vars and converts 256-color export values to hex"
        let mut theme = base_dark_theme();
        theme.name = "custom-export-recursive".to_string();
        theme.vars.insert(
            "deepPageBg".to_string(),
            ColorValue::Hex("#abcdef".to_string()),
        );
        theme.vars.insert(
            "pageBgAlias".to_string(),
            ColorValue::Hex("deepPageBg".to_string()),
        );
        theme
            .vars
            .insert("cardBgAnsi".to_string(), ColorValue::Ansi256(24));
        theme.export = Some(ThemeExportSection {
            page_bg: Some(ColorValue::Hex("pageBgAlias".to_string())),
            card_bg: Some(ColorValue::Hex("cardBgAnsi".to_string())),
            info_bg: Some(ColorValue::Empty),
        });

        assert_eq!(
            get_theme_export_colors(&theme),
            ThemeExportColors {
                page_bg: Some("#abcdef".to_string()),
                card_bg: Some("#005f87".to_string()),
                info_bg: None,
            }
        );
    }

    // --- ported from theme-detection.test.ts (pure subset) ------------------

    #[test]
    fn detects_background_from_colorfgbg() {
        struct Case {
            env: Vec<(&'static str, &'static str)>,
            theme: TerminalTheme,
            source: DetectionSource,
            confidence: Confidence,
        }
        let cases = [
            Case {
                env: vec![("COLORFGBG", "0;15")],
                theme: TerminalTheme::Light,
                source: DetectionSource::ColorFgBg,
                confidence: Confidence::High,
            },
            Case {
                env: vec![("COLORFGBG", "15;0")],
                theme: TerminalTheme::Dark,
                source: DetectionSource::ColorFgBg,
                confidence: Confidence::High,
            },
            // "uses the last COLORFGBG field as the background"
            Case {
                env: vec![("COLORFGBG", "0;7;15")],
                theme: TerminalTheme::Light,
                source: DetectionSource::ColorFgBg,
                confidence: Confidence::High,
            },
            // "defaults to dark without terminal background hints"
            Case {
                env: vec![],
                theme: TerminalTheme::Dark,
                source: DetectionSource::Fallback,
                confidence: Confidence::Low,
            },
        ];

        for case in cases {
            let detection = detect_terminal_background_from_env(&env_of(&case.env));
            assert_eq!(detection.theme, case.theme, "env={:?}", case.env);
            assert_eq!(detection.source, case.source, "env={:?}", case.env);
            assert_eq!(detection.confidence, case.confidence, "env={:?}", case.env);
        }
    }

    #[test]
    fn classifies_rgb_colors_by_luminance() {
        assert_eq!(
            get_theme_for_rgb_color(RgbColor { r: 8, g: 8, b: 8 }),
            TerminalTheme::Dark
        );
        assert_eq!(
            get_theme_for_rgb_color(RgbColor {
                r: 250,
                g: 250,
                b: 250
            }),
            TerminalTheme::Light
        );
    }

    #[test]
    fn parses_and_resolves_automatic_theme_settings() {
        assert_eq!(
            parse_auto_theme_setting(Some("light/dark")),
            Some(AutoThemeSetting {
                light_theme: "light".to_string(),
                dark_theme: "dark".to_string(),
            })
        );
        assert_eq!(
            resolve_theme_setting(Some("dark"), TerminalTheme::Light),
            Some("dark".to_string())
        );
        assert_eq!(
            resolve_theme_setting(Some("light/dark"), TerminalTheme::Light),
            Some("light".to_string())
        );
        assert_eq!(
            resolve_theme_setting(Some("light/dark"), TerminalTheme::Dark),
            Some("dark".to_string())
        );
        assert_eq!(
            resolve_theme_setting(Some("light/dark/extra"), TerminalTheme::Dark),
            None
        );
    }

    // --- extra coverage for the pure color-math layer -----------------------

    #[test]
    fn ansi256_to_hex_covers_all_three_ranges() {
        assert_eq!(ansi256_to_hex(0), "#000000"); // basic
        assert_eq!(ansi256_to_hex(15), "#ffffff"); // basic
        assert_eq!(ansi256_to_hex(24), "#005f87"); // 6x6x6 cube
        assert_eq!(ansi256_to_hex(232), "#080808"); // grayscale ramp start
        assert_eq!(ansi256_to_hex(255), "#eeeeee"); // grayscale ramp end
    }

    #[test]
    fn resolve_var_refs_detects_cycles_and_missing_names() {
        let mut vars: IndexMap<String, ColorValue> = IndexMap::new();
        vars.insert("a".to_string(), ColorValue::Hex("b".to_string()));
        vars.insert("b".to_string(), ColorValue::Hex("a".to_string()));
        assert_eq!(
            resolve_var_refs(&ColorValue::Hex("a".to_string()), &vars),
            Err(ThemeError::CircularReference("a".to_string()))
        );

        let empty: IndexMap<String, ColorValue> = IndexMap::new();
        assert_eq!(
            resolve_var_refs(&ColorValue::Hex("missing".to_string()), &empty),
            Err(ThemeError::VariableNotFound("missing".to_string()))
        );
    }

    #[test]
    fn get_resolved_theme_colors_applies_empty_and_ansi_and_order() {
        let mut theme = base_dark_theme();
        theme.colors.insert("text".to_string(), ColorValue::Empty);
        theme
            .colors
            .insert("accent".to_string(), ColorValue::Ansi256(24));

        let dark = get_resolved_theme_colors(&theme, false).expect("resolves");
        let light = get_resolved_theme_colors(&theme, true).expect("resolves");
        let dark_map: IndexMap<_, _> = dark.iter().cloned().collect();
        let light_map: IndexMap<_, _> = light.iter().cloned().collect();

        // Empty -> light/dark default text.
        assert_eq!(dark_map["text"], DEFAULT_DARK_TEXT);
        assert_eq!(light_map["text"], DEFAULT_LIGHT_TEXT);
        // Ansi256 -> hex.
        assert_eq!(dark_map["accent"], "#005f87");
        // Order preserved: first color key is `accent` (as in the base theme).
        assert_eq!(dark.first().map(|(k, _)| k.as_str()), Some("accent"));
        // thinkingMax fallback present.
        assert!(dark_map.contains_key("thinkingMax"));
    }

    #[test]
    fn with_theme_color_fallbacks_defaults_thinking_max() {
        let mut colors: IndexMap<String, ColorValue> = IndexMap::new();
        colors.insert(
            "thinkingXhigh".to_string(),
            ColorValue::Hex("#d183e8".to_string()),
        );
        let result = with_theme_color_fallbacks(&colors);
        assert_eq!(
            result.get("thinkingMax"),
            Some(&ColorValue::Hex("#d183e8".to_string()))
        );
    }

    #[test]
    fn is_light_theme_checks_name() {
        assert!(is_light_theme("light"));
        assert!(!is_light_theme("dark"));
        assert!(!is_light_theme("custom"));
    }

    #[test]
    fn load_theme_json_resolves_builtins_without_fs() {
        let dirs = ThemeDirs::default();
        assert_eq!(load_theme_json("dark", &dirs).expect("dark").name, "dark");
        assert_eq!(
            load_theme_json("light", &dirs).expect("light").name,
            "light"
        );
        assert_eq!(
            load_theme_json("nope", &dirs).unwrap_err(),
            ThemeError::ThemeNotFound("nope".to_string())
        );
    }
}
