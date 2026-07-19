//! Integration tests over the embedded model-catalog snapshot.

use std::collections::BTreeMap;
use std::path::PathBuf;

use pidgin_model_catalog::{catalog, manifest, Model};

#[test]
fn embedded_catalog_parses_and_is_substantial() {
    let cat = catalog();
    // Thresholds are set well below the observed snapshot (35 providers,
    // 1070 models) so the test stays green across ordinary refreshes.
    assert!(
        cat.provider_count() > 20,
        "expected > 20 providers, got {}",
        cat.provider_count()
    );
    assert!(cat.len() > 50, "expected > 50 models, got {}", cat.len());
    assert!(!cat.is_empty());
}

#[test]
fn every_model_has_sane_required_fields() {
    let cat = catalog();
    for provider_id in cat.providers() {
        let models = cat.provider(provider_id).expect("provider present");
        assert!(!models.is_empty(), "provider {provider_id} has no models");
        for (model_key, model) in models {
            assert_eq!(
                model_key, &model.id,
                "map key must equal model.id for {provider_id}"
            );
            assert_eq!(
                provider_id, model.provider,
                "outer key must equal model.provider for {}",
                model.id
            );
            assert!(!model.id.is_empty(), "empty id in {provider_id}");
            assert!(!model.name.is_empty(), "empty name for {}", model.id);
            assert!(
                !model.provider.is_empty(),
                "empty provider for {}",
                model.id
            );
            assert!(
                model.context_window > 0,
                "context_window must be > 0 for {}",
                model.id
            );
            assert!(
                model.max_tokens > 0,
                "max_tokens must be > 0 for {}",
                model.id
            );
        }
    }
}

// pi uses -1_000_000 as a sentinel for models whose price is dynamic/unknown
// (e.g. openrouter's auto-routing entries), so cost rates are finite but not
// always non-negative.
const DYNAMIC_PRICE_SENTINEL: f64 = -1_000_000.0;

#[test]
fn cost_fields_are_finite_and_numeric() {
    let cat = catalog();
    for (_provider, model) in cat.all_models() {
        let rates = &model.cost.rates;
        for (label, value) in [
            ("input", rates.input),
            ("output", rates.output),
            ("cache_read", rates.cache_read),
            ("cache_write", rates.cache_write),
        ] {
            assert!(
                value.is_finite(),
                "{label} rate for {} must be finite, got {value}",
                model.id
            );
            assert!(
                value >= 0.0 || value == DYNAMIC_PRICE_SENTINEL,
                "{label} rate for {} must be non-negative or the dynamic-price sentinel, got {value}",
                model.id
            );
        }

        if let Some(tiers) = &model.cost.tiers {
            let mut previous_threshold: Option<u64> = None;
            for tier in tiers {
                assert!(
                    tier.input_tokens_above > 0,
                    "tier threshold must be positive for {}",
                    model.id
                );
                if let Some(prev) = previous_threshold {
                    assert!(
                        tier.input_tokens_above > prev,
                        "tier thresholds must be strictly increasing for {}",
                        model.id
                    );
                }
                previous_threshold = Some(tier.input_tokens_above);
                for value in [
                    tier.rates.input,
                    tier.rates.output,
                    tier.rates.cache_read,
                    tier.rates.cache_write,
                ] {
                    assert!(
                        value.is_finite(),
                        "tier rate for {} must be finite",
                        model.id
                    );
                    assert!(
                        value >= 0.0 || value == DYNAMIC_PRICE_SENTINEL,
                        "tier rate for {} must be non-negative or the dynamic-price sentinel",
                        model.id
                    );
                }
            }
        }
    }
}

#[test]
fn unknown_fields_are_tolerated_and_captured() {
    // A model JSON with an extra, unmodeled field must still deserialize, and
    // the field must land in `extra` — proving forward compatibility.
    let json = r#"{
        "id": "future-model",
        "name": "Future Model",
        "api": "some-future-api",
        "provider": "future-provider",
        "baseUrl": "https://example.com",
        "reasoning": true,
        "input": ["text", "image", "audio"],
        "cost": { "input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 0.2 },
        "contextWindow": 100000,
        "maxTokens": 8000,
        "bogusFutureField": { "nested": 42 }
    }"#;

    let model: Model = serde_json::from_str(json).expect("must deserialize despite unknown field");
    assert_eq!(model.id, "future-model");
    assert!(
        model.extra.contains_key("bogusFutureField"),
        "unknown field must be captured in extra"
    );
    // An unknown modality ("audio") must map to the catch-all rather than error.
    assert_eq!(model.input.len(), 3);
}

#[test]
fn per_provider_files_agree_with_aggregate() {
    let cat = catalog();
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/providers");

    let mut checked = 0usize;
    for provider_id in cat.providers() {
        let path = data_dir.join(format!("{provider_id}.json"));
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let per_provider: BTreeMap<String, Model> = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parsing {provider_id}.json: {e}"));

        let aggregate = cat.provider(provider_id).expect("provider in aggregate");
        assert_eq!(
            &per_provider, aggregate,
            "per-provider file for {provider_id} must match the aggregate"
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        cat.provider_count(),
        "must check one file per provider"
    );
}

#[test]
fn manifest_parses_and_matches_embedded_catalog() {
    let m = manifest();
    let cat = catalog();
    assert_eq!(
        m.provider_count,
        cat.provider_count(),
        "manifest provider_count must match embedded catalog"
    );
    assert_eq!(
        m.model_count,
        cat.len(),
        "manifest model_count must match embedded catalog"
    );
    assert!(!m.pi_pin.is_empty());
    assert!(!m.generated_at.is_empty());
}
