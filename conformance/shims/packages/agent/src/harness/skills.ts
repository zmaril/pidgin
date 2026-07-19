// Native shim for packages/agent/src/harness/skills.ts, backed by the pidgin
// Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `skills.__pi_original__.ts` and this shim takes its
// place, so pi's tests hit the Rust-backed skill loading + formatting.
//
// Scope of the native flip:
//   - `formatSkillInvocation` (pure, sync): pi's `Skill` crosses as a JSON
//     object; the string result is returned unchanged. pi's empty-string
//     "no additional instructions" truthiness is preserved by passing
//     `undefined` for a falsy argument.
//   - `loadSkills` (async surface over the sync native loader): the loader
//     reads files through the SAME host-backed Rust env the passed
//     `NodeExecutionEnv` already holds, reached via `nativeExecutionCore`. The
//     `{skills,diagnostics}` result crosses as a JSON string and is parsed here.
//     If the passed env is not our `NodeExecutionEnv` (e.g. a
//     `MemoryExecutionEnv`), the call delegates to pi's original async loader.
//   - `loadSourcedSkills` (async): composed in JS over `loadSkills`, exactly as
//     pi does, so the opaque `source` value and optional `mapSkill` mapper never
//     cross the native boundary.
// Everything else (the `SkillDiagnostic*` types) is re-exported from the
// preserved original.

export * from "./skills.__pi_original__.ts";

import { formatSkillInvocation as nativeFormatSkillInvocation } from "pidgin-napi";
import { nativeExecutionCore } from "./env/nodejs.ts";
import { loadSkills as piLoadSkills } from "./skills.__pi_original__.ts";
import type { SkillDiagnostic } from "./skills.__pi_original__.ts";
import type { ExecutionEnv, Skill } from "./types.ts";

/** Format a skill invocation prompt, optionally appending additional user instructions. */
export function formatSkillInvocation(skill: Skill, additionalInstructions?: string): string {
	// pi treats an empty string as "no additional instructions" (`x ? ... : ...`);
	// pass `undefined` so the native formatter does not append an empty block.
	return nativeFormatSkillInvocation(JSON.stringify(skill), additionalInstructions ? additionalInstructions : undefined);
}

/**
 * Load skills from one or more directories.
 *
 * Routes to the Rust loader when `env` is our native `NodeExecutionEnv`;
 * otherwise delegates to pi's original async loader.
 */
export async function loadSkills(
	env: ExecutionEnv,
	dirs: string | string[],
): Promise<{ skills: Skill[]; diagnostics: SkillDiagnostic[] }> {
	const core = nativeExecutionCore(env);
	if (!core) return piLoadSkills(env, dirs);
	const dirArray = Array.isArray(dirs) ? dirs : [dirs];
	return JSON.parse(core.loadSkills(dirArray)) as { skills: Skill[]; diagnostics: SkillDiagnostic[] };
}

/**
 * Load skills from source-tagged directories.
 *
 * Composed over {@link loadSkills} in JS, mirroring pi: the `source` value and
 * optional `mapSkill` mapper stay on the JS side and never cross to Rust.
 */
export async function loadSourcedSkills<TSource, TSkill extends Skill = Skill>(
	env: ExecutionEnv,
	inputs: Array<{ path: string; source: TSource }>,
	mapSkill?: (skill: Skill, source: TSource) => TSkill,
): Promise<{
	skills: Array<{ skill: TSkill; source: TSource }>;
	diagnostics: Array<SkillDiagnostic & { source: TSource }>;
}> {
	const skills: Array<{ skill: TSkill; source: TSource }> = [];
	const diagnostics: Array<SkillDiagnostic & { source: TSource }> = [];
	for (const input of inputs) {
		const result = await loadSkills(env, input.path);
		for (const skill of result.skills) {
			skills.push({ skill: mapSkill ? mapSkill(skill, input.source) : (skill as TSkill), source: input.source });
		}
		for (const diagnostic of result.diagnostics) diagnostics.push({ ...diagnostic, source: input.source });
	}
	return { skills, diagnostics };
}
