//! The static image-model catalog — the Rust port of pi-ai's
//! `image-models.generated.ts` (`packages/ai/src/image-models.generated.ts`).
//!
//! pi ships an auto-generated `IMAGE_MODELS` const (provider → model id →
//! [`ImagesModel`]). This port keeps the data faithfully but stores it as an
//! embedded JSON document (`data/image-models.json`) parsed once into a
//! [`OnceLock`], mirroring how [`pidgin_model_catalog`](../../pidgin_model_catalog)
//! embeds the chat catalog via `include_str!` + `serde_json::from_str`. The JSON
//! is transcribed verbatim from the pi generated file at pinned commit
//! `3da591ab` — data, not logic.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::types::ImagesModel;

/// The embedded catalog document, transcribed from pi's `IMAGE_MODELS`.
const IMAGE_MODELS_JSON: &str = include_str!("../data/image-models.json");

/// The image-model catalog: provider id → model id → [`ImagesModel`], pi's
/// `IMAGE_MODELS` (`image-models.generated.ts`).
///
/// Parsed once on first use; panics only if the embedded JSON is corrupt, a
/// condition the module's tests guard against so it cannot occur for a published
/// build.
pub fn image_models() -> &'static BTreeMap<String, BTreeMap<String, ImagesModel>> {
    static MODELS: OnceLock<BTreeMap<String, BTreeMap<String, ImagesModel>>> = OnceLock::new();
    MODELS.get_or_init(|| {
        serde_json::from_str(IMAGE_MODELS_JSON)
            .expect("embedded image-models.json must be a valid IMAGE_MODELS catalog")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_carries_openrouter() {
        let models = image_models();
        let openrouter = models
            .get("openrouter")
            .expect("openrouter provider present");
        assert!(!openrouter.is_empty());
        // Every entry is an openrouter-images model under the openrouter provider.
        for (id, model) in openrouter {
            assert_eq!(model.id, *id);
            assert_eq!(model.api, "openrouter-images");
            assert_eq!(model.provider, "openrouter");
        }
    }

    #[test]
    fn a_known_model_round_trips_faithfully() {
        let model = image_models()["openrouter"]
            .get("google/gemini-2.5-flash-image")
            .expect("gemini flash image present");
        assert_eq!(model.name, "Google: Nano Banana (Gemini 2.5 Flash Image)");
        assert_eq!(model.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(model.cost.input, 0.3);
        assert_eq!(model.cost.output, 2.5);
    }
}
