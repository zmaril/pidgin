// Native shim for packages/ai/src/auth/oauth/device-code.ts, backed by the pidgin
// Rust addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module
// is marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `device-code.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/auth/oauth/device-code.ts` unchanged and hit
// Rust.
//
// Scope of the native flip: `pollOAuthDeviceCodeFlow`, ported to the Rust RFC 8628
// `DeviceCodePollMachine` (`pidgin_ai::auth::oauth::device_code`). The Rust machine
// owns the poll-loop logic (deadline math, interval progression, server slow_down
// handling, and the exact timeout/cancel messages); the shared `driveDevicePoll`
// helper performs the effects (the caller's `poll()` callback and `setTimeout`
// waits) in JS so pi's fake timers still control the poll timing. The result/option
// types are re-exported from the original unchanged.

export * from "./device-code.__pi_original__.ts";

import type { OAuthDeviceCodePollOptions } from "./device-code.__pi_original__.ts";
import { driveDevicePoll } from "pidgin-napi/oauth-flow-driver.js";

export function pollOAuthDeviceCodeFlow<T>(options: OAuthDeviceCodePollOptions<T>): Promise<T> {
	return driveDevicePoll(options) as Promise<T>;
}
