//! Provider attribution header injection.
//!
//! Ported from pi's `core/provider-attribution.ts`. Pure header computation: it
//! matches a model against a small set of known hosts (by provider name or URL
//! hostname) and layers attribution/session headers.
//!
//! pi threads a `SettingsManager` through to read the install-telemetry flag;
//! the pure port takes the resolved `telemetry_enabled: bool` and
//! `session_id: Option<&str>` directly, so the caller owns settings/telemetry
//! resolution.

use std::collections::BTreeMap;

use pidgin_ai::types::Model;

/// A set of provider request headers (`ProviderHeaders` in pi-ai).
pub type ProviderHeaders = BTreeMap<String, String>;

const OPENROUTER_HOST: &str = "openrouter.ai";
const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const CLOUDFLARE_API_HOST: &str = "api.cloudflare.com";
const CLOUDFLARE_AI_GATEWAY_HOST: &str = "gateway.ai.cloudflare.com";
const OPENCODE_HOST: &str = "opencode.ai";

/// Extract the lowercased hostname from an absolute URL, mirroring
/// `new URL(baseUrl).hostname`. Returns `None` when the URL has no scheme or
/// host (matching pi's `try/catch` around the `URL` constructor).
fn url_hostname(base_url: &str) -> Option<String> {
    let scheme_end = base_url.find("://")?;
    let after = &base_url[scheme_end + 3..];
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..authority_end];
    // Strip any `userinfo@` prefix.
    let host_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        // IPv6 literal: take up to the closing bracket.
        &rest[..rest.find(']')?]
    } else {
        match host_port.find(':') {
            Some(colon) => &host_port[..colon],
            None => host_port,
        }
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

/// Whether `base_url`'s hostname equals `expected_host` (`provider-attribution.ts:11`).
fn matches_host(base_url: &str, expected_host: &str) -> bool {
    url_hostname(base_url).as_deref() == Some(expected_host)
}

fn is_openrouter_model(model: &Model) -> bool {
    model.provider == "openrouter" || model.base_url.contains(OPENROUTER_HOST)
}

fn is_nvidia_nim_model(model: &Model) -> bool {
    model.provider == "nvidia" || matches_host(&model.base_url, NVIDIA_NIM_HOST)
}

fn is_cloudflare_model(model: &Model) -> bool {
    model.provider == "cloudflare-workers-ai"
        || model.provider == "cloudflare-ai-gateway"
        || matches_host(&model.base_url, CLOUDFLARE_API_HOST)
        || matches_host(&model.base_url, CLOUDFLARE_AI_GATEWAY_HOST)
}

fn headers_from(pairs: &[(&str, &str)]) -> ProviderHeaders {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// Default attribution headers for known gateways (`provider-attribution.ts:36`).
///
/// Returns `None` when install telemetry is disabled or the model matches no
/// known host.
fn default_attribution_headers(model: &Model, telemetry_enabled: bool) -> Option<ProviderHeaders> {
    if !telemetry_enabled {
        return None;
    }
    if is_openrouter_model(model) {
        return Some(headers_from(&[
            ("HTTP-Referer", "https://pi.dev"),
            ("X-OpenRouter-Title", "pi"),
            ("X-OpenRouter-Categories", "cli-agent"),
        ]));
    }
    if is_nvidia_nim_model(model) {
        return Some(headers_from(&[("X-BILLING-INVOKE-ORIGIN", "Pi")]));
    }
    if is_cloudflare_model(model) {
        return Some(headers_from(&[("User-Agent", "pi-coding-agent")]));
    }
    None
}

/// Session-affinity headers for opencode hosts (`provider-attribution.ts:67`).
fn session_headers(model: &Model, session_id: Option<&str>) -> Option<ProviderHeaders> {
    let session_id = session_id?;
    if model.provider != "opencode"
        && model.provider != "opencode-go"
        && !matches_host(&model.base_url, OPENCODE_HOST)
    {
        return None;
    }
    Some(headers_from(&[
        ("x-opencode-session", session_id),
        ("x-opencode-client", "pi"),
    ]))
}

/// Merge session + attribution headers with caller-supplied sources
/// (`provider-attribution.ts:79`).
///
/// Layering order (later wins on key conflicts): session headers, then default
/// attribution headers, then each entry of `header_sources` in order. `None`
/// entries in `header_sources` are skipped. Returns `None` when the merged set
/// is empty.
pub fn merge_provider_attribution_headers(
    model: &Model,
    telemetry_enabled: bool,
    session_id: Option<&str>,
    header_sources: &[Option<ProviderHeaders>],
) -> Option<ProviderHeaders> {
    let mut merged = ProviderHeaders::new();
    if let Some(headers) = session_headers(model, session_id) {
        merged.extend(headers);
    }
    if let Some(headers) = default_attribution_headers(model, telemetry_enabled) {
        merged.extend(headers);
    }
    for source in header_sources.iter().flatten() {
        for (key, value) in source {
            merged.insert(key.clone(), value.clone());
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_model;

    /// Build a model with the two fields attribution reads (`provider`, `base_url`).
    fn model(provider: &str, base_url: &str) -> Model {
        let mut m = test_model("m", provider);
        m.base_url = base_url.to_string();
        m
    }

    #[test]
    fn url_hostname_extraction() {
        assert_eq!(
            url_hostname("https://openrouter.ai/api/v1").as_deref(),
            Some("openrouter.ai")
        );
        assert_eq!(
            url_hostname("https://USER:pw@integrate.api.nvidia.com:443/v1").as_deref(),
            Some("integrate.api.nvidia.com")
        );
        assert_eq!(
            url_hostname("https://EXAMPLE.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(url_hostname("not a url"), None);
        assert_eq!(
            url_hostname("http://[2001:db8::1]:8080/x").as_deref(),
            Some("2001:db8::1")
        );
    }

    #[test]
    fn openrouter_headers_by_provider_and_host() {
        let by_provider =
            default_attribution_headers(&model("openrouter", "https://example.invalid"), true)
                .unwrap();
        assert_eq!(
            by_provider.get("X-OpenRouter-Title").map(String::as_str),
            Some("pi")
        );
        // Host substring match also triggers, even for another provider name.
        let by_host =
            default_attribution_headers(&model("custom", "https://openrouter.ai/api/v1"), true)
                .unwrap();
        assert_eq!(
            by_host.get("HTTP-Referer").map(String::as_str),
            Some("https://pi.dev")
        );
    }

    #[test]
    fn nvidia_and_cloudflare_and_none() {
        let nvidia =
            default_attribution_headers(&model("x", "https://integrate.api.nvidia.com/v1"), true)
                .unwrap();
        assert_eq!(
            nvidia.get("X-BILLING-INVOKE-ORIGIN").map(String::as_str),
            Some("Pi")
        );
        let cf =
            default_attribution_headers(&model("cloudflare-workers-ai", "https://x.invalid"), true)
                .unwrap();
        assert_eq!(
            cf.get("User-Agent").map(String::as_str),
            Some("pi-coding-agent")
        );
        assert!(default_attribution_headers(
            &model("anthropic", "https://api.anthropic.com"),
            true
        )
        .is_none());
    }

    #[test]
    fn telemetry_disabled_suppresses_attribution() {
        assert!(
            default_attribution_headers(&model("openrouter", "https://openrouter.ai"), false)
                .is_none()
        );
    }

    #[test]
    fn session_headers_only_for_opencode() {
        let hs = session_headers(&model("opencode", "https://x.invalid"), Some("sess-1")).unwrap();
        assert_eq!(
            hs.get("x-opencode-session").map(String::as_str),
            Some("sess-1")
        );
        assert_eq!(hs.get("x-opencode-client").map(String::as_str), Some("pi"));
        assert!(
            session_headers(&model("openai", "https://api.openai.com"), Some("sess-1")).is_none()
        );
        assert!(session_headers(&model("opencode", "https://x.invalid"), None).is_none());
    }

    #[test]
    fn merge_layers_and_later_sources_win() {
        let extra: ProviderHeaders = [("X-OpenRouter-Title".to_string(), "override".to_string())]
            .into_iter()
            .collect();
        let merged = merge_provider_attribution_headers(
            &model("openrouter", "https://openrouter.ai"),
            true,
            None,
            &[Some(extra)],
        )
        .unwrap();
        // Later source overrides the default attribution value.
        assert_eq!(
            merged.get("X-OpenRouter-Title").map(String::as_str),
            Some("override")
        );
        assert_eq!(
            merged.get("HTTP-Referer").map(String::as_str),
            Some("https://pi.dev")
        );
    }

    #[test]
    fn merge_returns_none_when_empty() {
        assert!(merge_provider_attribution_headers(
            &model("anthropic", "https://api.anthropic.com"),
            true,
            None,
            &[None],
        )
        .is_none());
    }
}
