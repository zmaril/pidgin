//! OpenAI `prompt_cache_key` clamping, ported from pi-ai's
//! `packages/ai/src/api/openai-prompt-cache.ts` at pinned commit `3da591ab`.
//!
//! OpenAI documents a 64-character cap on `prompt_cache_key`; pi clamps longer
//! keys by taking the first 64 code points. This is the single shared home for
//! that logic — the OpenAI Responses, OpenAI Completions, and Azure OpenAI
//! Responses drivers all call into it.

/// OpenAI's documented 64-character cap on `prompt_cache_key`
/// (pi's `OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH`).
pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// Clamp a prompt cache key to OpenAI's 64-character limit (pi's
/// `clampOpenAIPromptCacheKey`). Counts and slices by Unicode scalar, mirroring
/// pi's `Array.from(key)` code-point iteration.
pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        Some(key.to_string())
    } else {
        Some(
            key.chars()
                .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
                .collect(),
        )
    }
}
