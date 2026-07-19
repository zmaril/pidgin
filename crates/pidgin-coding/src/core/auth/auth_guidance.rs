//! Login-guidance message formatters, ported from pi's
//! `core/auth-guidance.ts`.
//!
//! Four pure formatters produce the user-facing prose shown when no models /
//! model / API key is available, pointing the user at `/login` and the bundled
//! docs.
//!
//! # Docs-path deviation
//!
//! pi's `getDocsPath` (`config.ts:432`) resolves `<packageDir>/docs`, where
//! `getPackageDir` honours `PI_PACKAGE_DIR` and otherwise walks up from the
//! bundle's own directory for a `package.json`. That walk is environment-shaped
//! (it depends on how the JS bundle is laid out on disk) and has no pidgin
//! analogue, so [`get_docs_path`] ports only the `PI_PACKAGE_DIR` override and
//! falls back to the current working directory. The formatters themselves are a
//! faithful transcription.

use crate::utils::paths::{normalize_path, PathInputOptions};

/// The provider display name pi uses when the selected model has no known
/// provider (`auth-guidance.ts:4`).
const UNKNOWN_PROVIDER: &str = "unknown";

/// Environment variable overriding the package directory (pi's `PI_PACKAGE_DIR`,
/// `config.ts:369`).
const ENV_PACKAGE_DIR: &str = "PI_PACKAGE_DIR";

/// The package directory (pi's `getPackageDir`, `config.ts:367`).
///
/// Ports the `PI_PACKAGE_DIR` override; the bundle-relative `package.json` walk
/// is environment-shaped and falls back to the current working directory (see
/// the module docs).
fn get_package_dir() -> String {
    if let Ok(dir) = std::env::var(ENV_PACKAGE_DIR) {
        if !dir.is_empty() {
            return normalize_path(&dir, &PathInputOptions::default()).unwrap_or(dir);
        }
    }
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The bundled docs directory (pi's `getDocsPath`, `config.ts:432`).
pub fn get_docs_path() -> String {
    format!("{}/docs", get_package_dir())
}

/// Help text pointing the user at `/login` and the provider/model docs
/// (`auth-guidance.ts:6-12`).
pub fn get_provider_login_help() -> String {
    let docs = get_docs_path();
    [
        "Use /login to log into a provider via OAuth or API key. See:".to_string(),
        format!("  {docs}/providers.md"),
        format!("  {docs}/models.md"),
    ]
    .join("\n")
}

/// Message shown when no models are available at all (`auth-guidance.ts:14-16`).
pub fn format_no_models_available_message() -> String {
    format!("No models available. {}", get_provider_login_help())
}

/// Message shown when models exist but none is selected
/// (`auth-guidance.ts:18-20`).
pub fn format_no_model_selected_message() -> String {
    format!(
        "No model selected.\n\n{}\n\nThen use /model to select a model.",
        get_provider_login_help()
    )
}

/// Message shown when the selected provider has no API key
/// (`auth-guidance.ts:22-25`).
pub fn format_no_api_key_found_message(provider: &str) -> String {
    let provider_display = if provider == UNKNOWN_PROVIDER {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {provider_display}.\n\n{}",
        get_provider_login_help()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_login_help_lists_docs_paths() {
        let help = get_provider_login_help();
        assert!(help.starts_with("Use /login to log into a provider via OAuth or API key. See:\n"));
        assert!(help.contains("/providers.md"));
        assert!(help.contains("/models.md"));
    }

    #[test]
    fn no_models_available_prefixes_help() {
        let message = format_no_models_available_message();
        assert!(message.starts_with("No models available. "));
        assert!(message.contains("Use /login"));
    }

    #[test]
    fn no_model_selected_appends_model_hint() {
        let message = format_no_model_selected_message();
        assert!(message.starts_with("No model selected.\n\n"));
        assert!(message.ends_with("\n\nThen use /model to select a model."));
    }

    #[test]
    fn no_api_key_found_uses_provider_name() {
        let message = format_no_api_key_found_message("anthropic");
        assert!(message.starts_with("No API key found for anthropic.\n\n"));
    }

    #[test]
    fn no_api_key_found_masks_unknown_provider() {
        let message = format_no_api_key_found_message("unknown");
        assert!(message.starts_with("No API key found for the selected model.\n\n"));
    }
}
