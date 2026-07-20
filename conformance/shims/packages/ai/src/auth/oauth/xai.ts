// Native shim for packages/ai/src/auth/oauth/xai.ts, backed by the pidgin Rust
// addon (`pidgin-napi`). Installed by conformance/codegen.mjs when the module is
// marked `native` in conformance/manifest.json: the original pi file is preserved
// alongside as `xai.__pi_original__.ts` and this shim takes its place, so pi's
// tests import `../src/auth/oauth/xai.ts` unchanged and hit Rust.
//
// Scope of the native flip: the multi-step `login` / `refresh` OAuth flows, ported
// to the Rust `OAuthFlowMachine` (`pidgin_ai::auth::oauth`). The Rust machine owns
// the flow logic (the device-code grant, wait-before-first-poll polling with
// server slow_down/denied/expired handling, https-only verification-URI
// validation, refresh-token preservation and the token-response field checks); the
// shared `driveOAuthFlow` helper performs the effects (fetch / sleep / notify) in
// JS so pi's `vi.stubGlobal` fetch and fake timers still apply. Everything else the
// module exports (the `name`/`loginLabel`/`toAuth` surface and any constants) is
// re-exported from the original unchanged.

export * from "./xai.__pi_original__.ts";

import { xaiOAuth as piXaiOAuth } from "./xai.__pi_original__.ts";
import type { AuthInteraction, OAuthAuth, OAuthCredential } from "../types.ts";
import { driveOAuthFlow } from "pidgin-napi/oauth-flow-driver.js";

export const xaiOAuth: OAuthAuth = {
	...piXaiOAuth,
	login(interaction: AuthInteraction): Promise<OAuthCredential> {
		return driveOAuthFlow("xai", "login", undefined, interaction) as Promise<OAuthCredential>;
	},
	refresh(credential: OAuthCredential, signal?: AbortSignal): Promise<OAuthCredential> {
		return driveOAuthFlow("xai", "refresh", credential, { signal }) as Promise<OAuthCredential>;
	},
};
