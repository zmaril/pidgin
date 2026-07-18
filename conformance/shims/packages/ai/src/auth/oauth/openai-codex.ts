// Native shim for packages/ai/src/auth/oauth/openai-codex.ts, backed by the
// atilla Rust addon (`atilla-napi`). Installed by conformance/codegen.mjs when the
// module is marked `native` in conformance/manifest.json: the original pi file is
// preserved alongside as `openai-codex.__pi_original__.ts` and this shim takes its
// place, so pi's tests import `../src/auth/oauth/openai-codex.ts` unchanged and
// hit Rust.
//
// Scope of the native flip: the multi-step `login` / `refresh` OAuth flows, ported
// to the Rust `OAuthFlowMachine` (`atilla_ai::auth::oauth`). The Rust machine owns
// the flow logic (the login-method select prompt, the device-code poll loop with
// its interval/timeout/pending-on-403-404 handling, the JWT account-id decode, the
// token exchange); the shared `driveOAuthFlow` helper performs the effects (fetch /
// sleep / prompt / notify) in JS so pi's `vi.stubGlobal` fetch and fake timers
// still apply. Everything else the module exports (the `name`/`toAuth` surface and
// any constants) is re-exported from the original unchanged.

export * from "./openai-codex.__pi_original__.ts";

import { openaiCodexOAuth as piOpenaiCodexOAuth } from "./openai-codex.__pi_original__.ts";
import type { AuthInteraction, OAuthAuth, OAuthCredential } from "../types.ts";
import { driveOAuthFlow } from "atilla-napi/oauth-flow-driver.js";

export const openaiCodexOAuth: OAuthAuth = {
	...piOpenaiCodexOAuth,
	login(interaction: AuthInteraction): Promise<OAuthCredential> {
		return driveOAuthFlow("openai-codex", "login", undefined, interaction) as Promise<OAuthCredential>;
	},
	refresh(credential: OAuthCredential, signal?: AbortSignal): Promise<OAuthCredential> {
		return driveOAuthFlow("openai-codex", "refresh", credential, { signal }) as Promise<OAuthCredential>;
	},
};
