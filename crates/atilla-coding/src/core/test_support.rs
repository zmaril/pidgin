//! Shared `#[cfg(test)]` helpers for the `core` module's unit tests.
//!
//! `project_trust` and `trust_manager` both stand up throwaway temp
//! directories and stringify paths for their store/resource fixtures. That
//! setup used to be copy-pasted into each module's test block; hoisting it here
//! keeps a single source of truth for the scaffolding.

use std::path::{Path, PathBuf};

use atilla_ai::types::{Modality, Model, ModelCost};

/// A minimal `openai-completions` [`Model`] fixture used by the model-store and
/// remote-catalog tests, which each carried an identical local `model()` helper
/// (pi's per-test-file fixtures). Hoisted here so the builder has one home.
pub fn model(provider: &str, id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "openai-completions".to_string(),
        provider: provider.to_string(),
        base_url: "https://coding.example.test/openai/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.5,
            cache_write: 1.5,
            tiers: None,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        compat: None,
    }
}

/// Create a uniquely-named scratch directory under the system temp dir, tagged
/// for the calling test so parallel runs never collide.
pub fn scratch_dir(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "atilla-core-test-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// Borrow a path as an owned `String`, for the many APIs here that take `&str`
/// paths.
pub fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Write `contents` to `path`, creating any missing parent directories first.
/// Shared by the many tests that stage on-disk resource fixtures.
pub fn write(path: &str, contents: &str) {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}
