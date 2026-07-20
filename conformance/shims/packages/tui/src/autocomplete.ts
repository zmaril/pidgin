// Native shim for packages/tui/src/autocomplete.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `autocomplete.__pi_original__.ts` and this shim takes its place, so
// pi's tests import `../src/autocomplete.ts` unchanged and hit Rust.
//
// Scope of the native flip: the whole `CombinedAutocompleteProvider` — slash-command
// filtering, `@` fuzzy file walk, `./`/quoted path completion, prefix extraction,
// `applyCompletion`, and `shouldTriggerFileCompletion` — is ported bit-exactly in
// `crates/pidgin-tui` and validated against pi's autocomplete.test.ts. pi's provider
// reaches the filesystem/process world through four host seams (`readdirSync`,
// `statSync`, `homedir`, `spawn(fd, args)`); the Rust core abstracts those behind a
// `FileProvider` trait, and `AutocompleteCore` supplies a native implementation over
// `std::fs`/`std::process`. Because the tests drive the provider against real temp
// directories (and a real `fd` binary), reading the same filesystem from Rust and
// spawning the same `fd` reproduces pi's suggestions exactly.
//
// The only un-portable surface is a `SlashCommand.getArgumentCompletions` callback,
// which cannot cross the addon boundary. When any command supplies one, this shim
// delegates the whole provider to pi's original class (byte-for-byte pi). The suite
// always constructs with an empty command list, so the native path is exercised.

export * from "./autocomplete.__pi_original__.ts";

import { AutocompleteCore } from "pidgin-napi";
import {
	CombinedAutocompleteProvider as OriginalCombinedAutocompleteProvider,
	type AutocompleteItem,
	type AutocompleteProvider,
	type AutocompleteSuggestions,
	type SlashCommand,
} from "./autocomplete.__pi_original__.ts";

type Command = SlashCommand | AutocompleteItem;

/** A command carries an argument-completion callback that cannot cross the addon
 * boundary; such providers delegate wholesale to pi's original class. */
function commandHasArgumentCallback(command: Command): boolean {
	return typeof (command as SlashCommand).getArgumentCompletions === "function";
}

/** Serialize commands for the core, dropping the (never-present on this path)
 * callbacks. `name` marks a slash command; otherwise a plain item. */
function commandsToJson(commands: Command[]): string {
	return JSON.stringify(
		commands.map((command) => {
			const slash = command as SlashCommand;
			const item = command as AutocompleteItem;
			return {
				name: slash.name,
				value: item.value,
				label: item.label,
				description: command.description ?? null,
				argumentHint: slash.argumentHint ?? null,
			};
		}),
	);
}

/**
 * Combined provider handling slash commands and file paths. Native-backed unless
 * a command supplies `getArgumentCompletions`, in which case it delegates to pi's
 * original class.
 */
export class CombinedAutocompleteProvider implements AutocompleteProvider {
	private original?: OriginalCombinedAutocompleteProvider;
	private core?: AutocompleteCore;

	constructor(commands: Command[] = [], basePath: string, fdPath: string | null = null) {
		if (commands.some(commandHasArgumentCallback)) {
			this.original = new OriginalCombinedAutocompleteProvider(commands, basePath, fdPath);
			return;
		}
		this.core = new AutocompleteCore(commandsToJson(commands), basePath, fdPath ?? undefined);
	}

	async getSuggestions(
		lines: string[],
		cursorLine: number,
		cursorCol: number,
		options: { signal: AbortSignal; force?: boolean },
	): Promise<AutocompleteSuggestions | null> {
		if (this.original) {
			return this.original.getSuggestions(lines, cursorLine, cursorCol, options);
		}
		const json = this.core!.getSuggestionsJson(lines, cursorLine, cursorCol, options.force ?? false);
		return json === null ? null : (JSON.parse(json) as AutocompleteSuggestions);
	}

	applyCompletion(
		lines: string[],
		cursorLine: number,
		cursorCol: number,
		item: AutocompleteItem,
		prefix: string,
	): { lines: string[]; cursorLine: number; cursorCol: number } {
		if (this.original) {
			return this.original.applyCompletion(lines, cursorLine, cursorCol, item, prefix);
		}
		const itemJson = JSON.stringify({
			value: item.value,
			label: item.label,
			description: item.description ?? null,
		});
		return JSON.parse(this.core!.applyCompletionJson(lines, cursorLine, cursorCol, itemJson, prefix)) as {
			lines: string[];
			cursorLine: number;
			cursorCol: number;
		};
	}

	shouldTriggerFileCompletion(lines: string[], cursorLine: number, cursorCol: number): boolean {
		if (this.original) {
			return this.original.shouldTriggerFileCompletion(lines, cursorLine, cursorCol);
		}
		return this.core!.shouldTriggerFileCompletion(lines, cursorLine, cursorCol);
	}
}
