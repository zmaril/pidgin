// straitjacket-allow-file:duplication — the private `loadRawConfig` /
// `toKeybindingsConfig` glue and the `KeybindingsManager` overrides below mirror
// pi's originals line for line, because those symbols are file-private in pi and
// cannot be re-exported; the shim must rebuild them around the native default
// table and migration, so the structural overlap is intentional.
//
// Native shim for packages/coding-agent/src/core/keybindings.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `keybindings.__pi_original__.ts` and this shim
// takes its place, so pi's app and test/keybindings-migration.test.ts import
// `../src/core/keybindings.ts` unchanged and hit Rust.
//
// Scope of the native flip: the coding-agent module's own contributions — the
// default keybinding table (`KEYBINDINGS`, incl. the `app.*` actions and their
// platform-dependent defaults) and the legacy-name migration
// (`migrateKeybindingsConfig`, which `migrations.ts` calls to rewrite
// `keybindings.json`) — are backed by `atilla_coding::core::keybindings`. The
// resolution engine and `matches()` live in pi-tui's base `KeybindingsManager`,
// a SEPARATE, still-original module (its own manifest row); this shim keeps
// extending that base (so `matches()`, conflict detection, and `instanceof`
// stay pi-tui's exact behavior) and only swaps in the native default table and
// migration. camelCase field names (`defaultKeys`) cross verbatim in the native
// JSON.

export * from "./keybindings.__pi_original__.ts";

import {
	type Keybinding,
	type KeybindingDefinitions,
	type KeybindingsConfig,
	type KeyId,
	KeybindingsManager as TuiKeybindingsManager,
} from "@earendil-works/pi-tui";
import { existsSync, readFileSync } from "fs";
import { join } from "path";
import {
	keybindingsFor as nativeKeybindingsFor,
	migrateKeybindingsConfig as nativeMigrateKeybindingsConfig,
} from "atilla-napi";
import { getAgentDir } from "../config.ts";

/** The default keybinding table for the current platform, built by the Rust port. */
export const KEYBINDINGS = JSON.parse(nativeKeybindingsFor(process.platform)) as KeybindingDefinitions;

export function migrateKeybindingsConfig(rawConfig: Record<string, unknown>): {
	config: Record<string, unknown>;
	migrated: boolean;
} {
	return JSON.parse(nativeMigrateKeybindingsConfig(JSON.stringify(rawConfig))) as {
		config: Record<string, unknown>;
		migrated: boolean;
	};
}

function toKeybindingsConfig(value: Record<string, unknown>): KeybindingsConfig {
	const config: KeybindingsConfig = {};
	for (const [key, binding] of Object.entries(value)) {
		if (typeof binding === "string") {
			config[key] = binding as KeyId;
			continue;
		}
		if (Array.isArray(binding) && binding.every((entry) => typeof entry === "string")) {
			config[key] = binding as KeyId[];
		}
	}
	return config;
}

function loadRawConfig(path: string): Record<string, unknown> | undefined {
	if (!existsSync(path)) return undefined;
	try {
		const parsed = JSON.parse(readFileSync(path, "utf-8")) as unknown;
		if (typeof parsed !== "object" || parsed === null) return undefined;
		return parsed as Record<string, unknown>;
	} catch {
		return undefined;
	}
}

export class KeybindingsManager extends TuiKeybindingsManager {
	private configPath: string | undefined;

	constructor(userBindings: KeybindingsConfig = {}, configPath?: string) {
		super(KEYBINDINGS, userBindings);
		this.configPath = configPath;
	}

	static create(agentDir: string = getAgentDir()): KeybindingsManager {
		const configPath = join(agentDir, "keybindings.json");
		const userBindings = KeybindingsManager.loadFromFile(configPath);
		return new KeybindingsManager(userBindings, configPath);
	}

	reload(): void {
		if (!this.configPath) return;
		this.setUserBindings(KeybindingsManager.loadFromFile(this.configPath));
	}

	getEffectiveConfig(): KeybindingsConfig {
		return this.getResolvedBindings();
	}

	private static loadFromFile(path: string): KeybindingsConfig {
		const rawConfig = loadRawConfig(path);
		if (!rawConfig) return {};
		return toKeybindingsConfig(migrateKeybindingsConfig(rawConfig).config);
	}
}

export type { Keybinding, KeyId, KeybindingsConfig };
