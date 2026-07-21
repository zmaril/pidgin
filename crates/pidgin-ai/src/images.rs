//! The top-level image-generation dispatcher — the Rust port of pi-ai's
//! `images.ts` (`packages/ai/src/images.ts`).
//!
//! pi's `images.ts` resolves the api provider registered for `model.api` and
//! delegates to it, throwing `No API provider registered for api: {api}` when
//! none is registered. Importing the module additionally triggers built-in
//! registration via a side-effect import of `providers/images/register-builtins.ts`.
//!
//! The Rust port keeps the dispatcher pure: it resolves through
//! [`get_images_api_provider`](crate::images_api_registry::get_images_api_provider)
//! and delegates. Rust has no side-effect imports, so the built-in registration
//! is an explicit call — see
//! [`register_builtin_images_api_providers_with_transport`](crate::providers::images::register_builtins::register_builtin_images_api_providers_with_transport),
//! which must run before this dispatcher can resolve the `openrouter-images` api.

// straitjacket-allow-file:duplication — the `#[cfg(test)]` `ImagesModel` /
// `AssistantImages` fixture builders are near-identical to the sibling image
// runtime test modules by design (faithful transcriptions of pi's test
// fixtures); the clone detector pairs them across files.

use crate::images_api_registry::{get_images_api_provider, ImagesApiError, ImagesApiProvider};
use crate::seams::provider::AbortSignal;
use crate::types::{AssistantImages, ImagesContext, ImagesModel, ImagesOptions};

/// A dispatch error raised by [`generate_images`] — the value analog of pi's
/// synchronous `throw new Error(...)` paths in `images.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerateImagesError {
    /// pi's `resolveImagesApiProvider` throw (`images.ts:8`): no provider is
    /// registered for the dispatched model's api.
    NoApiProvider {
        /// The dispatched model's api with no registered provider.
        api: String,
    },
    /// The wrapped provider's api-mismatch guard fired (pi's `wrapGenerateImages`,
    /// surfaced through [`ImagesApiError`]).
    Provider(ImagesApiError),
}

impl std::fmt::Display for GenerateImagesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenerateImagesError::NoApiProvider { api } => {
                write!(f, "No API provider registered for api: {api}")
            }
            GenerateImagesError::Provider(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for GenerateImagesError {}

impl From<ImagesApiError> for GenerateImagesError {
    fn from(error: ImagesApiError) -> Self {
        GenerateImagesError::Provider(error)
    }
}

/// Resolve the api provider for `api`, pi's `resolveImagesApiProvider`
/// (`images.ts:6-12`). `Err` carries pi's `No API provider registered ...`
/// message.
fn resolve_images_api_provider(api: &str) -> Result<ImagesApiProvider, GenerateImagesError> {
    get_images_api_provider(api).ok_or_else(|| GenerateImagesError::NoApiProvider {
        api: api.to_string(),
    })
}

/// Generate images through the api provider registered for `model.api`, pi's
/// top-level `generateImages` (`images.ts:14-21`).
///
/// pi accepts `ProviderImagesOptions` (`ImagesOptions & Record<string, unknown>`);
/// the Rust port takes the plain-data [`ImagesOptions`] subset plus `signal`,
/// which pi carries inside `options.signal` but the serializable options defer
/// (threaded as its own parameter, exactly like the chat
/// [`compat::stream`](crate::compat::stream) dispatcher). A missing provider is
/// returned as [`GenerateImagesError::NoApiProvider`] rather than a thrown `Error`.
pub fn generate_images(
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    signal: Option<&AbortSignal>,
) -> Result<AssistantImages, GenerateImagesError> {
    let provider = resolve_images_api_provider(&model.api)?;
    Ok(provider.generate_images(model, context, options, signal)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images_api_registry::register_images_api_provider;
    use crate::types::{ImagesStopReason, Modality, ModelCost, ProviderImages};
    use std::sync::Arc;

    struct EchoProvider;

    impl ProviderImages for EchoProvider {
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
                stop_reason: ImagesStopReason::Stop,
                error_message: None,
                timestamp: 7,
            }
        }
    }

    fn model(api: &str) -> ImagesModel {
        ImagesModel {
            id: "m".into(),
            name: "m".into(),
            api: api.into(),
            provider: "p".into(),
            base_url: "https://example.test/v1".into(),
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
            headers: None,
            output: vec![Modality::Image],
        }
    }

    #[test]
    fn dispatches_to_registered_provider() {
        register_images_api_provider(
            ImagesApiProvider::new("images-dispatch-test", Arc::new(EchoProvider)),
            None,
        );
        let out = generate_images(
            &model("images-dispatch-test"),
            &ImagesContext::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(out.stop_reason, ImagesStopReason::Stop);
        assert_eq!(out.timestamp, 7);
    }

    #[test]
    fn errors_when_no_provider_registered() {
        let err = generate_images(
            &model("unregistered-image-api-xyz"),
            &ImagesContext::default(),
            None,
            None,
        )
        .expect_err("no provider");
        assert_eq!(
            err.to_string(),
            "No API provider registered for api: unregistered-image-api-xyz"
        );
    }
}
