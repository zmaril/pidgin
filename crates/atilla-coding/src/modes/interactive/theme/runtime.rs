//! Runtime theme loader, ported from pi's
//! `modes/interactive/theme/theme.ts` (the ANSI-baked runtime layer).
//!
//! This is the counterpart to the pure data layer in the parent [`super`]
//! module: it takes a parsed [`ThemeJson`] and produces a live [`Theme`] whose
//! every color slot is pre-rendered into an ANSI escape string for a fixed
//! [`ColorMode`]. The entry point [`load_theme_from_path`] is what the
//! resource-loader `reload()` calls for each discovered theme file.
//!
//! Split out of `theme.rs` purely to keep each file within the repo's file-size
//! budget; the two form pi's single `theme.ts` mirror.

use std::collections::HashMap;
use std::path::Path;

use indexmap::IndexMap;

use super::{
    hex_to_rgb, resolve_var_refs, with_theme_color_fallbacks, ColorValue, RgbColor, ThemeError,
    ThemeJson,
};
use crate::core::source_info::SourceInfo;

/// Reject theme names containing the reserved `/` separator (used only for
/// automatic `light/dark` settings). Mirrors pi's `assertThemeNameIsValid`.
fn assert_theme_name_is_valid(name: &str) -> Result<(), ThemeError> {
    if name.contains('/') {
        return Err(ThemeError::InvalidThemeName(name.to_string()));
    }
    Ok(())
}

/// Parse a theme from JSON text, tagging any parse failure with `label` and
/// enforcing the theme-name charset rule. Mirrors pi's `parseThemeJsonContent`
/// (+ `parseThemeJson`): serde performs the JSON parse and schema validation in
/// one step (a labeled `Failed to parse theme {label}` error), then the parsed
/// name is charset-checked via [`assert_theme_name_is_valid`] — the check the
/// serde-only [`parse_theme_json`] does not perform.
pub fn parse_theme_json_content(label: &str, content: &str) -> Result<ThemeJson, ThemeError> {
    let theme: ThemeJson = serde_json::from_str(content)
        .map_err(|e| ThemeError::Parse(format!("Failed to parse theme {label}: {e}")))?;
    assert_theme_name_is_valid(&theme.name)?;
    Ok(theme)
}

// ============================================================================
// Runtime theme (ANSI-baked)
// ============================================================================

/// The color depth a [`Theme`] bakes its ANSI escapes for. Mirrors pi's
/// `type ColorMode = "truecolor" | "256color"`. This is **distinct** from
/// [`TerminalTheme`] (dark/light): `ColorMode` is about *depth*, not brightness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit "truecolor" (`38;2;r;g;b`).
    Truecolor,
    /// 256-color palette (`38;5;n`).
    Color256,
}

/// One of the seven thinking-effort levels, selecting a border color. Mirrors
/// pi's `ThinkingLevel` union (`off | minimal | low | medium | high | xhigh |
/// max`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// The 6×6×6 color-cube channel values (indices 0-5). Mirrors pi's `CUBE_VALUES`.
const CUBE_VALUES: [i32; 6] = [0, 95, 135, 175, 215, 255];

/// The 24-step grayscale ramp values (8, 18, …, 238). Mirrors pi's `GRAY_VALUES`.
fn gray_values() -> [i32; 24] {
    let mut ramp = [0i32; 24];
    for (i, slot) in ramp.iter_mut().enumerate() {
        *slot = 8 + (i as i32) * 10;
    }
    ramp
}

/// Index of the closest 6×6×6 cube channel value. Mirrors `findClosestCubeIndex`.
fn find_closest_cube_index(value: i32) -> usize {
    let mut min_dist = i32::MAX;
    let mut min_idx = 0;
    for (i, cube) in CUBE_VALUES.iter().enumerate() {
        let dist = (value - cube).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

/// Index of the closest grayscale ramp value. Mirrors `findClosestGrayIndex`.
fn find_closest_gray_index(gray: i32) -> usize {
    let ramp = gray_values();
    let mut min_dist = i32::MAX;
    let mut min_idx = 0;
    for (i, value) in ramp.iter().enumerate() {
        let dist = (gray - value).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

/// Weighted Euclidean color distance (green-biased, matching the human eye).
/// Mirrors pi's `colorDistance`.
fn color_distance(r1: i32, g1: i32, b1: i32, r2: i32, g2: i32, b2: i32) -> f64 {
    let dr = f64::from(r1 - r2);
    let dg = f64::from(g1 - g2);
    let db = f64::from(b1 - b2);
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

/// Quantize a 24-bit RGB color to the closest 256-color palette index, preferring
/// the color cube unless the color is nearly neutral and the grayscale ramp is a
/// closer match. Mirrors pi's `rgbTo256`.
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));

    // Closest color in the 6×6×6 cube.
    let r_idx = find_closest_cube_index(r);
    let g_idx = find_closest_cube_index(g);
    let b_idx = find_closest_cube_index(b);
    let cube_r = CUBE_VALUES[r_idx];
    let cube_g = CUBE_VALUES[g_idx];
    let cube_b = CUBE_VALUES[b_idx];
    let cube_index = 16 + 36 * r_idx + 6 * g_idx + b_idx;
    let cube_dist = color_distance(r, g, b, cube_r, cube_g, cube_b);

    // Closest grayscale ramp value.
    let gray = (0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)).round() as i32;
    let gray_idx = find_closest_gray_index(gray);
    let gray_value = gray_values()[gray_idx];
    let gray_index = 232 + gray_idx;
    let gray_dist = color_distance(r, g, b, gray_value, gray_value, gray_value);

    // Only prefer grayscale for near-neutral colors where it is actually closer.
    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    let spread = max_c - min_c;
    if spread < 10 && gray_dist < cube_dist {
        return gray_index as u8;
    }
    cube_index as u8
}

/// Parse a `#rrggbb` hex string into its channels, erroring on any malformed
/// input. Mirrors pi's `hexToRgb` (which *throws* on a bad hex) — unlike the
/// infallible internal [`hex_to_rgb`] used for well-formed detection input, the
/// runtime loader sees user-authored themes and must surface a [`ThemeError`]
/// rather than silently coercing bad channels to zero.
fn hex_to_rgb_checked(hex: &str) -> Result<RgbColor, ThemeError> {
    let cleaned = hex.replace('#', "");
    if cleaned.len() != 6 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ThemeError::InvalidHexColor(hex.to_string()));
    }
    Ok(hex_to_rgb(hex))
}

/// Convert a `#rrggbb` hex string to a 256-color index. Mirrors pi's `hexTo256`.
pub fn hex_to_256(hex: &str) -> Result<u8, ThemeError> {
    let rgb = hex_to_rgb_checked(hex)?;
    Ok(rgb_to_256(rgb.r, rgb.g, rgb.b))
}

/// Bake a resolved color value into a foreground ANSI escape for the given
/// [`ColorMode`]. Mirrors pi's `fgAnsi`:
/// - [`ColorValue::Empty`] → `\x1b[39m` (default foreground)
/// - [`ColorValue::Ansi256`] → `\x1b[38;5;{n}m`
/// - [`ColorValue::Hex`] under [`ColorMode::Truecolor`] → `\x1b[38;2;{r};{g};{b}m`
/// - [`ColorValue::Hex`] under [`ColorMode::Color256`] → `\x1b[38;5;{index}m`
///
/// A [`ColorValue::Hex`] that is not a `#` literal (an unresolved variable
/// reference — unreachable after [`resolve_var_refs`]) yields
/// [`ThemeError::InvalidColorValue`], faithfully mirroring pi's final `throw`.
pub fn fg_ansi(color: &ColorValue, mode: ColorMode) -> Result<String, ThemeError> {
    match color {
        ColorValue::Empty => Ok("\x1b[39m".to_string()),
        ColorValue::Ansi256(n) => Ok(format!("\x1b[38;5;{n}m")),
        ColorValue::Hex(hex) if hex.starts_with('#') => match mode {
            ColorMode::Truecolor => {
                let rgb = hex_to_rgb_checked(hex)?;
                Ok(format!("\x1b[38;2;{};{};{}m", rgb.r, rgb.g, rgb.b))
            }
            ColorMode::Color256 => Ok(format!("\x1b[38;5;{}m", hex_to_256(hex)?)),
        },
        ColorValue::Hex(other) => Err(ThemeError::InvalidColorValue(other.clone())),
    }
}

/// Bake a resolved color value into a background ANSI escape for the given
/// [`ColorMode`]. Mirrors pi's `bgAnsi` (the `49m` / `48;5` / `48;2` counterpart
/// of [`fg_ansi`]).
pub fn bg_ansi(color: &ColorValue, mode: ColorMode) -> Result<String, ThemeError> {
    match color {
        ColorValue::Empty => Ok("\x1b[49m".to_string()),
        ColorValue::Ansi256(n) => Ok(format!("\x1b[48;5;{n}m")),
        ColorValue::Hex(hex) if hex.starts_with('#') => match mode {
            ColorMode::Truecolor => {
                let rgb = hex_to_rgb_checked(hex)?;
                Ok(format!("\x1b[48;2;{};{};{}m", rgb.r, rgb.g, rgb.b))
            }
            ColorMode::Color256 => Ok(format!("\x1b[48;5;{}m", hex_to_256(hex)?)),
        },
        ColorValue::Hex(other) => Err(ThemeError::InvalidColorValue(other.clone())),
    }
}

/// The six background color keys that [`create_theme`] routes into a theme's
/// `bg_colors` map (everything else becomes a foreground color). Mirrors pi's
/// `bgColorKeys` set.
const BG_COLOR_KEYS: [&str; 6] = [
    "selectedBg",
    "userMessageBg",
    "customMessageBg",
    "toolPendingBg",
    "toolSuccessBg",
    "toolErrorBg",
];

/// A live, ANSI-baked theme. Mirrors pi's runtime `Theme` class: each color slot
/// is pre-rendered into an ANSI escape string for a fixed [`ColorMode`], so
/// styling text at runtime is a map lookup plus a string concat.
///
/// The `fg_colors`/`bg_colors` maps are keyed by pi's string color tokens
/// (`ThemeColor` / `ThemeBg` unions upstream); this port keeps the crate's
/// existing string-key convention (see [`ThemeJson::colors`]) rather than
/// introducing 46- and 6-variant enums.
#[derive(Clone, Debug)]
pub struct Theme {
    /// The theme's declared name, if it came from a named/parsed source.
    pub name: Option<String>,
    /// The filesystem path the theme was loaded from, if any.
    pub source_path: Option<String>,
    /// Optional provenance metadata (populated by callers that resolve sources).
    pub source_info: Option<SourceInfo>,
    fg_colors: HashMap<String, String>,
    bg_colors: HashMap<String, String>,
    mode: ColorMode,
}

impl Theme {
    /// Build a theme from resolved (reference-free) fg/bg color maps, baking each
    /// value into an ANSI escape for `mode`. Re-applies the
    /// `thinkingMax ?? thinkingXhigh` fallback on the foreground map
    /// (belt-and-braces — [`create_theme`] already ran
    /// [`with_theme_color_fallbacks`]), matching pi's constructor.
    fn new(
        fg_colors: &IndexMap<String, ColorValue>,
        bg_colors: &IndexMap<String, ColorValue>,
        mode: ColorMode,
        name: Option<String>,
        source_path: Option<String>,
    ) -> Result<Self, ThemeError> {
        let mut fg = fg_colors.clone();
        if !fg.contains_key("thinkingMax") {
            if let Some(xhigh) = fg.get("thinkingXhigh").cloned() {
                fg.insert("thinkingMax".to_string(), xhigh);
            }
        }

        let mut fg_baked = HashMap::with_capacity(fg.len());
        for (key, value) in &fg {
            fg_baked.insert(key.clone(), fg_ansi(value, mode)?);
        }
        let mut bg_baked = HashMap::with_capacity(bg_colors.len());
        for (key, value) in bg_colors {
            bg_baked.insert(key.clone(), bg_ansi(value, mode)?);
        }

        Ok(Theme {
            name,
            source_path,
            source_info: None,
            fg_colors: fg_baked,
            bg_colors: bg_baked,
            mode,
        })
    }

    /// Wrap `text` in the foreground color for `color`, resetting only the
    /// foreground afterwards (`\x1b[39m`). Mirrors pi's `Theme.fg`.
    pub fn fg(&self, color: &str, text: &str) -> Result<String, ThemeError> {
        let ansi = self
            .fg_colors
            .get(color)
            .ok_or_else(|| ThemeError::UnknownThemeColor(color.to_string()))?;
        Ok(format!("{ansi}{text}\x1b[39m"))
    }

    /// Wrap `text` in the background color for `color`, resetting only the
    /// background afterwards (`\x1b[49m`). Mirrors pi's `Theme.bg`.
    pub fn bg(&self, color: &str, text: &str) -> Result<String, ThemeError> {
        let ansi = self
            .bg_colors
            .get(color)
            .ok_or_else(|| ThemeError::UnknownThemeBg(color.to_string()))?;
        Ok(format!("{ansi}{text}\x1b[49m"))
    }

    /// Emit `text` bold. Mirrors pi's `chalk.bold` (SGR 1 / reset 22).
    pub fn bold(&self, text: &str) -> String {
        format!("\x1b[1m{text}\x1b[22m")
    }

    /// Emit `text` italic. Mirrors pi's `chalk.italic` (SGR 3 / reset 23).
    pub fn italic(&self, text: &str) -> String {
        format!("\x1b[3m{text}\x1b[23m")
    }

    /// Emit `text` underlined. Mirrors pi's `chalk.underline` (SGR 4 / reset 24).
    pub fn underline(&self, text: &str) -> String {
        format!("\x1b[4m{text}\x1b[24m")
    }

    /// Emit `text` with foreground/background inverted. Mirrors pi's
    /// `chalk.inverse` (SGR 7 / reset 27).
    pub fn inverse(&self, text: &str) -> String {
        format!("\x1b[7m{text}\x1b[27m")
    }

    /// Emit `text` struck through. Mirrors pi's `chalk.strikethrough`
    /// (SGR 9 / reset 29).
    pub fn strikethrough(&self, text: &str) -> String {
        format!("\x1b[9m{text}\x1b[29m")
    }

    /// The pre-baked foreground ANSI escape for `color`. Mirrors pi's `getFgAnsi`.
    pub fn get_fg_ansi(&self, color: &str) -> Result<String, ThemeError> {
        self.fg_colors
            .get(color)
            .cloned()
            .ok_or_else(|| ThemeError::UnknownThemeColor(color.to_string()))
    }

    /// The pre-baked background ANSI escape for `color`. Mirrors pi's `getBgAnsi`.
    pub fn get_bg_ansi(&self, color: &str) -> Result<String, ThemeError> {
        self.bg_colors
            .get(color)
            .cloned()
            .ok_or_else(|| ThemeError::UnknownThemeBg(color.to_string()))
    }

    /// The [`ColorMode`] this theme was baked for. Mirrors pi's `getColorMode`.
    pub fn get_color_mode(&self) -> ColorMode {
        self.mode
    }

    /// Wrap `text` in the border color for a thinking `level`, mapping each of the
    /// seven levels to its dedicated `thinking*` foreground color. Mirrors pi's
    /// `getThinkingBorderColor` (whose returned closure is applied here directly).
    pub fn get_thinking_border_color(
        &self,
        level: ThinkingLevel,
        text: &str,
    ) -> Result<String, ThemeError> {
        let color = match level {
            ThinkingLevel::Off => "thinkingOff",
            ThinkingLevel::Minimal => "thinkingMinimal",
            ThinkingLevel::Low => "thinkingLow",
            ThinkingLevel::Medium => "thinkingMedium",
            ThinkingLevel::High => "thinkingHigh",
            ThinkingLevel::Xhigh => "thinkingXhigh",
            ThinkingLevel::Max => "thinkingMax",
        };
        self.fg(color, text)
    }

    /// Wrap `text` in the bash-mode border color. Mirrors pi's
    /// `getBashModeBorderColor`.
    pub fn get_bash_mode_border_color(&self, text: &str) -> Result<String, ThemeError> {
        self.fg("bashMode", text)
    }
}

/// Build a runtime [`Theme`] from parsed theme JSON. Resolves every color through
/// `vars` (after the `thinkingMax` fallback), splits the resolved map into
/// foreground vs. background by [`BG_COLOR_KEYS`], and bakes each value into an
/// ANSI escape for `mode`. Mirrors pi's `createTheme`.
///
/// **Divergence from pi:** pi defaults `mode` to `getCapabilities().trueColor`;
/// this crate has no terminal-capability probe, so an absent `mode` defaults to
/// [`ColorMode::Color256`] (the safe lowest-common-denominator). Callers that can
/// detect truecolor should pass [`ColorMode::Truecolor`] explicitly.
pub fn create_theme(
    theme_json: &ThemeJson,
    mode: Option<ColorMode>,
    source_path: Option<String>,
) -> Result<Theme, ThemeError> {
    let color_mode = mode.unwrap_or(ColorMode::Color256);
    let colors = with_theme_color_fallbacks(&theme_json.colors);

    let mut fg_colors: IndexMap<String, ColorValue> = IndexMap::new();
    let mut bg_colors: IndexMap<String, ColorValue> = IndexMap::new();
    for (key, value) in &colors {
        let resolved = resolve_var_refs(value, &theme_json.vars)?;
        if BG_COLOR_KEYS.contains(&key.as_str()) {
            bg_colors.insert(key.clone(), resolved);
        } else {
            fg_colors.insert(key.clone(), resolved);
        }
    }

    Theme::new(
        &fg_colors,
        &bg_colors,
        color_mode,
        Some(theme_json.name.clone()),
        source_path,
    )
}

/// Load a runtime [`Theme`] from a theme file on disk: read the file, parse it
/// with [`parse_theme_json_content`] (labeled by the path, name-charset checked),
/// then bake it via [`create_theme`]. Mirrors pi's `loadThemeFromPath` — the
/// entry point the resource-loader `reload()` calls per discovered theme file.
pub fn load_theme_from_path(
    theme_path: &Path,
    mode: Option<ColorMode>,
) -> Result<Theme, ThemeError> {
    let label = theme_path.display().to_string();
    let content = std::fs::read_to_string(theme_path).map_err(|e| ThemeError::Io(e.to_string()))?;
    let theme_json = parse_theme_json_content(&label, &content)?;
    create_theme(&theme_json, mode, Some(label))
}

#[cfg(test)]
mod tests {
    use super::super::{parse_theme_json, DARK_THEME_JSON};
    use super::*;

    /// Parse the embedded `dark` theme as a self-contained base for building
    /// runtime themes in tests, mirroring the parent module's test helper.
    fn base_dark_theme() -> ThemeJson {
        parse_theme_json(DARK_THEME_JSON).expect("embedded dark theme parses")
    }

    // --- runtime loader: ANSI baking ----------------------------------------

    #[test]
    fn fg_ansi_covers_all_value_shapes() {
        // Empty -> default foreground (39m); mode is irrelevant.
        assert_eq!(
            fg_ansi(&ColorValue::Empty, ColorMode::Truecolor).unwrap(),
            "\x1b[39m"
        );
        // 256-index -> 38;5;n, verbatim, in either mode.
        assert_eq!(
            fg_ansi(&ColorValue::Ansi256(24), ColorMode::Color256).unwrap(),
            "\x1b[38;5;24m"
        );
        // Hex under truecolor -> 38;2;r;g;b.
        assert_eq!(
            fg_ansi(
                &ColorValue::Hex("#ff8800".to_string()),
                ColorMode::Truecolor
            )
            .unwrap(),
            "\x1b[38;2;255;136;0m"
        );
        // Hex under 256 -> 38;5;{hex_to_256}; #005f87 quantizes to cube index 24.
        assert_eq!(
            fg_ansi(&ColorValue::Hex("#005f87".to_string()), ColorMode::Color256).unwrap(),
            "\x1b[38;5;24m"
        );
    }

    #[test]
    fn bg_ansi_covers_all_value_shapes() {
        assert_eq!(
            bg_ansi(&ColorValue::Empty, ColorMode::Color256).unwrap(),
            "\x1b[49m"
        );
        assert_eq!(
            bg_ansi(&ColorValue::Ansi256(24), ColorMode::Truecolor).unwrap(),
            "\x1b[48;5;24m"
        );
        assert_eq!(
            bg_ansi(
                &ColorValue::Hex("#ff8800".to_string()),
                ColorMode::Truecolor
            )
            .unwrap(),
            "\x1b[48;2;255;136;0m"
        );
        assert_eq!(
            bg_ansi(&ColorValue::Hex("#005f87".to_string()), ColorMode::Color256).unwrap(),
            "\x1b[48;5;24m"
        );
    }

    #[test]
    fn ansi_helpers_reject_bad_values_without_panicking() {
        // Malformed hex -> Err, never a panic.
        assert_eq!(
            fg_ansi(
                &ColorValue::Hex("#zzzzzz".to_string()),
                ColorMode::Truecolor
            ),
            Err(ThemeError::InvalidHexColor("#zzzzzz".to_string()))
        );
        // Wrong length hex -> Err.
        assert_eq!(
            hex_to_256("#fff"),
            Err(ThemeError::InvalidHexColor("#fff".to_string()))
        );
        // A non-# string (an unresolved reference) -> Invalid color value.
        assert_eq!(
            bg_ansi(&ColorValue::Hex("accent".to_string()), ColorMode::Color256),
            Err(ThemeError::InvalidColorValue("accent".to_string()))
        );
    }

    #[test]
    fn rgb_to_256_prefers_cube_and_grayscale_correctly() {
        // Exact cube corner: pure red -> 16 + 36*5 = 196.
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        // #005f87 (0,95,135) is an exact cube point at index 24.
        assert_eq!(rgb_to_256(0, 95, 135), 24);
        // Near-neutral dark gray prefers the grayscale ramp (index 232).
        assert_eq!(rgb_to_256(10, 10, 10), 232);
    }

    // --- runtime loader: create_theme / load_theme_from_path ----------------

    #[test]
    fn create_theme_splits_fg_and_bg_by_key() {
        let theme = create_theme(&base_dark_theme(), Some(ColorMode::Truecolor), None)
            .expect("create_theme");

        assert_eq!(theme.get_color_mode(), ColorMode::Truecolor);
        assert_eq!(theme.name.as_deref(), Some("dark"));

        // A background key lives only in the bg map.
        assert!(theme.get_bg_ansi("selectedBg").is_ok());
        assert_eq!(
            theme.get_fg_ansi("selectedBg"),
            Err(ThemeError::UnknownThemeColor("selectedBg".to_string()))
        );
        // A foreground key lives only in the fg map.
        assert!(theme.get_fg_ansi("accent").is_ok());
        assert_eq!(
            theme.get_bg_ansi("accent"),
            Err(ThemeError::UnknownThemeBg("accent".to_string()))
        );
        // thinkingMax is baked (present in dark.json, or the fallback fills it).
        assert!(theme.get_fg_ansi("thinkingMax").is_ok());
    }

    #[test]
    fn theme_style_passthroughs_emit_sgr_codes() {
        let theme = create_theme(&base_dark_theme(), Some(ColorMode::Color256), None)
            .expect("create_theme");
        assert_eq!(theme.bold("x"), "\x1b[1mx\x1b[22m");
        assert_eq!(theme.italic("x"), "\x1b[3mx\x1b[23m");
        assert_eq!(theme.underline("x"), "\x1b[4mx\x1b[24m");
        assert_eq!(theme.inverse("x"), "\x1b[7mx\x1b[27m");
        assert_eq!(theme.strikethrough("x"), "\x1b[9mx\x1b[29m");
        // fg wraps with a 39m foreground reset.
        let wrapped = theme.fg("accent", "hi").expect("fg accent");
        assert!(wrapped.ends_with("hi\x1b[39m"), "got {wrapped:?}");
    }

    #[test]
    fn create_theme_surfaces_invalid_hex_as_err_not_panic() {
        let mut theme = base_dark_theme();
        theme
            .colors
            .insert("accent".to_string(), ColorValue::Hex("#zzzzzz".to_string()));
        // 256color path routes through hex_to_256 -> Err; must not panic.
        assert_eq!(
            create_theme(&theme, Some(ColorMode::Color256), None).unwrap_err(),
            ThemeError::InvalidHexColor("#zzzzzz".to_string())
        );
    }

    #[test]
    fn parse_theme_json_content_rejects_slash_names() {
        let content = serde_json::to_string(&serde_json::json!({
            "name": "light/dark",
            "vars": {},
            "colors": {},
        }))
        .unwrap();
        // A "/" in the name is rejected (charset check pi's serde-only path lacked).
        assert!(matches!(
            parse_theme_json_content("label", &content),
            Err(ThemeError::InvalidThemeName(name)) if name == "light/dark"
        ));
    }

    #[test]
    fn load_theme_from_path_round_trips_via_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("round-trip.json");
        std::fs::write(&path, DARK_THEME_JSON).expect("write theme");

        let theme = load_theme_from_path(&path, Some(ColorMode::Truecolor)).expect("load theme");
        assert_eq!(theme.name.as_deref(), Some("dark"));
        assert_eq!(
            theme.source_path.as_deref(),
            Some(path.display().to_string().as_str())
        );
        assert_eq!(theme.get_color_mode(), ColorMode::Truecolor);
        assert!(theme.get_fg_ansi("accent").is_ok());
    }

    /// Ported from pi's `max-thinking.test.ts` ("falls back to thinkingXhigh for
    /// legacy themes"): a theme missing `thinkingMax` must render the `max`
    /// thinking border identically to `xhigh`.
    #[test]
    fn legacy_theme_falls_back_to_thinking_xhigh_for_max() {
        let mut theme = base_dark_theme();
        theme.name = "legacy-theme".to_string();
        theme.colors.shift_remove("thinkingMax");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy-theme.json");
        std::fs::write(&path, serde_json::to_vec(&raw_theme_value(&theme)).unwrap())
            .expect("write theme");

        let loaded = load_theme_from_path(&path, None).expect("load legacy theme");
        let max = loaded
            .get_thinking_border_color(ThinkingLevel::Max, "border")
            .expect("max border");
        let xhigh = loaded
            .get_thinking_border_color(ThinkingLevel::Xhigh, "border")
            .expect("xhigh border");
        assert_eq!(max, xhigh);
    }

    /// Serialize a [`ThemeJson`] back to the JSON shape `load_theme_from_path`
    /// parses (colors/vars as `#hex` strings or integers), so a mutated in-memory
    /// theme can be written to a temp file for a full load round-trip.
    fn raw_theme_value(theme: &ThemeJson) -> serde_json::Value {
        let dump = |map: &IndexMap<String, ColorValue>| -> serde_json::Value {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                let value = match v {
                    ColorValue::Hex(s) => serde_json::Value::String(s.clone()),
                    ColorValue::Empty => serde_json::Value::String(String::new()),
                    ColorValue::Ansi256(n) => serde_json::Value::from(*n),
                };
                obj.insert(k.clone(), value);
            }
            serde_json::Value::Object(obj)
        };
        serde_json::json!({
            "name": theme.name,
            "vars": dump(&theme.vars),
            "colors": dump(&theme.colors),
        })
    }
}
