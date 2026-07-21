//! The image-generation api-provider registry — the Rust port of pi-ai's
//! `images-api-registry.ts` (`packages/ai/src/images-api-registry.ts`).
//!
//! pi keeps a runtime `Map<ImagesApi, provider>` that image api ids resolve
//! through: [`register_images_api_provider`] populates it and
//! [`get_images_api_provider`] reads it back. It is the direct image-side
//! counterpart of [`crate::compat`]'s chat api-provider registry, and — like
//! that registry — stores its entries in an `OnceLock<Mutex<BTreeMap>>` rather
//! than pi's module-level `Map`.
//!
//! The registry is a *separate* registry from the chat one: registering image
//! builtins here never touches the chat `backend_for_api` mechanics that
//! [`crate::compat`] and [`crate::providers::builtins`] exercise.

// straitjacket-allow-file:duplication — this registry's `OnceLock<Mutex<BTreeMap>>`
// mirror of `compat.rs`'s api-provider registry (register/get plus the
// api-mismatch guard) is a faithful image-side transcription; the clone detector
// pairs the two by design.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::seams::provider::AbortSignal;
use crate::types::{AssistantImages, ImagesContext, ImagesModel, ImagesOptions, ProviderImages};

/// A mismatch raised by the registry's `generateImages` guard — the value analog
/// of pi's synchronous `throw new Error(...)` in `wrapGenerateImages`
/// (`images-api-registry.ts:31`).
///
/// pi throws when the dispatched model's api does not match the provider it was
/// routed to; modelling it as a returned `Err` keeps that behavior a value (a
/// catchable throw at the napi boundary rather than an uncatchable panic). Its
/// [`Display`](std::fmt::Display) text is byte-for-byte pi's `Error` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImagesApiError {
    /// pi's `wrapGenerateImages` mismatch throw (`images-api-registry.ts:31`).
    MismatchedApi {
        /// The api the provider serves (pi's expected `api`).
        expected: String,
        /// The dispatched model's api (pi's `model.api`).
        actual: String,
    },
}

impl std::fmt::Display for ImagesApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImagesApiError::MismatchedApi { expected, actual } => {
                write!(f, "Mismatched api: {actual} expected {expected}")
            }
        }
    }
}

impl std::error::Error for ImagesApiError {}

/// A registered image api provider — pi's `ImagesApiProvider`
/// (`images-api-registry.ts:9-12`).
///
/// Carries the api id it serves and the [`ProviderImages`] seam that generates
/// images for it. [`generate_images`](Self::generate_images) guards against an
/// api mismatch exactly as pi's `wrapGenerateImages` does. Clone is a cheap
/// `Arc` bump, which is how [`get_images_api_provider`] hands an entry back out.
///
/// pi declares this interface in `images-api-registry.ts` (not `types.ts`), so
/// the port keeps it here rather than in [`crate::types`], preserving pi's file
/// boundaries.
#[derive(Clone)]
pub struct ImagesApiProvider {
    api: String,
    provider: Arc<dyn ProviderImages>,
}

impl ImagesApiProvider {
    /// Wrap a [`ProviderImages`] seam as a registry entry serving `api`
    /// (pi's `{ api, generateImages }`).
    pub fn new(api: impl Into<String>, provider: Arc<dyn ProviderImages>) -> Self {
        Self {
            api: api.into(),
            provider,
        }
    }

    /// The api id this provider serves (pi's `provider.api`).
    pub fn api(&self) -> &str {
        &self.api
    }

    /// Generate images through the wrapped provider. Returns
    /// [`ImagesApiError::MismatchedApi`] on an api mismatch, mirroring pi's
    /// `wrapGenerateImages` throw; the normal [`get_images_api_provider`]
    /// dispatch path never mismatches, so the `Err` only fires on a caller
    /// contract violation.
    pub fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
        signal: Option<&AbortSignal>,
    ) -> Result<AssistantImages, ImagesApiError> {
        if model.api != self.api {
            return Err(ImagesApiError::MismatchedApi {
                expected: self.api.clone(),
                actual: model.api.clone(),
            });
        }
        Ok(self
            .provider
            .generate_images(model, context, options, signal))
    }
}

impl std::fmt::Debug for ImagesApiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagesApiProvider")
            .field("api", &self.api)
            .finish()
    }
}

/// A registry entry: the provider plus the optional source id that scopes bulk
/// removal (pi's `RegisteredImagesApiProvider`, `images-api-registry.ts:18-21`).
struct RegisteredImagesApiProvider {
    provider: ImagesApiProvider,
    #[allow(dead_code)]
    source_id: Option<String>,
}

fn registry() -> &'static Mutex<BTreeMap<String, RegisteredImagesApiProvider>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, RegisteredImagesApiProvider>>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Register `provider` under its api id, optionally tagged with `source_id`.
/// pi's `registerImagesApiProvider` (`images-api-registry.ts:35-48`).
pub fn register_images_api_provider(provider: ImagesApiProvider, source_id: Option<&str>) {
    registry().lock().unwrap().insert(
        provider.api().to_string(),
        RegisteredImagesApiProvider {
            provider,
            source_id: source_id.map(str::to_string),
        },
    );
}

/// Look up the provider registered for `api`. pi's `getImagesApiProvider`
/// (`images-api-registry.ts:50-52`).
///
/// Returns an owned (`Arc`-backed) clone rather than pi's `&`-shape: the registry
/// is a runtime-mutable map, so a borrowed-static handle cannot be handed out
/// safely. The clone is a cheap `Arc` bump — the faithful Rust analog.
pub fn get_images_api_provider(api: &str) -> Option<ImagesApiProvider> {
    registry()
        .lock()
        .unwrap()
        .get(api)
        .map(|entry| entry.provider.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImagesStopReason, Modality};

    struct StubProvider {
        api: String,
    }

    impl ProviderImages for StubProvider {
        fn generate_images(
            &self,
            model: &ImagesModel,
            _context: &ImagesContext,
            _options: Option<&ImagesOptions>,
            _signal: Option<&AbortSignal>,
        ) -> AssistantImages {
            AssistantImages {
                api: self.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                output: Vec::new(),
                response_id: None,
                usage: None,
                stop_reason: ImagesStopReason::Stop,
                error_message: None,
                timestamp: 1,
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
            cost: crate::types::ModelCost {
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
    fn register_and_get_round_trip() {
        register_images_api_provider(
            ImagesApiProvider::new(
                "registry-test-api",
                Arc::new(StubProvider {
                    api: "registry-test-api".into(),
                }),
            ),
            None,
        );
        let provider = get_images_api_provider("registry-test-api").expect("registered");
        let out = provider
            .generate_images(
                &model("registry-test-api"),
                &ImagesContext::default(),
                None,
                None,
            )
            .expect("api matches");
        assert_eq!(out.stop_reason, ImagesStopReason::Stop);
    }

    #[test]
    fn generate_images_guards_api_mismatch() {
        let provider = ImagesApiProvider::new(
            "expected-api",
            Arc::new(StubProvider {
                api: "expected-api".into(),
            }),
        );
        let err = provider
            .generate_images(&model("other-api"), &ImagesContext::default(), None, None)
            .expect_err("mismatch");
        assert_eq!(
            err,
            ImagesApiError::MismatchedApi {
                expected: "expected-api".into(),
                actual: "other-api".into(),
            }
        );
        assert_eq!(
            err.to_string(),
            "Mismatched api: other-api expected expected-api"
        );
    }

    #[test]
    fn get_returns_none_for_unregistered() {
        assert!(get_images_api_provider("no-such-image-api-xyz").is_none());
    }
}
