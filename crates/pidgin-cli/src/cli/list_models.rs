//! `--list-models`: enumerate available models with optional fuzzy search.
//!
//! Hand-ported from pi's `packages/coding-agent/src/cli/list-models.ts`,
//! reproducing its exact stdout/stderr routing, ordering, column widths, and
//! message text. `console.log` maps to [`out_line`] (stdout unless taken over),
//! `console.error` to [`err_line`] (always stderr).

// straitjacket-allow-file:duplication

use pidgin_coding::core::auth::auth_guidance::format_no_models_available_message;
use pidgin_coding::core::model_runtime::ModelRuntime;
use pidgin_core::ai::{Modality, Model};
use pidgin_tui::fuzzy_filter;

use super::output_guard::{err_line, out_line};

/// Format a number as human-readable (e.g., `200000` -> `"200K"`,
/// `1000000` -> `"1M"`). Mirrors pi's `formatTokenCount` (`list-models.ts:14`),
/// including its float division and `toFixed(1)` rounding.
fn format_token_count(count: u64) -> String {
    if count >= 1_000_000 {
        let millions = count as f64 / 1_000_000.0;
        if millions.fract() == 0.0 {
            format!("{}M", millions as u64)
        } else {
            format!("{}M", to_fixed_1(millions))
        }
    } else if count >= 1_000 {
        let thousands = count as f64 / 1_000.0;
        if thousands.fract() == 0.0 {
            format!("{}K", thousands as u64)
        } else {
            format!("{}K", to_fixed_1(thousands))
        }
    } else {
        count.to_string()
    }
}

/// Render `value` with one fractional digit, matching JS `Number.toFixed(1)`
/// (round-half-away-from-zero). Rust's [`f64::round`] uses the same tie-break.
fn to_fixed_1(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    format!("{rounded:.1}")
}

/// A rendered table row (all columns already stringified).
struct Row {
    provider: String,
    model: String,
    context: String,
    max_out: String,
    thinking: String,
    images: String,
}

/// List available models, optionally filtered by `search_pattern`. Mirrors
/// pi's `listModels` (`list-models.ts:29`).
pub fn list_models(runtime: &ModelRuntime, search_pattern: Option<&str>) {
    if let Some(load_error) = runtime.get_error() {
        err_line(&format!(
            "Warning: errors loading models.json:\n{load_error}"
        ));
    }

    let models: Vec<Model> = runtime.get_available_snapshot().to_vec();

    if models.is_empty() {
        out_line(&format_no_models_available_message());
        return;
    }

    // Apply fuzzy filter if a search pattern was provided.
    let mut filtered_models: Vec<Model> = match search_pattern {
        Some(pattern) => fuzzy_filter(models, pattern, |m| format!("{} {}", m.provider, m.id)),
        None => models,
    };

    if filtered_models.is_empty() {
        // A pattern was necessarily present to reach an empty filtered set.
        let pattern = search_pattern.unwrap_or("");
        out_line(&format!("No models matching \"{pattern}\""));
        return;
    }

    // Sort by provider, then by model id.
    filtered_models.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));

    let rows: Vec<Row> = filtered_models
        .iter()
        .map(|m| Row {
            provider: m.provider.clone(),
            model: m.id.clone(),
            context: format_token_count(m.context_window),
            max_out: format_token_count(m.max_tokens),
            thinking: if m.reasoning { "yes" } else { "no" }.to_string(),
            images: if m.input.contains(&Modality::Image) {
                "yes"
            } else {
                "no"
            }
            .to_string(),
        })
        .collect();

    let headers = Row {
        provider: "provider".to_string(),
        model: "model".to_string(),
        context: "context".to_string(),
        max_out: "max-out".to_string(),
        thinking: "thinking".to_string(),
        images: "images".to_string(),
    };

    let w_provider = column_width(&headers.provider, rows.iter().map(|r| &r.provider));
    let w_model = column_width(&headers.model, rows.iter().map(|r| &r.model));
    let w_context = column_width(&headers.context, rows.iter().map(|r| &r.context));
    let w_max_out = column_width(&headers.max_out, rows.iter().map(|r| &r.max_out));
    let w_thinking = column_width(&headers.thinking, rows.iter().map(|r| &r.thinking));
    let w_images = column_width(&headers.images, rows.iter().map(|r| &r.images));

    let format_line = |r: &Row| -> String {
        [
            pad_end(&r.provider, w_provider),
            pad_end(&r.model, w_model),
            pad_end(&r.context, w_context),
            pad_end(&r.max_out, w_max_out),
            pad_end(&r.thinking, w_thinking),
            pad_end(&r.images, w_images),
        ]
        .join("  ")
    };

    out_line(&format_line(&headers));
    for row in &rows {
        out_line(&format_line(row));
    }
}

/// The `padEnd` width for a column: the max character count of the header and
/// every cell.
fn column_width<'a>(header: &str, cells: impl Iterator<Item = &'a String>) -> usize {
    cells.fold(char_len(header), |acc, cell| acc.max(char_len(cell)))
}

/// JS `String.length` for the strings this table renders (all ASCII). Uses the
/// Unicode scalar count, matching the `padEnd` measure for BMP text.
fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Right-pad `s` with spaces to `width` characters (JS `padEnd`). Never
/// truncates; strings already at/over `width` are returned unchanged.
fn pad_end(s: &str, width: usize) -> String {
    let len = char_len(s);
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_token_count_matches_pi() {
        assert_eq!(format_token_count(200_000), "200K");
        assert_eq!(format_token_count(1_000_000), "1M");
        assert_eq!(format_token_count(1_500_000), "1.5M");
        assert_eq!(format_token_count(128_000), "128K");
        assert_eq!(format_token_count(1_048_576), "1.0M");
        assert_eq!(format_token_count(32_768), "32.8K");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(0), "0");
    }

    #[test]
    fn pad_end_pads_and_never_truncates() {
        assert_eq!(pad_end("no", 6), "no    ");
        assert_eq!(pad_end("images", 6), "images");
        assert_eq!(pad_end("toolong", 3), "toolong");
    }
}
