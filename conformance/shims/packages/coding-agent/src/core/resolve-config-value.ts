// Native shim for packages/coding-agent/src/core/resolve-config-value.ts, backed
// by the atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs
// when the module is marked `native` in conformance/manifest.json: the original
// pi file is preserved alongside as `resolve-config-value.__pi_original__.ts` and
// this shim takes its place, so pi's callers (auth-storage, model-registry) and
// test/resolve-config-value.test.ts import `../src/core/resolve-config-value.ts`
// unchanged and hit Rust.
//
// Scope of the native flip: every ported symbol, backed by
// `atilla_coding::core::resolve_config_value`. The Rust port owns the literal /
// `$ENV` template / `!command` parsing, the process-lifetime command result
// cache, and the subprocess execution (default `sh -c`, pi's Unix path). pi's
// optional credential-scoped `env` override crosses as a JSON object string; the
// process environment is read by Rust directly (`std::env::var`), matching pi's
// `env?.[name] || process.env[name]`. Rust returns `None` for "unresolved",
// which this shim maps back to pi's `undefined`. `resolveConfigValueOrThrow` /
// `resolveHeadersOrThrow` throw pi's exact messages (the addon maps `Err` to a
// thrown JS `Error`).

export * from "./resolve-config-value.__pi_original__.ts";

import {
	clearConfigValueCache as nativeClearConfigValueCache,
	getConfigValueEnvVarName as nativeGetConfigValueEnvVarName,
	getConfigValueEnvVarNames as nativeGetConfigValueEnvVarNames,
	getMissingConfigValueEnvVarNames as nativeGetMissingConfigValueEnvVarNames,
	isCommandConfigValue as nativeIsCommandConfigValue,
	isConfigValueConfigured as nativeIsConfigValueConfigured,
	resolveConfigValue as nativeResolveConfigValue,
	resolveConfigValueOrThrow as nativeResolveConfigValueOrThrow,
	resolveConfigValueUncached as nativeResolveConfigValueUncached,
	resolveHeaders as nativeResolveHeaders,
	resolveHeadersOrThrow as nativeResolveHeadersOrThrow,
} from "atilla-napi";

/** Serialize pi's optional `env` override for the JSON boundary. */
function envJson(env?: Record<string, string>): string | undefined {
	return env === undefined ? undefined : JSON.stringify(env);
}

export function resolveConfigValue(config: string, env?: Record<string, string>): string | undefined {
	const resolved = nativeResolveConfigValue(config, envJson(env));
	return resolved === null ? undefined : resolved;
}

export function resolveConfigValueUncached(config: string, env?: Record<string, string>): string | undefined {
	const resolved = nativeResolveConfigValueUncached(config, envJson(env));
	return resolved === null ? undefined : resolved;
}

export function resolveConfigValueOrThrow(config: string, description: string, env?: Record<string, string>): string {
	return nativeResolveConfigValueOrThrow(config, description, envJson(env));
}

export function getConfigValueEnvVarName(config: string): string | undefined {
	const name = nativeGetConfigValueEnvVarName(config);
	return name === null ? undefined : name;
}

export function getConfigValueEnvVarNames(config: string): string[] {
	return nativeGetConfigValueEnvVarNames(config);
}

export function getMissingConfigValueEnvVarNames(config: string, env?: Record<string, string>): string[] {
	return nativeGetMissingConfigValueEnvVarNames(config, envJson(env));
}

export function isCommandConfigValue(config: string): boolean {
	return nativeIsCommandConfigValue(config);
}

export function isConfigValueConfigured(config: string, env?: Record<string, string>): boolean {
	return nativeIsConfigValueConfigured(config, envJson(env));
}

export function resolveHeaders(
	headers: Record<string, string> | undefined,
	env?: Record<string, string>,
): Record<string, string> | undefined {
	const resolved = nativeResolveHeaders(headers === undefined ? undefined : JSON.stringify(headers), envJson(env));
	return resolved === null ? undefined : (JSON.parse(resolved) as Record<string, string>);
}

export function resolveHeadersOrThrow(
	headers: Record<string, string> | undefined,
	description: string,
	env?: Record<string, string>,
): Record<string, string> | undefined {
	const resolved = nativeResolveHeadersOrThrow(
		headers === undefined ? undefined : JSON.stringify(headers),
		description,
		envJson(env),
	);
	return resolved === null ? undefined : (JSON.parse(resolved) as Record<string, string>);
}

export function clearConfigValueCache(): void {
	nativeClearConfigValueCache();
}
