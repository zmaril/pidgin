// Native shim for packages/tui/src/components/markdown.ts, backed by the atilla
// Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `markdown.__pi_original__.ts` and this shim takes
// its place, so pi's tests import `../src/components/markdown.ts` unchanged.
//
// Scope of the native flip: the Markdown terminal renderer ported bit-exactly
// in `crates/atilla-tui` (validated against the vectors extracted from pi's
// markdown.test.ts). The native entry `markdownRender(source, width)` bakes in
// pi's `defaultMarkdownTheme` at chalk level 3, zero padding, no default text
// style, and no options. This shim re-implements pi's `Markdown` class by
// composing the original and routing `render(width)` through Rust ONLY when the
// construction matches that shape — the theme deep-matches the default (probed
// against its exact chalk-level-3 output), padding is zero, there is no default
// text style, and no options flag is set. Every other construction (custom
// theme, padding, default text style, preserve* options) delegates to pi's
// original class, so behavior is byte-for-byte pi's in all cases.

export * from "./markdown.__pi_original__.ts";

import { getCapabilities } from "../terminal-image.ts";
import type { Component } from "../tui.ts";
import { markdownRender as nativeMarkdownRender } from "atilla-napi";
import {
	type DefaultTextStyle,
	Markdown as OriginalMarkdown,
	type MarkdownOptions,
	type MarkdownTheme,
} from "./markdown.__pi_original__.ts";

// pi's `defaultMarkdownTheme` (test-themes.ts) rendered at chalk level 3, probed
// on a single-space sentinel. A theme matches the native default iff every style
// function reproduces these exact byte strings and it carries no `highlightCode`
// / `codeBlockIndent` extension.
const SENTINEL = " ";
const DEFAULT_THEME_PROBE: Record<string, string> = {
	heading: "\x1b[1m\x1b[36m \x1b[39m\x1b[22m",
	link: "\x1b[34m \x1b[39m",
	linkUrl: "\x1b[2m \x1b[22m",
	code: "\x1b[33m \x1b[39m",
	codeBlock: "\x1b[32m \x1b[39m",
	codeBlockBorder: "\x1b[2m \x1b[22m",
	quote: "\x1b[3m \x1b[23m",
	quoteBorder: "\x1b[2m \x1b[22m",
	hr: "\x1b[2m \x1b[22m",
	listBullet: "\x1b[36m \x1b[39m",
	bold: "\x1b[1m \x1b[22m",
	italic: "\x1b[3m \x1b[23m",
	strikethrough: "\x1b[9m \x1b[29m",
	underline: "\x1b[4m \x1b[24m",
};

function isDefaultTheme(theme: MarkdownTheme): boolean {
	if (
		(theme as { highlightCode?: unknown }).highlightCode !== undefined ||
		(theme as { codeBlockIndent?: unknown }).codeBlockIndent !== undefined
	) {
		return false;
	}
	for (const [key, expected] of Object.entries(DEFAULT_THEME_PROBE)) {
		const fn = (theme as unknown as Record<string, unknown>)[key];
		if (typeof fn !== "function") return false;
		try {
			if ((fn as (t: string) => string)(SENTINEL) !== expected) return false;
		} catch {
			return false;
		}
	}
	return true;
}

function optionsAreDefault(options?: MarkdownOptions): boolean {
	if (!options) return true;
	return !options.preserveOrderedListMarkers && !options.preserveBackslashEscapes;
}

/**
 * Markdown component. Native-backed on the default-theme / no-padding /
 * no-options path; otherwise delegates to pi's original class.
 */
export class Markdown implements Component {
	private text: string;
	private useNative: boolean;
	private original: OriginalMarkdown;

	private cachedText?: string;
	private cachedWidth?: number;
	private cachedLines?: string[];

	constructor(
		text: string,
		paddingX: number,
		paddingY: number,
		theme: MarkdownTheme,
		defaultTextStyle?: DefaultTextStyle,
		options?: MarkdownOptions,
	) {
		this.text = text;
		this.original = new OriginalMarkdown(text, paddingX, paddingY, theme, defaultTextStyle, options);
		this.useNative =
			paddingX === 0 &&
			paddingY === 0 &&
			defaultTextStyle === undefined &&
			optionsAreDefault(options) &&
			isDefaultTheme(theme);
	}

	setText(text: string): void {
		this.text = text;
		this.original.setText(text);
		this.invalidate();
	}

	invalidate(): void {
		this.original.invalidate();
		this.cachedText = undefined;
		this.cachedWidth = undefined;
		this.cachedLines = undefined;
	}

	render(width: number): string[] {
		// The native renderer bakes in `hyperlinks: false`; pi reads the global
		// `getCapabilities().hyperlinks` seam at render time (OSC 8 emission). When
		// hyperlinks are enabled, delegate to pi's original so that seam is honored.
		if (!this.useNative || getCapabilities().hyperlinks) {
			return this.original.render(width);
		}
		if (this.cachedLines && this.cachedText === this.text && this.cachedWidth === width) {
			return this.cachedLines;
		}
		const result = nativeMarkdownRender(this.text, width);
		this.cachedText = this.text;
		this.cachedWidth = width;
		this.cachedLines = result;
		return result;
	}
}
