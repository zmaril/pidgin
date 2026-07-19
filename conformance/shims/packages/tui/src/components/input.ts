// Native shim for packages/tui/src/components/input.ts, backed by the pidgin
// Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file
// is preserved alongside as `input.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/components/input.ts` unchanged and hit
// Rust.
//
// Scope of the native flip: the single-line `Input` component ported bit-exactly
// in `crates/pidgin-tui` (validated against pi's input.test.ts) — grapheme-aware
// cursor movement, the Emacs-style kill ring, undo, word navigation, bracketed
// paste, and the horizontally-scrolling `render`. This shim re-implements pi's
// `Input` class over the native `InputCore`, keeping `onSubmit`/`onEscape` as JS
// callbacks and `focused` as a JS-facing accessor (the core reads focus at
// render time, so the setter forwards it). `InputCore` cannot call JS closures,
// so it records any submit/escape that fired during a `handleInput` call and
// returns it as `{ submit, escape }`; the shim replays that onto the callbacks,
// mapping napi `null` to the "no submit" case.

export * from "./input.__pi_original__.ts";

import type { Component, Focusable } from "../tui.ts";
import { InputCore } from "pidgin-napi";

/**
 * Input component - single-line text input with horizontal scrolling.
 * Native-backed via `InputCore`.
 */
export class Input implements Component, Focusable {
	public onSubmit?: (value: string) => void;
	public onEscape?: () => void;

	private core: InputCore;
	private _focused: boolean = false;

	constructor() {
		this.core = new InputCore();
	}

	/** Focusable interface - set by TUI when focus changes. */
	get focused(): boolean {
		return this._focused;
	}

	set focused(value: boolean) {
		this._focused = value;
		this.core.setFocused(value);
	}

	getValue(): string {
		return this.core.getValue();
	}

	setValue(value: string): void {
		this.core.setValue(value);
	}

	handleInput(data: string): void {
		const event = this.core.handleInput(data);
		// `submit` is the submitted value (possibly "") or null for no submit;
		// pi fires onSubmit with the value even when it is empty.
		if (event.submit !== null && event.submit !== undefined) {
			if (this.onSubmit) this.onSubmit(event.submit);
		}
		if (event.escape) {
			if (this.onEscape) this.onEscape();
		}
	}

	invalidate(): void {
		// No cached state to invalidate currently.
	}

	render(width: number): string[] {
		return this.core.render(width);
	}
}
