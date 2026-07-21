//! The static image-model registry — the Rust port of pi-ai's `image-models.ts`
//! (`packages/ai/src/image-models.ts`).
//!
//! pi builds a `Map<provider, Map<id, ImagesModel>>` from the generated
//! [`IMAGE_MODELS`](crate::image_models_generated::image_models) const and
//! exposes sync lookups over it. This port reads directly from the embedded
//! catalog; pi's `TProvider`/`TModelId` generic api-inference collapses in Rust
//! (the [`ImagesModel::api`](crate::types::ImagesModel) field is a plain string).

use crate::image_models_generated::image_models;
use crate::types::ImagesModel;

/// Look up a single catalog model by provider and id, pi's `getImageModel`
/// (`image-models.ts:22-28`). `None` when the provider or model is unknown
/// (pi returns `undefined`, typed away by its conditional generic).
pub fn get_image_model(provider: &str, model_id: &str) -> Option<ImagesModel> {
    image_models()
        .get(provider)
        .and_then(|models| models.get(model_id))
        .cloned()
}

/// Every provider id in the catalog, pi's `getImageProviders`
/// (`image-models.ts:30-32`).
pub fn get_image_providers() -> Vec<String> {
    image_models().keys().cloned().collect()
}

/// Every model for `provider`, pi's `getImageModels` (`image-models.ts:34-41`).
/// An empty vector when the provider is unknown.
pub fn get_image_models(provider: &str) -> Vec<ImagesModel> {
    image_models()
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_image_model_resolves_known_and_unknown() {
        let model = get_image_model("openrouter", "google/gemini-2.5-flash-image")
            .expect("known model resolves");
        assert_eq!(model.id, "google/gemini-2.5-flash-image");
        assert_eq!(model.api, "openrouter-images");

        assert!(get_image_model("openrouter", "no-such-model").is_none());
        assert!(get_image_model("no-such-provider", "x").is_none());
    }

    #[test]
    fn get_image_providers_lists_openrouter() {
        let providers = get_image_providers();
        assert!(providers.contains(&"openrouter".to_string()));
    }

    #[test]
    fn get_image_models_returns_catalog_slice() {
        let models = get_image_models("openrouter");
        assert!(!models.is_empty());
        assert!(models.iter().all(|m| m.provider == "openrouter"));
        assert!(get_image_models("no-such-provider").is_empty());
    }
}
