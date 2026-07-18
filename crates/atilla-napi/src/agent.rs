//! Node-API exports for the agent tier (`crates/atilla-agent`), backing the
//! native `packages/agent/**` shims. Kept in its own module so the agent-tier
//! flips stay merge-clean beside the coding-agent/ai exports in `lib.rs`.
//!
//! As elsewhere in this crate, rich pi structures cross the boundary as JSON
//! strings: the shim `JSON.parse`s the result and re-adds pi's JS default
//! arguments (which the Rust ports drop). The JS-facing types come from pi's own
//! preserved `*.__pi_original__.ts`.

use napi_derive::napi;

// --- agent harness: system-prompt ------------------------------------------
//
// Thin wrapper over `atilla_agent::harness::system_prompt`, backing the native
// `harness/system-prompt.ts` shim. pi's `Skill[]` crosses as a JSON array; the
// `Skill` struct derives `serde` with camelCase field names matching pi's
// `Skill` (`filePath`, optional `disableModelInvocation`).

/// `formatSkillsForSystemPrompt` (harness/system-prompt.ts): render the
/// model-visible `<available_skills>` block from pi's `Skill[]`. The shim
/// serializes the array to JSON; this parses it and returns the string.
#[napi(js_name = "formatSkillsForSystemPrompt")]
pub fn format_skills_for_system_prompt(skills_json: String) -> napi::Result<String> {
    use atilla_agent::harness::skills::Skill;
    let skills: Vec<Skill> = serde_json::from_str(&skills_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid skills array: {e}")))?;
    Ok(atilla_agent::harness::system_prompt::format_skills_for_system_prompt(&skills))
}
