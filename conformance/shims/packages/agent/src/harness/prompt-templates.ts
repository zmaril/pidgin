// Native shim for packages/agent/src/harness/prompt-templates.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `prompt-templates.__pi_original__.ts` and this
// shim takes its place, so pi's tests hit the Rust-backed loading + formatting.
//
// Scope of the native flip:
//   - `parseCommandArgs`, `substituteArgs`, `formatPromptTemplateInvocation`
//     (pure, sync): pi's `PromptTemplate` crosses as a JSON object; argument
//     lists cross as string arrays; results are returned unchanged. pi's
//     `args = []` default is re-added here.
//   - `loadPromptTemplates` (async surface over the sync native loader): reads
//     through the SAME host-backed Rust env the passed `NodeExecutionEnv`
//     already holds, reached via `nativeExecutionCore`; the
//     `{promptTemplates,diagnostics}` result crosses as a JSON string and is
//     parsed here. If the passed env is not our `NodeExecutionEnv`, the call
//     delegates to pi's original async loader.
//   - `loadSourcedPromptTemplates` (async): composed in JS over
//     `loadPromptTemplates`, exactly as pi does, so the opaque `source` value
//     and optional `mapPromptTemplate` mapper never cross the native boundary.
// Everything else (the `PromptTemplateDiagnostic*` types) is re-exported from
// the preserved original.

export * from "./prompt-templates.__pi_original__.ts";

import {
	formatPromptTemplateInvocation as nativeFormatPromptTemplateInvocation,
	parseCommandArgs as nativeParseCommandArgs,
	substituteArgs as nativeSubstituteArgs,
} from "pidgin-napi";
import { nativeExecutionCore } from "./env/nodejs.ts";
import { loadPromptTemplates as piLoadPromptTemplates } from "./prompt-templates.__pi_original__.ts";
import type { PromptTemplateDiagnostic } from "./prompt-templates.__pi_original__.ts";
import type { ExecutionEnv, PromptTemplate } from "./types.ts";

/** Parse an argument string using simple shell-style single and double quotes. */
export function parseCommandArgs(argsString: string): string[] {
	return nativeParseCommandArgs(argsString);
}

/** Substitute prompt template placeholders (`$1`, `$@`, `$ARGUMENTS`, `${@:N}`, `${@:N:L}`) with command arguments. */
export function substituteArgs(content: string, args: string[]): string {
	return nativeSubstituteArgs(content, args);
}

/** Format a prompt template invocation with positional arguments. */
export function formatPromptTemplateInvocation(template: PromptTemplate, args: string[] = []): string {
	return nativeFormatPromptTemplateInvocation(JSON.stringify(template), args);
}

/**
 * Load prompt templates from one or more paths.
 *
 * Routes to the Rust loader when `env` is our native `NodeExecutionEnv`;
 * otherwise delegates to pi's original async loader.
 */
export async function loadPromptTemplates(
	env: ExecutionEnv,
	paths: string | string[],
): Promise<{ promptTemplates: PromptTemplate[]; diagnostics: PromptTemplateDiagnostic[] }> {
	const core = nativeExecutionCore(env);
	if (!core) return piLoadPromptTemplates(env, paths);
	const pathArray = Array.isArray(paths) ? paths : [paths];
	return JSON.parse(core.loadPromptTemplates(pathArray)) as {
		promptTemplates: PromptTemplate[];
		diagnostics: PromptTemplateDiagnostic[];
	};
}

/**
 * Load prompt templates from source-tagged paths.
 *
 * Composed over {@link loadPromptTemplates} in JS, mirroring pi: the `source`
 * value and optional `mapPromptTemplate` mapper stay on the JS side and never
 * cross to Rust.
 */
export async function loadSourcedPromptTemplates<TSource, TPromptTemplate extends PromptTemplate = PromptTemplate>(
	env: ExecutionEnv,
	inputs: Array<{ path: string; source: TSource }>,
	mapPromptTemplate?: (promptTemplate: PromptTemplate, source: TSource) => TPromptTemplate,
): Promise<{
	promptTemplates: Array<{ promptTemplate: TPromptTemplate; source: TSource }>;
	diagnostics: Array<PromptTemplateDiagnostic & { source: TSource }>;
}> {
	const promptTemplates: Array<{ promptTemplate: TPromptTemplate; source: TSource }> = [];
	const diagnostics: Array<PromptTemplateDiagnostic & { source: TSource }> = [];
	for (const input of inputs) {
		const result = await loadPromptTemplates(env, input.path);
		for (const promptTemplate of result.promptTemplates) {
			promptTemplates.push({
				promptTemplate: mapPromptTemplate
					? mapPromptTemplate(promptTemplate, input.source)
					: (promptTemplate as TPromptTemplate),
				source: input.source,
			});
		}
		for (const diagnostic of result.diagnostics) diagnostics.push({ ...diagnostic, source: input.source });
	}
	return { promptTemplates, diagnostics };
}
