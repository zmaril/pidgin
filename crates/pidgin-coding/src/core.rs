//! Mirror of pi-coding-agent's `core` module (`packages/coding-agent/src/core`).
//!
//! The `tools` subtree, the config/settings cluster, and a first slice of core
//! glue modules are ported so far — including the `prompt_templates` /
//! `system_prompt` / `slash_commands` shell modules; remaining sibling
//! submodules land later.

pub mod agent_session;
pub mod auth;
pub mod cache_stats;
pub mod command_flow;
pub mod compaction;
pub mod defaults;
pub mod diagnostics;
pub mod event_bus;
pub mod experimental;
pub mod export_html;
pub mod extensions;
pub mod footer_data_provider;
pub mod http_dispatcher;
pub mod keybindings;
pub mod messages;
pub mod model_config;
pub mod model_registry;
pub mod model_resolver;
pub mod model_runtime;
pub mod models_store;
pub mod output_guard;
pub mod package_manager;
pub mod project_trust;
pub mod prompt_templates;
pub mod provider_attribution;
pub mod provider_composer;
pub mod radius;
pub mod remote_catalog_provider;
pub mod resolve_config_value;
pub mod resource_loader;
pub mod resource_loader_orchestrator;
pub mod session_cwd;
pub mod session_manager;
pub mod settings_manager;
pub mod skills;
pub mod slash_commands;
pub mod source_info;
pub mod system_prompt;
pub mod telemetry;
pub mod timings;
pub mod tools;
pub mod trust_manager;

/// Build a fully-populated test [`pidgin_ai::types::Model`] with sensible
/// defaults, shared across the `model_resolver` and `provider_attribution`
/// test modules. Callers mutate the few fields they exercise (`reasoning`,
/// `name`, `base_url`).
///
/// This lives here — rather than in a `core/test_support.rs` — because the
/// coding-agent port keeps its shared test fixtures module-local; a single
/// builder avoids cloning the 13-field `Model` literal into each test module.
#[cfg(test)]
pub(crate) fn test_model(id: &str, provider: &str) -> pidgin_ai::types::Model {
    use pidgin_ai::types::{Modality, ModelCost};
    pidgin_ai::types::Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "anthropic-messages".to_string(),
        provider: provider.to_string(),
        base_url: format!("https://{provider}.example"),
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 1.0,
            tiers: None,
        },
        context_window: 128_000,
        max_tokens: 8192,
        headers: None,
        compat: None,
    }
}

#[cfg(test)]
mod test_support;
