//! The OpenRouter image-generation provider factory — the Rust port of pi-ai's
//! `providers/openrouter-images.ts` (`packages/ai/src/providers/openrouter-images.ts`).
//!
//! pi's `openrouterImagesProvider()` builds the OpenRouter image provider via
//! `createImagesProvider`: id `openrouter`, `OPENROUTER_API_KEY` env auth, models
//! = `IMAGE_MODELS.openrouter` values, api = the (lazy) openrouter images api.
//!
//! Like the chat [`builtin_providers`](crate::providers::builtin_providers) vs.
//! [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport)
//! split, the no-transport [`openrouter_images_provider`] wires an
//! [`UnimplementedImagesApi`] (the image analog of
//! [`ApiRouting::Unimplemented`](crate::providers::ApiRouting::Unimplemented)),
//! while [`openrouter_images_provider_with_transport`] binds the real
//! [`OpenrouterImagesBackend`](crate::api::openrouter_images::OpenrouterImagesBackend)
//! over an injected transport/clock.

use std::sync::Arc;

use crate::api::openrouter_images::OpenrouterImagesBackend;
use crate::auth::{env_api_key_auth, ProviderAuth};
use crate::image_models::get_image_models;
use crate::images_models::{create_images_provider, CreateImagesProviderOptions, ImagesProvider};
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::AbortSignal;
use crate::types::{
    AssistantImages, ImagesContext, ImagesModel, ImagesOptions, ImagesStopReason, ProviderImages,
};

/// A [`ProviderImages`] that is registered but not wired to a transport — the
/// image analog of [`ApiRouting::Unimplemented`](crate::providers::ApiRouting::Unimplemented).
///
/// Constructing the provider without a transport (the default build carries no
/// HTTP stack) leaves generation unwired; a `generateImages` call returns an
/// error [`AssistantImages`] rather than performing network I/O.
pub struct UnimplementedImagesApi;

impl ProviderImages for UnimplementedImagesApi {
    fn generate_images(
        &self,
        model: &ImagesModel,
        _context: &ImagesContext,
        _options: Option<&ImagesOptions>,
        _signal: Option<&AbortSignal>,
    ) -> AssistantImages {
        AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: Vec::new(),
            response_id: None,
            usage: None,
            stop_reason: ImagesStopReason::Error,
            // Parallels the chat `Unimplemented` routing's error text
            // (`registry` / pi `models.ts:583`). Deterministic timestamp, as on
            // that sibling error shell.
            error_message: Some(format!(
                "Provider {} has no API implementation for \"{}\"",
                model.provider, model.api
            )),
            timestamp: 0,
        }
    }
}

/// Assemble the OpenRouter provider options, pi's `createImagesProvider(...)`
/// argument, parameterized on the [`ProviderImages`] api implementation.
fn openrouter_options(api: Arc<dyn ProviderImages>) -> CreateImagesProviderOptions {
    CreateImagesProviderOptions {
        id: "openrouter".to_string(),
        name: Some("OpenRouter".to_string()),
        auth: ProviderAuth {
            api_key: Some(Box::new(env_api_key_auth(
                "OpenRouter API key",
                &["OPENROUTER_API_KEY"],
            ))),
            oauth: None,
        },
        // pi: Object.values(IMAGE_MODELS.openrouter).
        models: get_image_models("openrouter"),
        refresh_models: None,
        api,
    }
}

/// The OpenRouter image provider with generation unwired, pi's
/// `openrouterImagesProvider` (`providers/openrouter-images.ts:6-14`) built
/// without a transport (see the module note).
pub fn openrouter_images_provider() -> Arc<dyn ImagesProvider> {
    create_images_provider(openrouter_options(Arc::new(UnimplementedImagesApi)))
}

/// The OpenRouter image provider wired for real HTTP over `transport`, stamping
/// results from `clock`.
pub fn openrouter_images_provider_with_transport(
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
) -> Arc<dyn ImagesProvider> {
    create_images_provider(openrouter_options(Arc::new(OpenrouterImagesBackend::new(
        transport, clock,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_carries_openrouter_catalog_and_env_auth() {
        let provider = openrouter_images_provider();
        assert_eq!(provider.id(), "openrouter");
        assert_eq!(provider.name(), "OpenRouter");
        let models = provider.get_models();
        assert!(!models.is_empty());
        assert!(models.iter().all(|m| m.api == "openrouter-images"));
    }
}
