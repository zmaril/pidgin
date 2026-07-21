//! Built-in image api-provider registration — the Rust port of pi-ai's
//! `providers/images/register-builtins.ts`
//! (`packages/ai/src/providers/images/register-builtins.ts`).
//!
//! pi lazily loads the openrouter-images HTTP module (a dynamic `import()`),
//! wraps it so a load failure returns an error [`AssistantImages`], and registers
//! `{ api: "openrouter-images", generateImages }` into the image api registry.
//! Rust has no code-splitting, so the lazy/dynamic-import indirection collapses:
//! [`OpenrouterImagesBackend`] is registered directly.
//!
//! # Port deviation — the transport parameter
//!
//! pi's `registerBuiltInImagesApiProviders()` takes no arguments because the
//! openrouter module constructs its own `new OpenAI(...)` client ambiently. The
//! Rust backend takes its HTTP transport and clock by injection, so registration
//! threads them through — the analog of the chat builtins'
//! `builtin_providers_with_transport`.

// straitjacket-allow-file:duplication — the `#[cfg(test)]` `ImagesModel` /
// `ImagesContext` fixture builders are near-identical to the sibling image
// runtime test modules by design (faithful test fixtures); the clone detector
// pairs them across files.

use std::sync::Arc;

use crate::api::openrouter_images::OpenrouterImagesBackend;
use crate::images_api_registry::{register_images_api_provider, ImagesApiProvider};
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::types::KNOWN_IMAGES_API;

/// Register the built-in image api providers over an injected transport/clock,
/// pi's `registerBuiltInImagesApiProviders` (`register-builtins.ts:44-49`).
///
/// Registers [`OpenrouterImagesBackend`] under the `openrouter-images` api, so
/// [`generate_images`](crate::images::generate_images) can resolve it.
pub fn register_builtin_images_api_providers_with_transport(
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
) {
    register_images_api_provider(
        ImagesApiProvider::new(
            KNOWN_IMAGES_API,
            Arc::new(OpenrouterImagesBackend::new(transport, clock)),
        ),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::generate_images;
    use crate::seams::clock::SystemClock;
    use crate::seams::http::ScriptedTransport;
    use crate::seams::provider::AbortSignal;
    use crate::types::{
        ImagesContext, ImagesInputContent, ImagesModel, ImagesOptions, ImagesStopReason, Modality,
        ModelCost,
    };

    #[test]
    fn registers_openrouter_images_and_dispatches_through_it() {
        let transport = Arc::new(ScriptedTransport::new());
        transport.push_ok(
            serde_json::json!({
                "id": "img-9",
                "choices": [{ "message": {
                    "content": "",
                    "images": [{ "image_url": "data:image/png;base64,ZmFrZQ==" }]
                } }]
            })
            .to_string(),
        );
        register_builtin_images_api_providers_with_transport(
            transport.clone(),
            Arc::new(SystemClock::new()),
        );

        let model = ImagesModel {
            id: "black-forest-labs/flux.2-pro".into(),
            name: "FLUX.2 Pro".into(),
            api: KNOWN_IMAGES_API.into(),
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
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
        };
        let context = ImagesContext {
            input: vec![ImagesInputContent::Text {
                text: "Generate a dog".into(),
                text_signature: None,
            }],
        };
        let options = ImagesOptions {
            api_key: Some("test".into()),
            ..ImagesOptions::default()
        };

        let output = generate_images(&model, &context, Some(&options), None).unwrap();
        assert_eq!(output.stop_reason, ImagesStopReason::Stop);
        assert_eq!(output.response_id.as_deref(), Some("img-9"));
    }

    #[test]
    fn abort_signal_threads_through_the_full_dispatch_path() {
        // An aborted signal handed to the top-level dispatcher must reach the
        // openrouter backend and short-circuit before any transport call.
        let transport = Arc::new(ScriptedTransport::new());
        register_builtin_images_api_providers_with_transport(
            transport.clone(),
            Arc::new(SystemClock::new()),
        );

        let model = ImagesModel {
            id: "black-forest-labs/flux.2-pro".into(),
            name: "FLUX.2 Pro".into(),
            api: KNOWN_IMAGES_API.into(),
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
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
        };
        let context = ImagesContext {
            input: vec![ImagesInputContent::Text {
                text: "Generate a dog".into(),
                text_signature: None,
            }],
        };
        let options = ImagesOptions {
            api_key: Some("test".into()),
            ..ImagesOptions::default()
        };
        let signal = AbortSignal::aborted();

        let output = generate_images(&model, &context, Some(&options), Some(&signal)).unwrap();
        assert_eq!(output.stop_reason, ImagesStopReason::Aborted);
        assert_eq!(output.error_message.as_deref(), Some("Request aborted"));
        assert!(transport.requests().is_empty());
    }
}
