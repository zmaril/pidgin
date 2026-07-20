# pidgin-ai Un-merge Evidence Packet

**Provenance:** pidgin `7756e93`, pi pinned commit `3da591ab`. Read-only audit of `/workspace/pidgin`.
**Scope:** symbol-level un-merge evidence for splitting merged Rust files back to per-pi-file modules, per the strict split-not-merge rule. All line ranges verified against the current tree.

**Dependency graph (verified from `Cargo.toml`):** `pidgin-ai` depends only downward (never on coding/extensions). `pidgin-coding` -> `pidgin-ai` (+ `pidgin-agent`), **not** on extensions. `pidgin-extensions` -> `pidgin-ai` (unconditional) and -> `pidgin-coding` (optional, `deno` feature only). This graph is what makes the moves legal and constrains section A.

---

## A. `extension.rs` — EXACT symbol partition

File: `crates/pidgin-ai/src/auth/oauth/extension.rs` (499 lines). Merges pi's `packages/ai/src/compat/extension-oauth-types.ts` (TYPE surface -> **STAYS** in pidgin-ai) and the `adaptOAuth` logic from `packages/coding-agent/src/core/provider-composer.ts:230-248` (-> **MOVES** to pidgin-coding).

### STAYS in pidgin-ai (extension-oauth-types.ts)

| rs item | lines | pi symbol |
|---|---|---|
| `OAuthAuthInfo` (struct) | 63–71 | `OAuthAuthInfo` (types.ts:11-14) |
| `OAuthDeviceCodeInfo` (struct) | 73–85 | `OAuthDeviceCodeInfo` (types.ts:17-22) |
| `OAuthPrompt` (struct) | 87–98 | `OAuthPrompt` (types.ts:4-8) |
| `OAuthSelectOption` (struct) | 100–108 | `OAuthSelectOption` (types.ts:24-27) |
| `OAuthSelectPrompt` (struct) | 110–118 | `OAuthSelectPrompt` (types.ts:29-32) |
| `OAuthLoginCallbacks` (trait) | 120–142 | `OAuthLoginCallbacks` (types.ts:35-43) |

### MOVES to pidgin-coding (provider-composer.ts `adaptOAuth`)

| rs item | lines | pi symbol |
|---|---|---|
| `adapt_extension_oauth` (pub fn) | 163–174 | `adaptOAuth` (provider-composer.ts:230-248) |
| `ExtensionOAuthAuth` (priv struct + `impl OAuthAuth`) | 176–214 | adaptOAuth's returned object (`:231-247`) |
| `NOT_WIRED` (const) | 216–218 | (pidgin-only wiring guard; supports the moved path) |
| `Resume` (priv enum) | 220–226 | re-inversion bridge (no pi analog) |
| `Running` (priv struct) | 228–236 | re-inversion bridge (no pi analog) |
| `ExtensionLoginMachine` (priv struct + impls) | 238–362 | re-inversion of adaptOAuth's `login` (`:234-243`) |
| `ChannelCallbacks` (priv struct + `impl OAuthLoginCallbacks`) | 364–455 | adaptOAuth's callback map `onAuth/onDeviceCode/...` (`:234-242`) |
| `ExtensionRefreshMachine` (priv struct + impls) | 457–495 | adaptOAuth's `refresh` (`:245`) |
| `#[cfg(test)] mod tests` | 497–498 | (test file `extension/tests.rs`) |

### The genuinely-ambiguous item — `ExtensionOAuthLogin` (trait), lines 144–161

pi provenance points to **provider-composer.ts** (it mirrors the effectful members of pi's `ExtensionOAuthConfig`, `:37-39`) -> argues MOVE. But its **role** is a cross-crate seam: it is implemented in `pidgin-extensions` (`DenoExtensionOAuthLogin`) and held by `composer.rs`'s `ExtensionOAuthConfig.login: Option<Arc<dyn ExtensionOAuthLogin>>`. It pairs with `OAuthLoginCallbacks`, which stays. **Recommendation: keep `ExtensionOAuthLogin` in pidgin-ai** (Option B), because:
- The moved code (`ExtensionLoginMachine`, `ChannelCallbacks`, `ExtensionRefreshMachine`) and `composer.rs`'s `ExtensionOAuthConfig` both need it; keeping it in pidgin-ai lets both just `use pidgin_ai::auth::oauth::extension::ExtensionOAuthLogin`.
- The extensions-plane consumer (`oauth_login_impl.rs:54`) then needs **no import change**.
- Option A (move it too) is *technically legal* — `oauth_login_impl.rs` is `#[cfg(feature="deno")]`-gated (lib.rs:120-121) and pidgin-extensions already pulls pidgin-coding under `deno` — but it forces the extensions lane to re-source that one trait from pidgin-coding while `OAuthLoginCallbacks` still comes from pidgin-ai. Higher churn, no benefit.

### Cross-crate `use` the moved code needs after the split

The moved bridge is deeply coupled to pidgin-ai internals. After landing in pidgin-coding it must import (all verified pub-reachable):

```rust
use pidgin_ai::auth::error::AuthFlowError;                         // error.rs:126 pub
use pidgin_ai::auth::types::{AuthEvent, AuthPrompt, AuthPromptKind,
    AuthSelectOption, ModelAuth, OAuthAuth, OAuthCredential};      // types.rs, all pub; auth::types is pub mod
use pidgin_ai::auth::oauth::flow::{OAuthFlowMachine, Step, StepInput};  // pub, also re-exported at auth::oauth
use pidgin_ai::auth::oauth::device_code::CANCEL_MESSAGE;           // device_code.rs:45 `pub const`
use pidgin_ai::auth::oauth::extension::{OAuthAuthInfo, OAuthDeviceCodeInfo,
    OAuthPrompt, OAuthSelectOption, OAuthSelectPrompt, OAuthLoginCallbacks,
    ExtensionOAuthLogin};                                          // the STAYING type surface (incl. the seam trait per Option B)
```

**This is a heavy, not thin, move** — flag honestly: unlike pi's trivial `adaptOAuth`, the Rust port carries a full `std::thread` + `mpsc` re-inversion bridge that leans on ~10 pidgin-ai types. It compiles cross-crate (all pub), but the PR is larger than "port a 19-line function."

`auth/oauth/mod.rs:33-36` currently re-exports all 8 extension symbols; that re-export list must be split (drop the moved ones, keep the 6 types + `ExtensionOAuthLogin`).

---

## B. `composer.rs` — confirm move detail

File: `crates/pidgin-ai/src/providers/composer.rs` (902 lines). Port of the **credential-aware AUTH half** of pi's `provider-composer.ts`. Per the split rule this whole file moves to pidgin-coding.

### (1) Full pub symbol list + line ranges

| pub item | lines | note |
|---|---|---|
| `ConfigValueError` (struct + Display + Error) | 88–101 | seam error |
| `ConfigValueResolver` (trait) | 103–137 | the seam (see B5) |
| `ProviderAuthConfig` (struct) | 139–153 | |
| `ExtensionOAuthConfig` (struct + Debug/PartialEq/Eq) | 155–197 | holds `Arc<dyn ExtensionOAuthLogin>` |
| `ExtensionAuthConfig` (struct) | 199–212 | |
| `with_configured_auth` (fn) | 256–291 | `provider-composer.ts:250` |
| `config_context_env` (fn) | 293–321 | `:279` |
| `compose_api_key_auth` (fn) | 542–577 | `:293` |
| `compose_oauth_auth` (fn) | 630–657 | `:359` |
| `adapt_oauth` (fn) | 659–672 | thin call into `adapt_extension_oauth` |
| `ComposedProvider` (struct + impl) | 674–740 | |
| `ComposeAuthError` (struct + impls) | 742–753 | |
| `ComposeModelProviderInput` (struct) | 755–785 | |
| `compose_model_provider` (fn) | 802–853 | `:412` |

Private, move with the file: `configured_api_key` (214-222), `configured_headers` (224-242), `configured_auth_header` (244-254), `ComposedApiKeyAuth` (323-540), `ComposedOAuthAuth` (579-628), `require_auth_method` (787-800), `error_stream` (855-879), `zero_usage` (881-898). Exported at `providers/mod.rs:22-27` and re-exported at `lib.rs:35-39`.

### (2) All consumers across `crates/*/src`, by crate+file

**pidgin-coding** (the only external consumer crate):
- `core/model_runtime.rs:81` `use pidgin_ai::compose_model_provider as compose_rich_provider;`
- `core/model_runtime.rs:82` `use pidgin_ai::providers::composer::{ComposeAuthError, ComposeModelProviderInput};`
- `core/model_runtime.rs:87` `use pidgin_ai::providers::ConfigValueResolver;`
- `core/model_runtime.rs:747` call `compose_rich_provider(ComposeModelProviderInput {...})`; `:732` return type `ComposeAuthError`; `:172,228` `Arc<dyn ConfigValueResolver>`.
- `core/provider_composer.rs:835` `impl pidgin_ai::ConfigValueResolver for ConfigValueResolverAdapter`; `:849,852,860,864` `pidgin_ai::ConfigValueError`; `:872-873` returns `pidgin_ai::ProviderAuthConfig`; `:886-894` returns `pidgin_ai::ExtensionAuthConfig`, constructs `pidgin_ai::ExtensionOAuthConfig`.

No other crate references these symbols.

### (3) Internal-caller check (clean-move verification)

Grep of all 14 pub symbols across `crates/pidgin-ai/src/`, **excluding** composer.rs + its `tests/`, extension.rs + its `tests/`, and the three re-export files (`lib.rs`, `providers/mod.rs`, `auth/oauth/mod.rs`): **EMPTY**. Nothing inside pidgin-ai (outside the allowed files) calls these. **Clean move confirmed.**

### (4) What composer.rs imports from pidgin-ai (the `crate::` -> `pidgin_ai::` surface after moving)

Lines 69–86:

```
crate::auth::error::AuthFlowError
crate::auth::oauth::extension::{adapt_extension_oauth, ExtensionOAuthLogin}   // becomes intra-crate iff A's moved part lands here too
crate::auth::oauth::flow::OAuthFlowMachine
crate::auth::types::{ApiKeyAuth, ApiKeyCredential, AuthCheck, AuthContext, AuthInteraction,
    AuthPrompt, AuthPromptKind, AuthResult, AuthType, ModelAuth, OAuthAuth, OAuthCredential,
    ProviderAuth, ProviderHeaders}
crate::compat::get_api_provider
crate::providers::registry::RegistryProvider
crate::seams::provider::{AbortSignal, StreamResult}
crate::types::{AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model,
    StopReason, StreamOptions, Usage, UsageCost}
```

All rewrite to `pidgin_ai::...`. `RegistryProvider`/`ConfigValueResolver` are already proven cross-crate-reachable from model_runtime. **Two to double-check for pub visibility before the PR:** `pidgin_ai::compat::get_api_provider` and `pidgin_ai::seams::provider::{AbortSignal, StreamResult}`.

### (5) The `ConfigValueResolver` seam — collapses after the move

- Defined in composer.rs: `ConfigValueResolver` trait (103-137) + `ConfigValueError` (88-101).
- pidgin-coding wires it in `core/provider_composer.rs`: `ConfigValueResolverAdapter` (`:833-868`) implements `pidgin_ai::ConfigValueResolver`, delegating to pidgin-coding's `core/resolve_config_value.rs` (`resolveConfigValueOrThrow`/`resolveHeadersOrThrow`/...), re-wrapping `ConfigValueError` at `:852,864`.
- **After composer.rs lands in pidgin-coding, the trait, its sole implementor, and the underlying resolver are all in one crate.** The seam exists *only* to cross the pidgin-ai<->pidgin-coding boundary, so it can collapse: delete `ConfigValueResolverAdapter` and the `pidgin_ai::ConfigValueError(error.0)` re-wraps, and have the composers call `resolve_config_value.rs` directly. **Confirmed collapsible.**

### MAJOR complication for B — name collision

pidgin-coding **already owns** `compose_model_provider`, `ComposedProvider`, `ExtensionOAuthConfig`, and `ConfigValueError` in `core/provider_composer.rs` (the credential-*blind* half, PR #119). Moving pidgin-ai's AUTH-layer versions of those four names into the same crate **collides**. Today they coexist only because they're in different crates (disambiguated by `pidgin_ai::` vs `crate::` and the `compose_rich_provider` alias at model_runtime.rs:81). Post-move the teammate must module-scope or rename (e.g. land the AUTH layer as a `provider_composer_auth` submodule, or merge the two halves). This is the single biggest risk in the whole packet — the move is not a pure lift.

---

## C. `content.rs` / `tools.rs` / `simple_options.rs` — per-function provenance

All three live under `crates/pidgin-ai/src/api/anthropic/`; submodules declared in `api/anthropic.rs:40-45`. **No external consumers exist** (see D) — these are internal-only splits. Precedent for sibling split files already in-tree: `api/mistral/transform_messages.rs`, `api/bedrock/transform_messages.rs`.

### C1. `content.rs` — `transform-messages.ts` + `anthropic-messages.ts`

| rs item | lines | pi source | evidence |
|---|---|---|---|
| `NON_VISION_USER/TOOL_IMAGE_PLACEHOLDER` | 23–25 | transform-messages | consts for downgrade |
| `sanitize_surrogates` (pub) | 32–34 | **sanitize-unicode.ts** (3rd file) | `sanitizeSurrogates`; leaf, used by anthropic side |
| `normalize_tool_call_id` (pub) | 38–49 | anthropic-messages | `normalizeToolCallId` (:1050) |
| `is_image` (priv) | 56–58 | transform-messages | used by BOTH halves — shared leaf |
| `replace_images_with_placeholder` (priv) | 63–86 | transform-messages | `replaceImagesWithPlaceholder` (:15) |
| `downgrade_unsupported_images` (priv) | 90–120 | transform-messages | `downgradeUnsupportedImages` (:35) |
| `is_same_model` (priv) | 124–128 | transform-messages | `:95` |
| `transform_assistant_content` (priv) | 132–210 | transform-messages | `:100-148` |
| `transform_messages` (pub) | 218–313 | transform-messages | `transformMessages` (:64) |
| `insert_synthetic_tool_results` (priv) | 318–345 | transform-messages | `:163` |
| `convert_content_blocks` (pub) | 355–390 | anthropic-messages | `convertContentBlocks` (:115) |
| `ConvertedToolResult` (priv struct) | 394–397 | anthropic-messages | return of `convertToolResult` (:1054) |
| `convert_tool_result` (priv) | 402–448 | anthropic-messages | `convertToolResult` (:1054) |
| `convert_messages` (pub) | 457–510 | anthropic-messages | `convertMessages` (:1089) |
| `push_user_message` (priv) | 514–556 | anthropic-messages | convertMessages user arm (:1103) |
| `push_assistant_message` (priv) | 560–635 | anthropic-messages | convertMessages assistant arm (:1141) |
| `apply_cache_control_to_last_user` (priv) | 639–675 | anthropic-messages | convertMessages tail (:1229) |

**Proposed un-merge:** new sibling **`transform_messages.rs`** <- {the transform-messages rows: consts 23-25, `is_image`, `replace_images_with_placeholder`, `downgrade_unsupported_images`, `is_same_model`, `transform_assistant_content`, `transform_messages`, `insert_synthetic_tool_results`}. **`content.rs` keeps** the anthropic-messages rows (`normalize_tool_call_id`, `convert_content_blocks`, `ConvertedToolResult`, `convert_tool_result`, `convert_messages`, `push_user_message`, `push_assistant_message`, `apply_cache_control_to_last_user`).
**Dependency direction:** `transform_messages.rs` -> `content.rs` for `normalize_tool_call_id` (`transform_assistant_content` calls it — and this matches pi, where `transformMessages` receives `normalizeToolCallId` from anthropic-messages). One-way; clean.
**Homeless helpers:** `is_image` used by both sides — keep a copy in each module (pi inlines the `type==="image"` check). `sanitize_surrogates` is genuinely from a **third** pi file (`utils/sanitize-unicode.ts`); it stays on the anthropic side (its only caller). A future micro-split to `sanitize_unicode.rs` is possible but out of scope — flag.

### C2. `tools.rs` — `anthropic-messages.ts` + `deferred-tools.ts`

| rs item | lines | pi source | evidence |
|---|---|---|---|
| `CLAUDE_CODE_TOOLS` (const) | 21–39 | anthropic-messages | `claudeCodeTools` (:79) |
| `to_claude_code_name` (pub) | 44–51 | anthropic-messages | `toClaudeCodeName` (:102) |
| `normalize_tool_name` (pub) | 55–61 | anthropic-messages | `normalizeToolName` (:929) |
| `tool_name` (priv) | 65–67 | helper | reads `.name`; shared leaf (dup in simple_options) |
| `ToolPlacement` (pub struct) | 72–78 | deferred-tools | result of `splitDeferredTools` |
| `split_deferred_tools` (pub) | 84–147 | deferred-tools | `splitDeferredTools` (:8) |
| `convert_tools` (pub) | 153–204 | anthropic-messages | `convertTools` (:1260) |

**Proposed un-merge:** new sibling **`deferred_tools.rs`** <- {`ToolPlacement`, `split_deferred_tools`}. **`tools.rs` keeps** {`CLAUDE_CODE_TOOLS`, `to_claude_code_name`, `normalize_tool_name`, `convert_tools`}.
**Dependency direction:** `deferred_tools.rs` -> `tools.rs` for `normalize_tool_name` (`split_deferred_tools` applies it — matches pi, `splitDeferredTools` takes a `normalizeName`). One-way; clean.
**Homeless helper:** `tool_name` — keep a copy in each module.

### C3. `simple_options.rs` — `simple-options.ts` + `estimate.ts` slice

Estimate slice (-> new **`estimate.rs`**): `CHARS_PER_TOKEN` (36), `ESTIMATED_IMAGE_CHARS` (38), `ContextUsageEstimate` (72-78, estimate.ts:3), `calculate_context_tokens` (81-87, :19), `js_len` (92-94), `safe_json_stringify` (97-99, :27), `estimate_text_tokens` (102-104, :40), `div_ceil` (107-109), `estimate_string_content_tokens` (113-115, :35), `estimate_block_content_tokens` (119-129, :36), `estimate_message_tokens` (132-154, :52), `message_timestamp` (157-163), `get_last_assistant_usage_info` (166-186, :74), `estimate_messages` (189-214, :100), `tool_name` (217-219, dup), `estimate_tools_tokens` (223-228, :130), `estimate_context_tokens` (pub, 231-282, :139).

Simple-options portion (-> **stays in `simple_options.rs`**): `CONTEXT_SAFETY_TOKENS` (32, :12), `MIN_MAX_TOKENS` (34, :13), `SimpleStreamOptions` (53-65), `clamp_max_tokens_to_context` (pub, 289-302, :15), `clamp_reasoning` (306-311, :45), `default_budget` (314-323, :57), `budget_for` (327-336), `AdjustedThinking` (339-343), `adjust_max_tokens_for_thinking` (pub, 348-371, :50), `build_base_options` (pub, 376-397, :20).

**Ambiguous:** `ThinkingBudgets` (42-48) is from pi's **`types.ts`** (a 3rd file), not simple-options.ts — colocate on the simple-options side (its only consumer); flag provenance.
**Dependency direction:** `simple_options.rs` -> `estimate.rs` (`clamp_max_tokens_to_context` calls `estimate_context_tokens`). `estimate.rs` has no back-dependency. One-way; matches pi (simple-options.ts imports utils/estimate.ts). The file header comment already anticipates this: it says the estimator was colocated only because "the Rust port cannot edit outside `api/anthropic*`" — a sibling `estimate.rs` under `api/anthropic/` satisfies that constraint.
`driver.rs:54` imports `adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context, SimpleStreamOptions` — all stay in `simple_options.rs`; **unchanged**.

---

## D. Consumer hunks by crate (sign-offs)

### pidgin-coding lane — REQUIRED sign-off (from B, the big one)

`core/model_runtime.rs` and `core/provider_composer.rs` import the composer AUTH symbols. Old -> new path when `composer.rs` moves into pidgin-coding:

| file | symbols | old path | new path |
|---|---|---|---|
| model_runtime.rs:81 | `compose_model_provider` (as `compose_rich_provider`) | `pidgin_ai::compose_model_provider` | `crate::...` (intra-crate; **resolve name collision** with existing local `compose_model_provider`) |
| model_runtime.rs:82 | `ComposeAuthError`, `ComposeModelProviderInput` | `pidgin_ai::providers::composer::...` | `crate::...` |
| model_runtime.rs:87 | `ConfigValueResolver` | `pidgin_ai::providers::ConfigValueResolver` | `crate::...` |
| provider_composer.rs:835,849-864 | `ConfigValueResolver`, `ConfigValueError` | `pidgin_ai::...` | seam **collapses** — adapter deleted (B5) |
| provider_composer.rs:872-894 | `ProviderAuthConfig`, `ExtensionAuthConfig`, `ExtensionOAuthConfig` | `pidgin_ai::...` | `crate::...` (**collision** on `ExtensionOAuthConfig`, `ConfigValueError`) |

### pidgin-extensions lane — sign-off ONLY under Option A (from A)

- `src/oauth_login_impl.rs:54` — `use pidgin_ai::auth::oauth::extension::{ExtensionOAuthLogin, OAuthLoginCallbacks};`
- `tests/deno_oauth_phase_a.rs:29-31` — imports `ExtensionOAuthLogin, OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectPrompt`.

Under **recommended Option B** (`ExtensionOAuthLogin` stays in pidgin-ai): **both unchanged — no extensions sign-off needed for A.** Under Option A (trait moves): split each import — `ExtensionOAuthLogin` from `pidgin_coding::...`, the rest from `pidgin_ai::...` (legal only because these files are `deno`-gated and pidgin-extensions already depends on pidgin-coding under `deno`).

### No external sign-off for C

Grep for every content/tools/simple_options symbol across `crates/*/src` outside pidgin-ai returns **only false positives** — pidgin-coding's and pidgin-agent's *own* `estimate_context_tokens` / `ContextUsageEstimate` / `SimpleStreamOptions` in their `compaction/` modules (distinct symbols, different signatures: `estimate_context_tokens(&[AgentMessage])` vs pidgin-ai's `(&Context)`). None import from `pidgin_ai::api::anthropic::{content,tools,simple_options}`. **The C splits are internal-only**; the sole `use` edits are intra-crate:
- `api/anthropic/request.rs:22` -> split: `transform_messages` from `super::transform_messages`; `convert_messages`+`sanitize_surrogates` from `super::content`.
- `api/anthropic/request.rs:24` -> split: `split_deferred_tools` from `super::deferred_tools`; `convert_tools`+`normalize_tool_name` from `super::tools`.
- `api/anthropic.rs:40-45` -> add `pub mod transform_messages; pub mod deferred_tools; pub mod estimate;`.
- `content.rs:21` and `driver.rs:54` — unchanged.

---

## Summary

Clean moves: composer.rs internal-caller check is empty (B3), and the C-file splits have zero external consumers. Two flagged complications:

1. **Part B name collision** — pidgin-coding already owns `compose_model_provider` / `ComposedProvider` / `ExtensionOAuthConfig` / `ConfigValueError` in its `provider_composer.rs`; the move needs module-scoping/renaming, not a pure lift.
2. **Part A `ExtensionOAuthLogin` ambiguity** — pi provenance = provider-composer.ts, but seam role = pidgin-ai; recommend keeping it in pidgin-ai to avoid extensions-lane churn. Note the A move drags a heavy `std::thread`+`mpsc` bridge (not pi's trivial `adaptOAuth`) across the boundary.

The `ConfigValueResolver` seam collapses after B.
