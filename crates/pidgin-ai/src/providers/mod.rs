//! Provider implementations, mirroring pi-ai's `providers` module
//! (`packages/ai/src/providers`).
//!
//! Providers implement the [`crate::seams::provider::Provider`] seam. Stage 3
//! ports pi's faux provider ([`faux`]) — the scripted, deterministic provider
//! pi's agent and coding-agent tests drive via `registerFauxProvider`. The real
//! wire providers implement the same seam as their HTTP/streaming paths land.

pub mod anthropic_backend;
pub mod builtins;
pub mod composer;
pub mod faux;
pub mod google_generative_ai_backend;
pub mod mistral_backend;
pub mod openai_completions_backend;
pub mod registry;

pub use anthropic_backend::{AnthropicMessagesBackend, ANTHROPIC_MESSAGES_API};
pub use builtins::{
    builtin_models, builtin_providers, builtin_providers_with_transport, catalog_model_to_ai,
    provider_from_catalog, provider_from_catalog_with_transport, radius_provider,
};
pub use composer::{
    adapt_oauth, compose_api_key_auth, compose_model_provider, compose_oauth_auth,
    config_context_env, with_configured_auth, ComposeAuthError, ComposeModelProviderInput,
    ComposedProvider, ConfigValueError, ConfigValueResolver, ExtensionAuthConfig,
    ExtensionOAuthConfig, ProviderAuthConfig,
};
pub use google_generative_ai_backend::{GoogleGenerativeAiBackend, GOOGLE_GENERATIVE_AI_API};
pub use mistral_backend::{MistralBackend, MISTRAL_CONVERSATIONS_API};
pub use openai_completions_backend::{OpenAICompletionsBackend, OPENAI_COMPLETIONS_API};
pub use registry::{
    clamp_thinking_level, create_models, create_provider, get_supported_thinking_levels,
    models_are_equal, ApiRouting, CreateProviderOptions, FilterModels, Models, MutableModels,
    ProviderAuth, ProviderHeaders, ProviderSnapshot, RefreshContext, RefreshOptions, RefreshResult,
    RegistryProvider, StreamBackendRef,
};
