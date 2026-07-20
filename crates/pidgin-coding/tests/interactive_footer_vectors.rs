// straitjacket-allow-file:duplication — the `load()`/`dark_theme()` helpers and
// the per-component replay loops intentionally mirror interactive_message_vectors.rs
// and the pidgin-tui vector-test binaries; each integration-test binary is standalone.
//! Drives the Rust port of pi's interactive footer + status chrome
//! (FooterComponent, IdleStatus, WorkingStatusIndicator) against vectors extracted
//! from pi itself (`crates/pidgin-coding/vectors/gen/generate_interactive_footer.mjs`).
//! Every assertion is byte-identical: pi's `render(width)` output is the source of
//! truth. The theme is baked at 256-color to match the generator.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use pidgin_coding::modes::interactive::components::{
    FooterComponent, FooterData, IdleStatus, WorkingStatusIndicator,
};
use pidgin_coding::modes::interactive::theme::{create_theme, parse_theme_json, ColorMode, Theme};
use pidgin_tui::renderer::Component;

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Build the runtime `dark` theme baked at 256-color — the same theme the
/// generator loads (the JSON is byte-identical to pi's).
fn dark_theme() -> Theme {
    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("modes")
        .join("interactive")
        .join("theme")
        .join("dark.json");
    let content = std::fs::read_to_string(&json_path).expect("read dark.json");
    let theme_json = parse_theme_json(&content).expect("parse dark.json");
    create_theme(&theme_json, Some(ColorMode::Color256), None).expect("create dark theme")
}

// --- FooterComponent --------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FooterVec {
    label: String,
    cwd: String,
    home: Option<String>,
    git_branch: Option<String>,
    session_name: Option<String>,
    total_input: u64,
    total_output: u64,
    total_cache_read: u64,
    total_cache_write: u64,
    latest_cache_hit_rate: Option<f64>,
    total_cost: f64,
    using_subscription: bool,
    context_percent: Option<f64>,
    context_window: u64,
    auto_compact: bool,
    experimental: bool,
    model_id: Option<String>,
    provider: String,
    thinking: Option<String>,
    provider_count: usize,
    extension_statuses: BTreeMap<String, String>,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn interactive_footer_vectors() {
    let theme = dark_theme();
    let vectors: Vec<FooterVec> = load("interactive_footer");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let data = FooterData {
            cwd: v.cwd.clone(),
            home: v.home.clone(),
            git_branch: v.git_branch.clone(),
            session_name: v.session_name.clone(),
            total_input: v.total_input,
            total_output: v.total_output,
            total_cache_read: v.total_cache_read,
            total_cache_write: v.total_cache_write,
            latest_cache_hit_rate: v.latest_cache_hit_rate,
            total_cost: v.total_cost,
            using_subscription: v.using_subscription,
            context_percent: v.context_percent,
            context_window: v.context_window,
            auto_compact_enabled: v.auto_compact,
            experimental: v.experimental,
            model_id: v.model_id.clone(),
            provider: v.provider.clone(),
            thinking: v.thinking.clone(),
            provider_count: v.provider_count,
            extension_statuses: v.extension_statuses.clone(),
        };
        let component = FooterComponent::new(data, theme.clone());
        assert_eq!(
            component.render(v.width),
            v.expected,
            "FooterComponent[{}] width={}",
            v.label,
            v.width
        );
    }
}

// --- IdleStatus / WorkingStatusIndicator ------------------------------------

#[derive(Deserialize)]
struct StatusVec {
    kind: String,
    message: Option<String>,
    ticks: usize,
    width: usize,
    expected: Vec<String>,
}

#[test]
fn interactive_status_vectors() {
    let theme = dark_theme();
    let vectors: Vec<StatusVec> = load("interactive_status");
    assert!(!vectors.is_empty());
    for v in &vectors {
        let rendered = match v.kind.as_str() {
            "idle" => IdleStatus.render(v.width),
            "working" => {
                let mut indicator =
                    WorkingStatusIndicator::new(&theme, v.message.as_deref().unwrap_or(""), None);
                for _ in 0..v.ticks {
                    indicator.tick();
                }
                indicator.render(v.width)
            }
            other => panic!("unknown status kind {other}"),
        };
        assert_eq!(
            rendered, v.expected,
            "Status[{}] ticks={} width={}",
            v.kind, v.ticks, v.width
        );
    }
}
