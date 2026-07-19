// Native shim for packages/agent/src/harness/system-prompt.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `system-prompt.__pi_original__.ts` and this
// shim takes its place, so pi's `formatSkillsForSystemPrompt` hits Rust.
//
// Scope of the native flip: `formatSkillsForSystemPrompt`, ported to
// `atilla_agent::harness::system_prompt`. pi's `Skill[]` crosses as a JSON
// array (serialized here, parsed with serde in Rust); the string result is
// returned unchanged. Everything else in the module (there is nothing else
// exported) is re-exported from the preserved original.

export * from "./system-prompt.__pi_original__.ts";

import { formatSkillsForSystemPrompt as nativeFormatSkillsForSystemPrompt } from "atilla-napi";
import type { Skill } from "./types.ts";

export function formatSkillsForSystemPrompt(skills: Skill[]): string {
	return nativeFormatSkillsForSystemPrompt(JSON.stringify(skills));
}
