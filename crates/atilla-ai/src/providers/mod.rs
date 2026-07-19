//! Provider implementations, mirroring pi-ai's `providers` module
//! (`packages/ai/src/providers`).
//!
//! Providers implement the [`crate::seams::provider::Provider`] seam. Stage 3
//! ports pi's faux provider ([`faux`]) — the scripted, deterministic provider
//! pi's agent and coding-agent tests drive via `registerFauxProvider`. The real
//! wire providers implement the same seam as their HTTP/streaming paths land.

pub mod builtins;
pub mod faux;
pub mod registry;

pub use builtins::{
    builtin_models, builtin_providers, catalog_model_to_ai, provider_from_catalog, radius_provider,
};
pub use registry::{
    clamp_thinking_level, create_models, create_provider, get_supported_thinking_levels,
    models_are_equal, ApiRouting, CreateProviderOptions, FilterModels, Models, MutableModels,
    ProviderAuth, ProviderHeaders, ProviderSnapshot, RefreshContext, RefreshOptions, RefreshResult,
    RegistryProvider, StreamBackendRef,
};
