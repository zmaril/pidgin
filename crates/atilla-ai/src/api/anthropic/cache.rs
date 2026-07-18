// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `anthropic-messages.ts` cache helpers (`resolveCacheRetention`,
// `getCacheControl`). The clone detector may read the small option/serde
// scaffolding as duplicative; it is kept verbatim to mirror pi exactly.
//! Prompt-cache control resolution, ported from pi-ai's
//! `packages/ai/src/api/anthropic-messages.ts` (`resolveCacheRetention`,
//! `getCacheControl`) at pinned commit `3da591ab`.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::types::{AnthropicMessagesCompat, CacheRetention, Model};

use super::compat::{get_anthropic_compat, get_provider_env_value};

/// Resolve the effective cache retention, mirroring pi's `resolveCacheRetention`
/// (`anthropic-messages.ts:47`). Defaults to `short`, with `PI_CACHE_RETENTION`
/// as a backward-compatible override to `long`.
pub fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&BTreeMap<String, String>>,
) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// The resolved retention plus the `cache_control` block to stamp onto cached
/// content, mirroring pi's `getCacheControl` (`anthropic-messages.ts:57`).
/// `cache_control` is `None` when retention is `none`; otherwise it is
/// `{ "type": "ephemeral" }`, gaining `"ttl": "1h"` only for `long` retention on
/// a model that supports long cache retention.
pub fn get_cache_control(
    model: &Model<AnthropicMessagesCompat>,
    cache_retention: Option<CacheRetention>,
    env: Option<&BTreeMap<String, String>>,
) -> (CacheRetention, Option<Value>) {
    let retention = resolve_cache_retention(cache_retention, env);
    if retention == CacheRetention::None {
        return (retention, None);
    }
    let use_long_ttl = retention == CacheRetention::Long
        && get_anthropic_compat(model).supports_long_cache_retention;
    let cache_control = if use_long_ttl {
        json!({ "type": "ephemeral", "ttl": "1h" })
    } else {
        json!({ "type": "ephemeral" })
    };
    (retention, Some(cache_control))
}
