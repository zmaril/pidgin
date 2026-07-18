//! Environment-variable API-key resolution, ported from pi's
//! `packages/ai/src/env-api-keys.ts` (pinned commit `3da591ab`).
//!
//! This mirrors pi's `getApiKeyEnvVars` (including the `github-copilot` and
//! `anthropic` special cases), `findEnvKeys`, and `getEnvApiKey`, plus the
//! ambient sentinel [`AMBIENT_SENTINEL`] returned for `google-vertex` (ADC) and
//! `amazon-bedrock` (AWS credential chain).
//!
//! # Environment access
//!
//! pi reads variables through `getProviderEnvValue(name, env?)`, which prefers a
//! scoped override map, then `process.env`, treating empty strings as absent
//! (JS `||`). The Rust port takes an explicit `env` lookup closure so tests can
//! inject a fake environment without mutating the process-global — the public
//! [`find_env_keys`]/[`get_env_api_key`] convenience wrappers read the real
//! process environment via [`std::env::var`].

/// Sentinel returned by [`get_env_api_key`] for providers configured through an
/// ambient credential source (Vertex ADC, the AWS credential chain) rather than
/// a literal API key. Mirrors pi's `"<authenticated>"`.
pub const AMBIENT_SENTINEL: &str = "<authenticated>";

/// The ordered list of environment variables that can supply an API key for
/// `provider`, or `None` when the provider has no known env-key source.
///
/// Port of pi's `getApiKeyEnvVars` (`env-api-keys.ts:64`). The order is
/// significant: [`get_env_api_key`] returns the first configured variable, so
/// `anthropic` resolves `ANTHROPIC_OAUTH_TOKEN` ahead of `ANTHROPIC_API_KEY`.
pub fn get_api_key_env_vars(provider: &str) -> Option<Vec<&'static str>> {
    // github-copilot uses only the dedicated token, never generic GitHub tokens.
    if provider == "github-copilot" {
        return Some(vec!["COPILOT_GITHUB_TOKEN"]);
    }

    // ANTHROPIC_OAUTH_TOKEN takes precedence over ANTHROPIC_API_KEY.
    if provider == "anthropic" {
        return Some(vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]);
    }

    let var = match provider {
        "ant-ling" => "ANT_LING_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "azure-openai-responses" => "AZURE_OPENAI_API_KEY",
        "nvidia" => "NVIDIA_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "google" => "GEMINI_API_KEY",
        "google-vertex" => "GOOGLE_CLOUD_API_KEY",
        "groq" => "GROQ_API_KEY",
        "cerebras" => "CEREBRAS_API_KEY",
        "xai" => "XAI_API_KEY",
        "radius" => "RADIUS_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "vercel-ai-gateway" => "AI_GATEWAY_API_KEY",
        "zai" => "ZAI_API_KEY",
        "zai-coding-cn" => "ZAI_CODING_CN_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "minimax-cn" => "MINIMAX_CN_API_KEY",
        "moonshotai" => "MOONSHOT_API_KEY",
        "moonshotai-cn" => "MOONSHOT_API_KEY",
        "huggingface" => "HF_TOKEN",
        "fireworks" => "FIREWORKS_API_KEY",
        "together" => "TOGETHER_API_KEY",
        "opencode" => "OPENCODE_API_KEY",
        "opencode-go" => "OPENCODE_API_KEY",
        "kimi-coding" => "KIMI_API_KEY",
        "cloudflare-workers-ai" => "CLOUDFLARE_API_KEY",
        "cloudflare-ai-gateway" => "CLOUDFLARE_API_KEY",
        "xiaomi" => "XIAOMI_API_KEY",
        "xiaomi-token-plan-cn" => "XIAOMI_TOKEN_PLAN_CN_API_KEY",
        "xiaomi-token-plan-ams" => "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
        "xiaomi-token-plan-sgp" => "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        _ => return None,
    };
    Some(vec![var])
}

/// Read an environment value the way pi's `getProviderEnvValue` does: an empty
/// string is treated as absent (JS truthiness), so callers only ever see
/// non-empty values.
fn read_env(env: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    match env(name) {
        Some(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// Find the configured API-key environment variables for `provider`, resolving
/// values through the `env` lookup closure.
///
/// Port of pi's `findEnvKeys` (`env-api-keys.ts:122`): returns only variables
/// that are actually set, preserving declaration order, or `None` when the
/// provider is unknown or none are set. Ambient sources (AWS profiles/IAM,
/// Google ADC) are intentionally excluded — see [`get_env_api_key_with`].
pub fn find_env_keys_with(
    provider: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Option<Vec<String>> {
    let vars = get_api_key_env_vars(provider)?;
    let found: Vec<String> = vars
        .into_iter()
        .filter(|name| read_env(&env, name).is_some())
        .map(str::to_string)
        .collect();
    if found.is_empty() {
        None
    } else {
        Some(found)
    }
}

/// [`find_env_keys_with`] against the real process environment.
pub fn find_env_keys(provider: &str) -> Option<Vec<String>> {
    find_env_keys_with(provider, |name| std::env::var(name).ok())
}

/// Resolve an API key for `provider` from known environment variables, or the
/// ambient [`AMBIENT_SENTINEL`] for providers configured via ADC / the AWS
/// credential chain.
///
/// Port of pi's `getEnvApiKey` (`env-api-keys.ts:137`). `env` supplies variable
/// values; `file_exists` reports whether a path exists (used for the Vertex ADC
/// default-credentials probe). Returns `None` when nothing is configured. Does
/// not return OAuth-only credentials for providers that require them.
pub fn get_env_api_key_with(
    provider: &str,
    env: impl Fn(&str) -> Option<String>,
    file_exists: impl Fn(&str) -> bool,
) -> Option<String> {
    if let Some(keys) = find_env_keys_with(provider, &env) {
        if let Some(first) = keys.first() {
            return read_env(&env, first);
        }
    }

    // Vertex AI: an explicit API key or Application Default Credentials plus a
    // configured project and location.
    if provider == "google-vertex" {
        let has_credentials = has_vertex_adc_credentials(&env, &file_exists);
        let has_project = read_env(&env, "GOOGLE_CLOUD_PROJECT").is_some()
            || read_env(&env, "GCLOUD_PROJECT").is_some();
        let has_location = read_env(&env, "GOOGLE_CLOUD_LOCATION").is_some();
        if has_credentials && has_project && has_location {
            return Some(AMBIENT_SENTINEL.to_string());
        }
    }

    // Amazon Bedrock: any supported AWS credential source marks it configured.
    if provider == "amazon-bedrock" {
        let configured = read_env(&env, "AWS_PROFILE").is_some()
            || (read_env(&env, "AWS_ACCESS_KEY_ID").is_some()
                && read_env(&env, "AWS_SECRET_ACCESS_KEY").is_some())
            || read_env(&env, "AWS_BEARER_TOKEN_BEDROCK").is_some()
            || read_env(&env, "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_some()
            || read_env(&env, "AWS_CONTAINER_CREDENTIALS_FULL_URI").is_some()
            || read_env(&env, "AWS_WEB_IDENTITY_TOKEN_FILE").is_some();
        if configured {
            return Some(AMBIENT_SENTINEL.to_string());
        }
    }

    None
}

/// [`get_env_api_key_with`] against the real process environment and filesystem.
pub fn get_env_api_key(provider: &str) -> Option<String> {
    get_env_api_key_with(
        provider,
        |name| std::env::var(name).ok(),
        |path| std::path::Path::new(path).exists(),
    )
}

/// Whether Vertex ADC credentials are available: an explicit
/// `GOOGLE_APPLICATION_CREDENTIALS` file, or the default gcloud ADC path.
/// Mirrors pi's `hasVertexAdcCredentials` (`env-api-keys.ts:31`).
fn has_vertex_adc_credentials(
    env: &impl Fn(&str) -> Option<String>,
    file_exists: &impl Fn(&str) -> bool,
) -> bool {
    if let Some(path) = read_env(env, "GOOGLE_APPLICATION_CREDENTIALS") {
        return file_exists(&path);
    }
    match std::env::var("HOME").ok() {
        Some(home) if !home.is_empty() => file_exists(&format!(
            "{home}/.config/gcloud/application_default_credentials.json"
        )),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    // env-api-keys.test.ts:36 — generic GitHub tokens are not Copilot credentials.
    #[test]
    fn ignores_generic_github_tokens_for_copilot() {
        let env = env_of(&[("GH_TOKEN", "gh-token"), ("GITHUB_TOKEN", "github-token")]);
        assert_eq!(find_env_keys_with("github-copilot", &env), None);
        assert_eq!(
            get_env_api_key_with("github-copilot", &env, |_| false),
            None
        );
    }

    // env-api-keys.test.ts:45 — Copilot resolves from COPILOT_GITHUB_TOKEN.
    #[test]
    fn resolves_copilot_from_dedicated_token() {
        let env = env_of(&[
            ("COPILOT_GITHUB_TOKEN", "copilot-token"),
            ("GH_TOKEN", "gh-token"),
            ("GITHUB_TOKEN", "github-token"),
        ]);
        assert_eq!(
            find_env_keys_with("github-copilot", &env),
            Some(vec!["COPILOT_GITHUB_TOKEN".to_string()])
        );
        assert_eq!(
            get_env_api_key_with("github-copilot", &env, |_| false),
            Some("copilot-token".to_string())
        );
    }

    // env-api-keys.test.ts:54 — ZAI China Coding Plan from ZAI_CODING_CN_API_KEY.
    #[test]
    fn resolves_zai_coding_cn() {
        let env = env_of(&[("ZAI_CODING_CN_API_KEY", "zai-coding-cn-token")]);
        assert_eq!(
            find_env_keys_with("zai-coding-cn", &env),
            Some(vec!["ZAI_CODING_CN_API_KEY".to_string()])
        );
        assert_eq!(
            get_env_api_key_with("zai-coding-cn", &env, |_| false),
            Some("zai-coding-cn-token".to_string())
        );
    }

    // anthropic OAuth precedence (providers.test.ts:72 relies on this ordering).
    #[test]
    fn anthropic_prefers_oauth_token() {
        assert_eq!(
            get_api_key_env_vars("anthropic"),
            Some(vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"])
        );
        let env = env_of(&[
            ("ANTHROPIC_API_KEY", "key"),
            ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
        ]);
        assert_eq!(
            get_env_api_key_with("anthropic", &env, |_| false),
            Some("oauth-token".to_string())
        );
    }

    // "<authenticated>" sentinel for ambient AWS credentials (amazon-bedrock).
    #[test]
    fn bedrock_ambient_sentinel() {
        let env = env_of(&[("AWS_PROFILE", "dev")]);
        assert_eq!(
            get_env_api_key_with("amazon-bedrock", &env, |_| false),
            Some(AMBIENT_SENTINEL.to_string())
        );
        let empty = env_of(&[]);
        assert_eq!(
            get_env_api_key_with("amazon-bedrock", &empty, |_| false),
            None
        );
    }

    // "<authenticated>" sentinel for Vertex ADC + project + location.
    #[test]
    fn vertex_ambient_sentinel() {
        let adc = "~/.config/gcloud/application_default_credentials.json";
        let env = env_of(&[
            ("GOOGLE_APPLICATION_CREDENTIALS", adc),
            ("GOOGLE_CLOUD_PROJECT", "proj"),
            ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ]);
        assert_eq!(
            get_env_api_key_with("google-vertex", &env, |p| p == adc),
            Some(AMBIENT_SENTINEL.to_string())
        );
        // ADC without location is not configured.
        let partial = env_of(&[
            ("GOOGLE_APPLICATION_CREDENTIALS", adc),
            ("GOOGLE_CLOUD_PROJECT", "proj"),
        ]);
        assert_eq!(
            get_env_api_key_with("google-vertex", &partial, |p| p == adc),
            None
        );
        // Explicit key wins over ADC.
        let keyed = env_of(&[("GOOGLE_CLOUD_API_KEY", "vertex-key")]);
        assert_eq!(
            get_env_api_key_with("google-vertex", &keyed, |_| false),
            Some("vertex-key".to_string())
        );
    }

    #[test]
    fn unknown_provider_has_no_env_vars() {
        assert_eq!(get_api_key_env_vars("does-not-exist"), None);
        assert_eq!(find_env_keys_with("does-not-exist", env_of(&[])), None);
    }
}
