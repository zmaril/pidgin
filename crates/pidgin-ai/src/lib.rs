//! Rust mirror of `@earendil-works/pi-ai` (`packages/ai`).
//!
//! This crate mirrors the provider and model surface of pi's AI package.
//! Modules mirror pi's `src/` top-level layout; port order runs roughly
//! `types` and `utils` first, then `auth`, `providers`, `api`, and finally
//! `compat`. Stage 1 ports the boundary types (`types.ts`) and cost math
//! (`models.ts`'s `calculateCost`); Stage 2 ports the Anthropic Messages SSE
//! streaming parser (`api/anthropic.rs`) and its JSON-repair helpers
//! (`utils/json_parse.rs`). Stage 3 defines the injection seams
//! (`seams/`, the production-grade trait boundaries the core is built on) and
//! ports pi's faux provider (`providers/faux.rs`). The remaining modules are
//! still stubs.

pub mod api;
pub mod auth;
pub mod compat;
pub mod cost;
pub mod env_api_keys;
pub mod models_store;
pub mod providers;
pub mod seams;
pub mod session_resources;
pub mod types;
pub mod utils;

pub use api::lazy::lazy_stream;
pub use compat::{
    get_api_provider, get_api_providers, register_api_provider, register_builtin_api_providers,
    register_faux_provider, reset_api_providers, unregister_api_providers, ApiProvider,
    CompatError, FauxProviderRegistration,
};
pub use cost::{calculate_cost, calculate_cost_with};
pub use env_api_keys::{find_env_keys, get_api_key_env_vars, get_env_api_key, AMBIENT_SENTINEL};
pub use models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ProviderModelsStore};
pub use providers::composer::{
    adapt_oauth, compose_api_key_auth, compose_model_provider, compose_oauth_auth,
    config_context_env, with_configured_auth, ComposeAuthError, ComposeModelProviderInput,
    ComposedProvider, ConfigValueError, ConfigValueResolver, ExtensionAuthConfig,
    ExtensionOAuthConfig, ProviderAuthConfig,
};
pub use providers::{
    builtin_models, builtin_providers, builtin_providers_with_transport, clamp_thinking_level,
    create_models, create_provider, get_supported_thinking_levels, models_are_equal,
    provider_from_catalog_with_transport, radius_provider, AnthropicMessagesBackend, ApiRouting,
    CreateProviderOptions, FilterModels, Models, MutableModels, ProviderAuth, ProviderHeaders,
    ProviderSnapshot, RefreshContext, RefreshOptions, RefreshResult, RegistryProvider,
    ANTHROPIC_MESSAGES_API,
};
pub use session_resources::{
    cleanup_session_resources, register_session_resource_cleanup, AggregateCleanupError,
    CleanupError, SessionResourceCleanup,
};
pub use types::*;
pub use utils::event_stream::{
    create_assistant_message_event_stream, AssistantMessageEventStream, EventStream,
};
pub use utils::retry::is_retryable_assistant_error;

/// Name of the pi package this crate mirrors.
pub const PI_PACKAGE: &str = "@earendil-works/pi-ai";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_pi_ai() {
        assert_eq!(PI_PACKAGE, "@earendil-works/pi-ai");
    }
}
