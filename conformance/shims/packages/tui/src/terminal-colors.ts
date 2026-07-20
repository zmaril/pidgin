// Native shim for packages/tui/src/terminal-colors.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `terminal-colors.__pi_original__.ts` and this shim takes its place,
// so pi's tests import `../src/terminal-colors.ts` unchanged and hit Rust.
//
// Scope of the native flip: pi's entire terminal-colors module is a set of pure
// parsers, ported bit-exactly in `crates/pidgin-tui` (`terminal_colors.rs`,
// validated against the pi source) and exposed under pi's own export names —
// `isOsc11BackgroundColorResponse`, `parseOsc11BackgroundColor`, and
// `parseTerminalColorSchemeReport`. Each is a pure function of its input string:
// the OSC 11 response recognizer/parser (accepting the `#rrggbb`,
// `#rrrrggggbbbb`, and `rgb:`/`rgba:` `rrrr/gggg/bbbb` forms) and the DEC private
// mode 2031 color-scheme report parser. All parsing runs in Rust.
//
// The flip boundary: these are stateless pure functions, so the whole module
// crosses cleanly — inputs are whole strings, outputs are a `{ r, g, b }` object,
// the `"dark"`/`"light"` tag, or a boolean. No JS runtime concern (event target,
// timer, closure, stream, stable identity) is involved, so this shim reimplements
// no parsing logic; it only adapts napi's `null` to pi's `undefined` and re-typing
// the color-scheme tag. The `RgbColor` and `TerminalColorScheme` types are
// re-exported from the original unchanged.

export * from "./terminal-colors.__pi_original__.ts";

import {
	isOsc11BackgroundColorResponse as nativeIsOsc11BackgroundColorResponse,
	parseOsc11BackgroundColor as nativeParseOsc11BackgroundColor,
	parseTerminalColorSchemeReport as nativeParseTerminalColorSchemeReport,
} from "pidgin-napi";
import type { RgbColor, TerminalColorScheme } from "./terminal-colors.__pi_original__.ts";

export function isOsc11BackgroundColorResponse(data: string): boolean {
	return nativeIsOsc11BackgroundColorResponse(data);
}

export function parseOsc11BackgroundColor(data: string): RgbColor | undefined {
	return nativeParseOsc11BackgroundColor(data) ?? undefined;
}

export function parseTerminalColorSchemeReport(data: string): TerminalColorScheme | undefined {
	return (nativeParseTerminalColorSchemeReport(data) as TerminalColorScheme | null) ?? undefined;
}
