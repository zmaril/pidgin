//! Byte-exact port of pi's interactive-mode footer chrome
//! (`modes/interactive/components/footer.ts`): the pwd/branch/session line, the
//! token-stats + context + model stats line, and the optional extension-status
//! line, fully ANSI-styled to match pi's `theme.fg` / `theme.bold` output.
//!
//! ## Relationship to `core::footer_data_provider`
//!
//! `core/footer_data_provider.rs` already carries the **unstyled** layout core
//! (`render_footer`, plus the pure [`format_tokens`] / [`format_cwd_for_footer`]
//! helpers) — an earlier port that deferred all ANSI colourisation. This module
//! is the *interactive component* layer: it reuses those two proven, pure
//! helpers verbatim (rather than re-duplicating them) and adds pi's exact ANSI
//! styling — the dim body wrapping, the error/warning context-severity bands, the
//! bold `xp` marker, and the dim truncation ellipses — so its `render` output is
//! byte-identical to pi's live `FooterComponent`, verified against vectors
//! extracted from pi itself (`interactive_footer.json`).
//!
//! ## Input seam — [`FooterData`]
//!
//! pi's `FooterComponent` reads from a live `AgentSession` + `FooterDataProvider`
//! (neither ported yet). [`FooterData`] is the value seam standing in for them:
//! it carries exactly the values pi's `render` consults, already aggregated (pi
//! sums per-message usage into the totals here, and takes the cache-hit rate from
//! the latest assistant entry — both reproduced by the caller).

// straitjacket-allow-file:duplication — faithful line-for-line mirror of pi's
// `FooterComponent.render`; the layout/padding/truncation arithmetic is
// duplicated from pi by design so it tracks the upstream source exactly.

use std::collections::BTreeMap;

use pidgin_tui::renderer::Component;
use pidgin_tui::{truncate_to_width, visible_width};

use crate::core::footer_data_provider::{format_cwd_for_footer, format_tokens};
use crate::modes::interactive::theme::Theme;

/// `sanitizeStatusText` (footer.ts:12). Flatten a status string to a single line:
/// newlines/tabs/CRs become spaces, runs of spaces collapse, ends trimmed.
fn sanitize_status_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        let ch = if matches!(ch, '\r' | '\n' | '\t') {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// The values pi's `FooterComponent.render` reads from the live session /
/// provider, already aggregated. Field-for-field, this mirrors every value
/// consulted by `footer.ts:83-245`.
#[derive(Debug, Clone)]
pub struct FooterData {
    /// Working directory (`sessionManager.getCwd()`).
    pub cwd: String,
    /// Home directory used to abbreviate `cwd` to `~` (pi reads
    /// `process.env.HOME || process.env.USERPROFILE`). Explicit so vectors are
    /// stable — this module never reads the environment.
    pub home: Option<String>,
    /// Git branch (`footerData.getGitBranch()`); `None`/empty omits the suffix.
    pub git_branch: Option<String>,
    /// Session name (`sessionManager.getSessionName()`); `None`/empty omits it.
    pub session_name: Option<String>,
    /// Summed assistant `usage.input` across all session entries.
    pub total_input: u64,
    /// Summed assistant `usage.output`.
    pub total_output: u64,
    /// Summed assistant `usage.cacheRead`.
    pub total_cache_read: u64,
    /// Summed assistant `usage.cacheWrite`.
    pub total_cache_write: u64,
    /// Cache-hit rate of the latest assistant entry, or `None` when its prompt
    /// token count was zero (pi's `latestCacheHitRate === undefined`).
    pub latest_cache_hit_rate: Option<f64>,
    /// Summed assistant `usage.cost.total`, in dollars.
    pub total_cost: f64,
    /// Whether the model is subscription-backed (pi's `usingSubscription`:
    /// `provider === "kimi-coding" || modelRuntime.isUsingOAuth(provider)`).
    pub using_subscription: bool,
    /// Context-window percent in use. `None` reproduces pi's post-compaction
    /// `null` percent, rendered as `"?"` (and contributing 0 to the colour band).
    pub context_percent: Option<f64>,
    /// Effective context window in tokens (`contextUsage?.contextWindow ??
    /// model?.contextWindow ?? 0`).
    pub context_window: u64,
    /// Whether auto-compaction is on (renders the ` (auto)` indicator).
    pub auto_compact_enabled: bool,
    /// Whether experimental features are enabled (renders the bold `xp` marker).
    pub experimental: bool,
    /// Model id (`state.model?.id`); `None` renders `"no-model"`.
    pub model_id: Option<String>,
    /// Provider name (`state.model.provider`); used only for the `(provider)`
    /// prefix when `provider_count > 1` and a model is present.
    pub provider: String,
    /// Thinking descriptor: `None` when the model has no `reasoning` support;
    /// `Some(level)` when it does, where an empty string means `"off"` (pi's
    /// `state.thinkingLevel || "off"`).
    pub thinking: Option<String>,
    /// Number of providers with available models
    /// (`footerData.getAvailableProviderCount()`).
    pub provider_count: usize,
    /// Extension statuses (`footerData.getExtensionStatuses()`), sorted by key.
    pub extension_statuses: BTreeMap<String, String>,
}

/// Minimum spaces kept between the stats cluster and the right-aligned model.
const MIN_PADDING: i64 = 2;

/// Footer component: shows pwd/branch/session, token stats, context usage, model,
/// and extension statuses. Mirrors pi's `FooterComponent`.
pub struct FooterComponent {
    theme: Theme,
    data: FooterData,
}

impl FooterComponent {
    /// Build a footer over `data`, styled with `theme`.
    pub fn new(data: FooterData, theme: Theme) -> Self {
        Self { theme, data }
    }

    /// Replace the footer's data (pi mutates the live session; here the caller
    /// re-supplies the aggregated seam value).
    pub fn set_data(&mut self, data: FooterData) {
        self.data = data;
    }

    /// `theme.fg(color, text)` — the theme colours used here are always baked into
    /// the interactive themes, so a lookup miss is a programmer error.
    fn fg(&self, color: &str, text: &str) -> String {
        self.theme
            .fg(color, text)
            .expect("footer theme colour is present")
    }
}

impl Component for FooterComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let width = width as i64;
        let d = &self.data;

        // Context usage (post-compaction unknown -> "?").
        let context_percent_value = d.context_percent.unwrap_or(0.0);
        let context_percent_is_unknown = d.context_percent.is_none();

        // pwd + branch + session name.
        let mut pwd = format_cwd_for_footer(&d.cwd, d.home.as_deref());
        if let Some(branch) = d.git_branch.as_deref().filter(|b| !b.is_empty()) {
            pwd = format!("{pwd} ({branch})");
        }
        if let Some(name) = d.session_name.as_deref().filter(|n| !n.is_empty()) {
            pwd = format!("{pwd} \u{2022} {name}");
        }

        // Stats line, left cluster.
        let mut stats_parts: Vec<String> = Vec::new();
        if d.total_input != 0 {
            stats_parts.push(format!("\u{2191}{}", format_tokens(d.total_input as i64)));
        }
        if d.total_output != 0 {
            stats_parts.push(format!("\u{2193}{}", format_tokens(d.total_output as i64)));
        }
        if d.total_cache_read != 0 {
            stats_parts.push(format!("R{}", format_tokens(d.total_cache_read as i64)));
        }
        if d.total_cache_write != 0 {
            stats_parts.push(format!("W{}", format_tokens(d.total_cache_write as i64)));
        }
        if let Some(rate) = d.latest_cache_hit_rate {
            if d.total_cache_read > 0 || d.total_cache_write > 0 {
                stats_parts.push(format!("CH{rate:.1}%"));
            }
        }
        if d.total_cost != 0.0 || d.using_subscription {
            let sub = if d.using_subscription { " (sub)" } else { "" };
            stats_parts.push(format!("${:.3}{sub}", d.total_cost));
        }

        // Context percentage, coloured by severity band.
        let auto_indicator = if d.auto_compact_enabled {
            " (auto)"
        } else {
            ""
        };
        let context_percent_display = if context_percent_is_unknown {
            format!(
                "?/{}{auto_indicator}",
                format_tokens(d.context_window as i64)
            )
        } else {
            format!(
                "{:.1}%/{}{auto_indicator}",
                context_percent_value,
                format_tokens(d.context_window as i64)
            )
        };
        let context_percent_str = if context_percent_value > 90.0 {
            self.fg("error", &context_percent_display)
        } else if context_percent_value > 70.0 {
            self.fg("warning", &context_percent_display)
        } else {
            context_percent_display
        };
        stats_parts.push(context_percent_str);
        if d.experimental {
            stats_parts.push(format!(
                "{} {}",
                self.fg("dim", "\u{2022}"),
                self.theme.bold(&self.fg("warning", "xp"))
            ));
        }

        let mut stats_left = stats_parts.join(" ");
        let mut stats_left_width = visible_width(&stats_left) as i64;
        if stats_left_width > width {
            stats_left = truncate_to_width(&stats_left, width, "...", false);
            stats_left_width = visible_width(&stats_left) as i64;
        }

        // Right side: model name (+ thinking level, + provider prefix).
        let model_name = d.model_id.clone().unwrap_or_else(|| "no-model".to_string());

        let mut right_side_without_provider = model_name.clone();
        if let Some(level) = d.thinking.as_deref() {
            let thinking_level = if level.is_empty() { "off" } else { level };
            right_side_without_provider = if thinking_level == "off" {
                format!("{model_name} \u{2022} thinking off")
            } else {
                format!("{model_name} \u{2022} {thinking_level}")
            };
        }

        let mut right_side = right_side_without_provider.clone();
        if d.provider_count > 1 && d.model_id.is_some() {
            right_side = format!("({}) {right_side_without_provider}", d.provider);
            if stats_left_width + MIN_PADDING + visible_width(&right_side) as i64 > width {
                right_side = right_side_without_provider.clone();
            }
        }

        let right_side_width = visible_width(&right_side) as i64;
        let total_needed = stats_left_width + MIN_PADDING + right_side_width;

        let stats_line = if total_needed <= width {
            let padding = " ".repeat((width - stats_left_width - right_side_width) as usize);
            format!("{stats_left}{padding}{right_side}")
        } else {
            let available_for_right = width - stats_left_width - MIN_PADDING;
            if available_for_right > 0 {
                let truncated_right =
                    truncate_to_width(&right_side, available_for_right, "", false);
                let truncated_right_width = visible_width(&truncated_right) as i64;
                let pad = (width - stats_left_width - truncated_right_width).max(0) as usize;
                format!("{stats_left}{}{truncated_right}", " ".repeat(pad))
            } else {
                stats_left.clone()
            }
        };

        // Apply dim to each part separately. `stats_left` may embed colour codes
        // (context %, xp) whose resets would clear an outer dim wrapper, so pi dims
        // the coloured section and the plain remainder independently.
        let dim_stats_left = self.fg("dim", &stats_left);
        let remainder = &stats_line[stats_left.len()..];
        let dim_remainder = self.fg("dim", remainder);

        let pwd_line =
            truncate_to_width(&self.fg("dim", &pwd), width, &self.fg("dim", "..."), false);
        let mut lines = vec![pwd_line, format!("{dim_stats_left}{dim_remainder}")];

        // Extension statuses, sorted by key, sanitised, on one truncated line.
        if !d.extension_statuses.is_empty() {
            let status_line = d
                .extension_statuses
                .values()
                .map(|text| sanitize_status_text(text))
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(truncate_to_width(
                &status_line,
                width,
                &self.fg("dim", "..."),
                false,
            ));
        }

        lines
    }
}
