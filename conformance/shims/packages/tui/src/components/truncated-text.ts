// Native shim for packages/tui/src/components/truncated-text.ts, backed by the
// pidgin Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when
// the module is marked `native` in conformance/manifest.json: the original pi
// file is preserved alongside as `truncated-text.__pi_original__.ts` and this
// shim takes its place, so pi's tests import
// `../src/components/truncated-text.ts` unchanged and hit Rust.
//
// Scope of the native flip: `TruncatedText.render` ported bit-exactly in
// `crates/pidgin-tui` (validated against pi's truncated-text.test.ts). The
// class shape — constructor `(text, paddingX = 0, paddingY = 0)`, `invalidate`,
// and `render(width)` — mirrors pi's exactly; `render` delegates to the native
// `truncatedTextRender`.

export * from "./truncated-text.__pi_original__.ts";

import type { Component } from "../tui.ts";
import { truncatedTextRender as nativeTruncatedTextRender } from "pidgin-napi";

/**
 * Text component that truncates to fit viewport width.
 */
export class TruncatedText implements Component {
	private text: string;
	private paddingX: number;
	private paddingY: number;

	constructor(text: string, paddingX: number = 0, paddingY: number = 0) {
		this.text = text;
		this.paddingX = paddingX;
		this.paddingY = paddingY;
	}

	invalidate(): void {
		// No cached state to invalidate currently
	}

	render(width: number): string[] {
		return nativeTruncatedTextRender(this.text, this.paddingX, this.paddingY, width);
	}
}
