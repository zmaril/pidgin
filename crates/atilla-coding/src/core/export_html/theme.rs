// straitjacket-allow-file:color — theme color math; hardcoded fallback colors are the domain of this module.
//! Pure theme color math for the HTML export.
//!
//! Ported from the color helpers in pi's `core/export-html/index.ts`
//! (`parseColor`, `getLuminance`, `adjustBrightness`, `deriveExportColors`,
//! `generateThemeVars`).
//!
//! pi's `generateThemeVars` imports `getResolvedThemeColors` /
//! `getThemeExportColors` from the interactive theme module. That module is owned
//! by a sibling crate and is not yet on main, so this port is decoupled: the
//! resolved theme colors and the optional export-color overrides are passed in as
//! [`ThemeInputs`] rather than resolved here. When the shared theme crate lands, a
//! thin adapter can build [`ThemeInputs`] from it.

/// A parsed RGB color. Components are stored as signed integers to mirror
/// JavaScript's numeric handling of `rgb(r, g, b)` inputs whose channels may
/// exceed the 0-255 range.
struct Rgb {
    r: i64,
    g: i64,
    b: i64,
}

/// Export background colors, either read from a theme's explicit `export` block or
/// derived from a base color.
///
/// Migrates to the shared theme crate once it lands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeExportColors {
    pub page_bg: Option<String>,
    pub card_bg: Option<String>,
    pub info_bg: Option<String>,
}

/// Inputs for theme-variable generation, decoupled from the interactive theme
/// module.
///
/// Migrates to the shared theme crate once it lands.
pub struct ThemeInputs {
    /// Resolved theme colors as ordered `(key, value)` pairs. Order is preserved
    /// so the emitted CSS custom properties match pi's `Object.entries` iteration.
    pub resolved_colors: Vec<(String, String)>,
    /// Optional explicit export-color overrides from the theme's `export` block.
    pub export_colors: ThemeExportColors,
}

/// Derived export background colors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportBackgrounds {
    pub page_bg: String,
    pub card_bg: String,
    pub info_bg: String,
}

/// Parse a color string to RGB values. Supports hex (`#RRGGBB`) and `rgb(r,g,b)`.
fn parse_color(color: &str) -> Option<Rgb> {
    parse_hex(color).or_else(|| parse_rgb_func(color))
}

/// Parse a `#RRGGBB` hex color (exactly six hex digits), mirroring the anchored
/// regex `^#([0-9a-fA-F]{2}){3}$`.
fn parse_hex(color: &str) -> Option<Rgb> {
    let bytes = color.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return None;
    }
    let hex = &color[1..];
    if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(Rgb {
        r: i64::from_str_radix(&hex[0..2], 16).ok()?,
        g: i64::from_str_radix(&hex[2..4], 16).ok()?,
        b: i64::from_str_radix(&hex[4..6], 16).ok()?,
    })
}

/// Parse an `rgb(r, g, b)` function, mirroring the anchored regex
/// `^rgb\s*\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)$`.
fn parse_rgb_func(color: &str) -> Option<Rgb> {
    let rest = color.strip_prefix("rgb")?;
    let mut chars = rest.chars().peekable();

    let skip_ws = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
    };
    let read_uint = |chars: &mut std::iter::Peekable<std::str::Chars>| -> Option<i64> {
        let mut digits = String::new();
        while let Some(c) = chars.peek() {
            if c.is_ascii_digit() {
                digits.push(*c);
                chars.next();
            } else {
                break;
            }
        }
        digits.parse::<i64>().ok()
    };

    skip_ws(&mut chars);
    if chars.next()? != '(' {
        return None;
    }
    skip_ws(&mut chars);
    let r = read_uint(&mut chars)?;
    skip_ws(&mut chars);
    if chars.next()? != ',' {
        return None;
    }
    skip_ws(&mut chars);
    let g = read_uint(&mut chars)?;
    skip_ws(&mut chars);
    if chars.next()? != ',' {
        return None;
    }
    skip_ws(&mut chars);
    let b = read_uint(&mut chars)?;
    skip_ws(&mut chars);
    if chars.next()? != ')' {
        return None;
    }
    skip_ws(&mut chars);
    if chars.next().is_some() {
        // Trailing characters: the source regex is anchored with `$`.
        return None;
    }
    Some(Rgb { r, g, b })
}

/// Calculate the relative luminance of a color (0-1, higher = lighter), using the
/// WCAG linearization.
fn get_luminance(r: i64, g: i64, b: i64) -> f64 {
    let to_linear = |c: i64| {
        let s = c as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * to_linear(r) + 0.7152 * to_linear(g) + 0.0722 * to_linear(b)
}

/// Adjust color brightness. Factor > 1 lightens, < 1 darkens. Unparseable colors
/// are returned unchanged.
fn adjust_brightness(color: &str, factor: f64) -> String {
    let parsed = match parse_color(color) {
        Some(p) => p,
        None => return color.to_string(),
    };
    let adjust = |c: i64| -> i64 { (c as f64 * factor).round().clamp(0.0, 255.0) as i64 };
    format!(
        "rgb({}, {}, {})",
        adjust(parsed.r),
        adjust(parsed.g),
        adjust(parsed.b)
    )
}

/// Derive export background colors from a base color (e.g. `userMessageBg`).
fn derive_export_colors(base_color: &str) -> ExportBackgrounds {
    let parsed = match parse_color(base_color) {
        Some(p) => p,
        None => {
            return ExportBackgrounds {
                page_bg: "rgb(24, 24, 30)".to_string(),
                card_bg: "rgb(30, 30, 36)".to_string(),
                info_bg: "rgb(60, 55, 40)".to_string(),
            };
        }
    };

    let luminance = get_luminance(parsed.r, parsed.g, parsed.b);
    let is_light = luminance > 0.5;

    if is_light {
        ExportBackgrounds {
            page_bg: adjust_brightness(base_color, 0.96),
            card_bg: base_color.to_string(),
            info_bg: format!(
                "rgb({}, {}, {})",
                (parsed.r + 10).min(255),
                (parsed.g + 5).min(255),
                (parsed.b - 20).max(0)
            ),
        }
    } else {
        ExportBackgrounds {
            page_bg: adjust_brightness(base_color, 0.7),
            card_bg: adjust_brightness(base_color, 0.85),
            info_bg: format!(
                "rgb({}, {}, {})",
                (parsed.r + 20).min(255),
                (parsed.g + 15).min(255),
                parsed.b
            ),
        }
    }
}

/// Look up `userMessageBg` from the resolved colors, falling back to the default
/// used by pi when the value is missing or empty (`colors.userMessageBg || ...`).
fn user_message_bg(colors: &[(String, String)]) -> String {
    colors
        .iter()
        .find(|(k, _)| k == "userMessageBg")
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("#343541")
        .to_string()
}

/// Resolve the three export background colors, preferring the theme's explicit
/// export overrides and falling back to colors derived from `userMessageBg`.
///
/// Mirrors the `themeExport.pageBg ?? derivedExportColors.pageBg` chain in pi's
/// `generateHtml`.
pub fn resolve_export_backgrounds(inputs: &ThemeInputs) -> ExportBackgrounds {
    let derived = derive_export_colors(&user_message_bg(&inputs.resolved_colors));
    ExportBackgrounds {
        page_bg: inputs
            .export_colors
            .page_bg
            .clone()
            .unwrap_or(derived.page_bg),
        card_bg: inputs
            .export_colors
            .card_bg
            .clone()
            .unwrap_or(derived.card_bg),
        info_bg: inputs
            .export_colors
            .info_bg
            .clone()
            .unwrap_or(derived.info_bg),
    }
}

/// Generate CSS custom property declarations from theme colors.
///
/// Emits one `--key: value;` line per resolved color followed by the three
/// `--export*Bg` properties, joined the same way pi joins them (newline plus six
/// spaces of indentation, so they nest under the template's `:root` block).
pub fn generate_theme_vars(inputs: &ThemeInputs) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (key, value) in &inputs.resolved_colors {
        lines.push(format!("--{key}: {value};"));
    }

    let backgrounds = resolve_export_backgrounds(inputs);
    lines.push(format!("--exportPageBg: {};", backgrounds.page_bg));
    lines.push(format!("--exportCardBg: {};", backgrounds.card_bg));
    lines.push(format!("--exportInfoBg: {};", backgrounds.info_bg));

    lines.join("\n      ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(color: &str) -> Rgb {
        parse_color(color).unwrap()
    }

    #[test]
    fn parses_hex_colors() {
        let c = rgb("#183042");
        assert_eq!((c.r, c.g, c.b), (0x18, 0x30, 0x42));
    }

    #[test]
    fn rejects_malformed_hex() {
        assert!(parse_color("#12345").is_none());
        assert!(parse_color("#gggggg").is_none());
        assert!(parse_color("183042").is_none());
    }

    #[test]
    fn parses_rgb_function_with_whitespace() {
        let c = rgb("rgb( 24 , 24 , 30 )");
        assert_eq!((c.r, c.g, c.b), (24, 24, 30));
        assert!(parse_color("rgb(1,2,3)").is_some());
        assert!(parse_color("rgb(1,2,3) trailing").is_none());
        assert!(parse_color("rgb(1,2)").is_none());
    }

    #[test]
    fn luminance_matches_wcag_endpoints() {
        assert!((get_luminance(255, 255, 255) - 1.0).abs() < 1e-9);
        assert!(get_luminance(0, 0, 0).abs() < 1e-9);
    }

    #[test]
    fn adjust_brightness_darkens_and_clamps() {
        // 0x64 = 100; 100 * 0.7 = 70.
        assert_eq!(adjust_brightness("#646464", 0.7), "rgb(70, 70, 70)");
        // Clamps above 255.
        assert_eq!(adjust_brightness("#ffffff", 2.0), "rgb(255, 255, 255)");
        // Unparseable input is returned unchanged.
        assert_eq!(adjust_brightness("not-a-color", 0.5), "not-a-color");
    }

    #[test]
    fn derive_export_colors_dark_branch() {
        // Dark base (#343541, pi's default) takes the dark branch.
        let d = derive_export_colors("#343541");
        assert_eq!(d.page_bg, "rgb(36, 37, 46)");
        assert_eq!(d.card_bg, "rgb(44, 45, 55)");
        assert_eq!(d.info_bg, "rgb(72, 68, 65)");
    }

    #[test]
    fn derive_export_colors_light_branch() {
        // Light base takes the light branch and keeps the base as the card color.
        let d = derive_export_colors("#f0f0f0");
        assert_eq!(d.card_bg, "#f0f0f0");
        assert_eq!(d.page_bg, "rgb(230, 230, 230)");
        assert_eq!(d.info_bg, "rgb(250, 245, 220)");
    }

    #[test]
    fn derive_export_colors_fallback() {
        let d = derive_export_colors("not-a-color");
        assert_eq!(d.page_bg, "rgb(24, 24, 30)");
        assert_eq!(d.card_bg, "rgb(30, 30, 36)");
        assert_eq!(d.info_bg, "rgb(60, 55, 40)");
    }

    #[test]
    fn generate_theme_vars_emits_properties_and_exports() {
        let inputs = ThemeInputs {
            resolved_colors: vec![
                ("userMessageBg".to_string(), "#343541".to_string()),
                ("foreground".to_string(), "#ffffff".to_string()),
            ],
            export_colors: ThemeExportColors::default(),
        };
        let vars = generate_theme_vars(&inputs);
        let expected = [
            "--userMessageBg: #343541;",
            "--foreground: #ffffff;",
            "--exportPageBg: rgb(36, 37, 46);",
            "--exportCardBg: rgb(44, 45, 55);",
            "--exportInfoBg: rgb(72, 68, 65);",
        ]
        .join("\n      ");
        assert_eq!(vars, expected);
    }

    #[test]
    fn explicit_export_overrides_take_precedence() {
        let inputs = ThemeInputs {
            resolved_colors: vec![("userMessageBg".to_string(), "#343541".to_string())],
            export_colors: ThemeExportColors {
                page_bg: Some("#112233".to_string()),
                card_bg: None,
                info_bg: Some("#445566".to_string()),
            },
        };
        let bg = resolve_export_backgrounds(&inputs);
        assert_eq!(bg.page_bg, "#112233");
        // card_bg falls back to the derived dark-branch value.
        assert_eq!(bg.card_bg, "rgb(44, 45, 55)");
        assert_eq!(bg.info_bg, "#445566");
    }
}
