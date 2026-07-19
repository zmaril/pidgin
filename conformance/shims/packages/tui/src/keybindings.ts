// Native shim for packages/tui/src/keybindings.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `keybindings.__pi_original__.ts` and this shim takes
// its place, so pi's tests import `../src/keybindings.ts` unchanged and hit
// Rust.
//
// Scope of the native flip: the `KeybindingsManager` resolution logic ported
// bit-exactly in `crates/pidgin-tui` (validated against pi's keybindings.test.ts)
// — the default-vs-user binding merge, conflict detection, `matches` (via the
// native key parser), `getKeys`, and `getResolvedBindings`. This shim
// re-implements pi's `KeybindingsManager` class, keeping `definitions` /
// `userBindings` / `getDefinition` / `getUserBindings` as JS (identical to pi)
// and routing the resolution through the native `KeybindingsManagerCore`. The
// core is immutable per construction, so `setUserBindings` builds a fresh core.
// Definitions and user bindings cross as ordered JSON arrays to preserve JS
// object insertion order. `TUI_KEYBINDINGS`, `setKeybindings`, `getKeybindings`,
// and all types are re-exported from the original unchanged.

export * from "./keybindings.__pi_original__.ts";

import { KeybindingsManagerCore } from "pidgin-napi";
import type { KeyId } from "./keys.ts";
import type {
	Keybinding,
	KeybindingConflict,
	KeybindingDefinition,
	KeybindingDefinitions,
	KeybindingsConfig,
} from "./keybindings.__pi_original__.ts";

function buildCore(definitions: KeybindingDefinitions, userBindings: KeybindingsConfig): KeybindingsManagerCore {
	const defs = Object.entries(definitions).map(([id, def]) => ({
		id,
		defaultKeys: Array.isArray(def.defaultKeys) ? def.defaultKeys : [def.defaultKeys],
		description: def.description ?? null,
	}));
	const user = Object.entries(userBindings).map(([id, keys]) => ({
		id,
		keys: keys === undefined ? null : Array.isArray(keys) ? keys : [keys],
	}));
	return new KeybindingsManagerCore(JSON.stringify(defs), JSON.stringify(user));
}

export class KeybindingsManager {
	private definitions: KeybindingDefinitions;
	private userBindings: KeybindingsConfig;
	private core: KeybindingsManagerCore;

	constructor(definitions: KeybindingDefinitions, userBindings: KeybindingsConfig = {}) {
		this.definitions = definitions;
		this.userBindings = userBindings;
		this.core = buildCore(definitions, userBindings);
	}

	matches(data: string, keybinding: Keybinding): boolean {
		return this.core.matches(data, keybinding);
	}

	getKeys(keybinding: Keybinding): KeyId[] {
		return this.core.getKeys(keybinding) as KeyId[];
	}

	getDefinition(keybinding: Keybinding): KeybindingDefinition {
		return this.definitions[keybinding];
	}

	getConflicts(): KeybindingConflict[] {
		return JSON.parse(this.core.getConflictsJson()) as KeybindingConflict[];
	}

	setUserBindings(userBindings: KeybindingsConfig): void {
		this.userBindings = userBindings;
		this.core = buildCore(this.definitions, userBindings);
	}

	getUserBindings(): KeybindingsConfig {
		return { ...this.userBindings };
	}

	getResolvedBindings(): KeybindingsConfig {
		const arr = JSON.parse(this.core.getResolvedBindingsJson()) as [string, KeyId[]][];
		const resolved: KeybindingsConfig = {};
		for (const [id, keys] of arr) {
			resolved[id] = keys.length === 1 ? keys[0] : [...keys];
		}
		return resolved;
	}
}
