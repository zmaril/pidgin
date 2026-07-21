//! Node-API exports for the agent tier (`crates/pidgin-agent`), backing the
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
// Thin wrapper over `pidgin_agent::harness::system_prompt`, backing the native
// `harness/system-prompt.ts` shim. pi's `Skill[]` crosses as a JSON array; the
// `Skill` struct derives `serde` with camelCase field names matching pi's
// `Skill` (`filePath`, optional `disableModelInvocation`).

/// `formatSkillsForSystemPrompt` (harness/system-prompt.ts): render the
/// model-visible `<available_skills>` block from pi's `Skill[]`. The shim
/// serializes the array to JSON; this parses it and returns the string.
#[napi(js_name = "formatSkillsForSystemPrompt")]
pub fn format_skills_for_system_prompt(skills_json: String) -> napi::Result<String> {
    use pidgin_agent::harness::skills::Skill;
    let skills: Vec<Skill> = serde_json::from_str(&skills_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid skills array: {e}")))?;
    Ok(pidgin_agent::harness::system_prompt::format_skills_for_system_prompt(&skills))
}

// --- agent harness: skills / prompt-templates formatting --------------------
//
// Pure, synchronous formatters ported to `pidgin_agent::harness::skills` and
// `pidgin_agent::harness::prompt_templates`, backing the native `harness/
// skills.ts` and `harness/prompt-templates.ts` shims. pi's rich `Skill` /
// `PromptTemplate` cross as JSON strings (serialized in the shim, parsed with
// serde here); the string result is returned unchanged. The stateful loaders
// (`loadSkills`/`loadPromptTemplates`) are methods on the fluessig-generated
// `NodeExecutionEnvCore` handle (see `crate::generated` + `crate::core_impl`),
// so they borrow the same host-backed Rust env the shim already holds for
// `nodejs.ts`.

/// `formatSkillInvocation` (harness/skills.ts): render a `<skill>` invocation
/// block, optionally appending user instructions. pi's `Skill` crosses as a
/// JSON object; `additionalInstructions` is an optional string.
#[napi(js_name = "formatSkillInvocation")]
pub fn format_skill_invocation(
    skill_json: String,
    additional_instructions: Option<String>,
) -> napi::Result<String> {
    use pidgin_agent::harness::skills::{format_skill_invocation, Skill};
    let skill: Skill = serde_json::from_str(&skill_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid skill: {e}")))?;
    Ok(format_skill_invocation(
        &skill,
        additional_instructions.as_deref(),
    ))
}

/// `formatPromptTemplateInvocation` (harness/prompt-templates.ts): substitute
/// positional arguments into a template's content. pi's `PromptTemplate`
/// crosses as a JSON object; the argument list crosses as a string array.
#[napi(js_name = "formatPromptTemplateInvocation")]
pub fn format_prompt_template_invocation(
    template_json: String,
    args: Vec<String>,
) -> napi::Result<String> {
    use pidgin_agent::harness::prompt_templates::{
        format_prompt_template_invocation, PromptTemplate,
    };
    let template: PromptTemplate = serde_json::from_str(&template_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid prompt template: {e}")))?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(format_prompt_template_invocation(&template, &refs))
}
