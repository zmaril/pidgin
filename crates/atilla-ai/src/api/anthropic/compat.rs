// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` compat resolution: `getAnthropicCompat` is a wall of
// near-identical `?? <default>` fallbacks and `defaultSupportsToolReferences`
// re-implements pi's version regex by hand. The clone detector reads these
// mirrored fallbacks as duplicates; factoring them would distort the
// byte-faithful port, so the repetition is intentional.
//! Anthropic Messages compat resolution, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` (`getAnthropicCompat`,
//! `defaultSupportsToolReferences`) at pinned commit `3da591ab`.
//!
//! Reads a model's optional `compat` blob and applies pi's per-field defaults,
//! plus the first-party-model heuristic that decides `supportsToolReferences`.

use std::collections::BTreeMap;

use crate::types::{AnthropicMessagesCompat, Model};

/// The fully-resolved compat flags pi's `getAnthropicCompat` returns
/// (`anthropic-messages.ts:171-183`). Every optional `compat` field has been
/// collapsed to a concrete value using pi's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCompat {
    pub supports_eager_tool_input_streaming: bool,
    pub supports_long_cache_retention: bool,
    pub send_session_affinity_headers: bool,
    pub supports_cache_control_on_tools: bool,
    pub supports_temperature: bool,
    pub allow_empty_signature: bool,
    pub supports_tool_references: bool,
}

/// Resolve a model's Anthropic compat flags, mirroring pi's `getAnthropicCompat`
/// (`anthropic-messages.ts:171`).
pub fn get_anthropic_compat(model: &Model<AnthropicMessagesCompat>) -> ResolvedCompat {
    let compat = model.compat.as_ref();
    ResolvedCompat {
        supports_eager_tool_input_streaming: compat
            .and_then(|c| c.supports_eager_tool_input_streaming)
            .unwrap_or(true),
        supports_long_cache_retention: compat
            .and_then(|c| c.supports_long_cache_retention)
            .unwrap_or(true),
        send_session_affinity_headers: compat
            .and_then(|c| c.send_session_affinity_headers)
            .unwrap_or(false),
        supports_cache_control_on_tools: compat
            .and_then(|c| c.supports_cache_control_on_tools)
            .unwrap_or(true),
        supports_temperature: compat.and_then(|c| c.supports_temperature).unwrap_or(true),
        allow_empty_signature: compat
            .and_then(|c| c.allow_empty_signature)
            .unwrap_or(false),
        supports_tool_references: compat
            .and_then(|c| c.supports_tool_references)
            .unwrap_or_else(|| default_supports_tool_references(&model.provider, &model.id)),
    }
}

/// Default for `supportsToolReferences`: first-party Anthropic models except
/// Haiku and models that predate tool search (Claude 3.x, Opus/Sonnet 4.0,
/// Opus 4.1). Mirrors pi's `defaultSupportsToolReferences`
/// (`anthropic-messages.ts:190`), whose regex is
/// `^claude-(?:opus|sonnet|fable)-(\d+)(?:-(\d+))?(?:-|$)`.
pub fn default_supports_tool_references(provider: &str, id: &str) -> bool {
    if provider != "anthropic" || id.contains("haiku") {
        return false;
    }
    let Some((major, minor)) = parse_claude_version(id) else {
        return false;
    };
    major > 4 || (major == 4 && minor >= 5)
}

/// Hand-rolled equivalent of pi's version regex
/// `^claude-(?:opus|sonnet|fable)-(\d+)(?:-(\d+))?(?:-|$)`, returning
/// `(major, minor)` on a match. `minor` follows pi's rule: it is the second
/// numeric group only when that group is shorter than 8 digits (so a bare date
/// suffix like `20250101` reads as minor 0), else 0.
fn parse_claude_version(id: &str) -> Option<(i64, i64)> {
    let rest = id.strip_prefix("claude-")?;
    let after_family = ["opus-", "sonnet-", "fable-"]
        .into_iter()
        .find_map(|family| rest.strip_prefix(family))?;

    let major_len = after_family
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    if major_len == 0 {
        return None;
    }
    let major: i64 = after_family[..major_len].parse().ok()?;
    let tail = &after_family[major_len..];

    // Greedy `(?:-(\d+))?` followed by the required `(?:-|$)`.
    if let Some(after_dash) = tail.strip_prefix('-') {
        let digits = after_dash.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 {
            let minor_str = &after_dash[..digits];
            let remainder = &after_dash[digits..];
            if remainder.is_empty() || remainder.starts_with('-') {
                let minor = if minor_str.len() < 8 {
                    minor_str.parse().unwrap_or(0)
                } else {
                    0
                };
                return Some((major, minor));
            }
        }
    }

    // Minor group not taken: the required `(?:-|$)` must still match at `tail`.
    if tail.is_empty() || tail.starts_with('-') {
        Some((major, 0))
    } else {
        None
    }
}

/// Resolve a provider env value from scoped overrides, then the process
/// environment, mirroring pi's `getProviderEnvValue`
/// (`utils/provider-env.ts`). pi treats empty strings as absent (its `||`
/// chain), so we skip empty values at each layer. The Bun `/proc/self/environ`
/// fallback is Bun-specific and does not apply here.
pub fn get_provider_env_value(
    name: &str,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    if let Some(value) = env.and_then(|e| e.get(name)) {
        if !value.is_empty() {
            return Some(value.clone());
        }
    }
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}
