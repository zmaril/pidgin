//! Native `llama.cpp` model provider — a faithful port of pi-coding-agent's
//! `extensions/llama/provider.ts`.
//!
//! Mirrors pi symbol-for-symbol: the [`LLAMA_PROVIDER_ID`] /
//! [`DEFAULT_LLAMA_SERVER_URL`] consts, the private `credential_server_url` /
//! `resolve_server_url` / `to_pi_model` helpers, the
//! [`LlamaProviderController`] (`provider` + `set_catalog`), and
//! [`create_llama_provider`] which assembles the provider object with its
//! api-key `login`/`check`/`resolve` auth, `get_models`/`refresh_models`, and
//! `stream`/`stream_simple` delegation.
//!
//! # How pi's `Provider<"openai-completions">` maps into Rust
//!
//! pi's `createLlamaProvider()` returns an object literal typed
//! `Provider<"openai-completions">` (pi-ai `models.ts:75`). There is no single
//! Rust type that captures that whole interface faithfully:
//!
//! - The ported runtime provider unit, [`pidgin_ai::providers::RegistryProvider`],
//!   carries only a pared-down `ProviderAuth { name, api_key_env_vars }` and has
//!   no slot for a provider's `auth.apiKey` `login`/`check`/`resolve` handler.
//! - The full api-key handler lives in the separate
//!   [`pidgin_ai::auth::ApiKeyAuth`] trait (`auth/types.rs`).
//!
//! So this port assembles a dedicated [`LlamaProvider`] struct that composes the
//! real pidgin-ai boundary types — [`ApiKeyAuth`] (behind
//! [`pidgin_ai::auth::ProviderAuth`]) for auth, [`Model`] for the catalog,
//! [`OpenAICompletionsCompat`] for the `openai-completions` compat block, and the
//! [`pidgin_ai::compat`] registry-dispatch entrypoint for streaming. Each piece
//! maps to a real pi-ai symbol; only the outer container differs (a struct rather
//! than an object literal or a `RegistryProvider`).
//!
//! `Model<"openai-completions">` is represented as [`Model`] (i.e. `Model<Value>`,
//! the shape the registry / compat / snapshot paths all use): the TS const-generic
//! api tag becomes the runtime `api = "openai-completions"` field, and the typed
//! `openai-completions` compat block is built from [`OpenAICompletionsCompat`] and
//! serialized into `Model.compat`.
//!
//! # Seam deviations from pi
//!
//! - pi closes over ambient `fetch` and `process.env`. This sync port injects the
//!   [`HttpTransport`] seam (as [`client`](super::client) does) and the
//!   [`ExecutionEnv`] seam (as pi-ai's `DefaultAuthContext` does), so
//!   [`create_llama_provider`] takes them as arguments where pi takes none.
//! - `stream` / `stream_simple` delegate to [`pidgin_ai::compat::stream`], the
//!   registry-dispatch analog of pi's `compat` `stream`/`streamSimple`. The Rust
//!   `compat` exposes only the unified `stream` entrypoint (its provider seam
//!   unifies pi's raw/simple split into one eager stream — see
//!   `pidgin_ai::providers::Models::stream_simple`), so both delegate to it,
//!   preserving pi's two call sites.
//! - [`RefreshContext`] (the ported `RefreshModelsContext`) carries no `signal`
//!   field, so `refresh_models`' `signal?.aborted` early-return is subsumed and
//!   the catalog fetch runs with no abort signal.
//! - [`LlamaProviderController::set_catalog`] returns [`Result`] because
//!   [`llama_inference_url`] is fallible in Rust (pi's `toPiModel` throws on a
//!   malformed server URL); the callers only ever pass an already-normalized URL.
// straitjacket-allow-file:duplication

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use pidgin_ai::auth::{
    ApiKeyAuth, ApiKeyCredential, AuthCheck, AuthContext, AuthFlowError, AuthInteraction,
    AuthPrompt, AuthPromptKind, AuthResult, AuthType, Credential, ModelAuth, ProviderAuth,
};
use pidgin_ai::compat::{stream as compat_stream, CompatError};
use pidgin_ai::providers::RefreshContext;
use pidgin_ai::seams::http::HttpTransport;
use pidgin_ai::seams::provider::{AbortSignal, StreamResult};
use pidgin_ai::seams::storage::ExecutionEnv;
use pidgin_ai::StreamOptions;
use pidgin_ai::{Context, MaxTokensField, Modality, Model, ModelCost, OpenAICompletionsCompat};

use super::client::{
    llama_inference_url, normalize_llama_server_url, LlamaClient, LlamaListOptions, LlamaModelInfo,
    LlamaModelStatus,
};

/// The provider id (`LLAMA_PROVIDER_ID`).
pub const LLAMA_PROVIDER_ID: &str = "llama.cpp";
/// The default management server URL (`DEFAULT_LLAMA_SERVER_URL`).
pub const DEFAULT_LLAMA_SERVER_URL: &str = "http://127.0.0.1:8080";
/// The default per-request token cap (`DEFAULT_MAX_TOKENS`).
const DEFAULT_MAX_TOKENS: u64 = 16384;

/// Read the management server URL a credential carries in its
/// `env.LLAMA_BASE_URL`, normalized (`credentialServerUrl`).
fn credential_server_url(credential: Option<&ApiKeyCredential>) -> Option<String> {
    let value = credential?.env.as_ref()?.get("LLAMA_BASE_URL")?;
    if value.trim().is_empty() {
        return None;
    }
    normalize_llama_server_url(value).ok()
}

/// Resolve the effective management server URL from the credential first, then
/// the ambient `LLAMA_BASE_URL`, normalizing the result (`resolveServerUrl`).
fn resolve_server_url(
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
) -> Option<String> {
    let configured = credential_server_url(credential).or_else(|| {
        ctx.env("LLAMA_BASE_URL")
            .map(|value| value.trim().to_string())
    });
    match configured {
        Some(configured) if !configured.is_empty() => normalize_llama_server_url(&configured).ok(),
        _ => None,
    }
}

/// Map a router catalog entry into a `Model<"openai-completions">` with zero cost
/// and the `openai-completions` compat block (`toPiModel`).
fn to_pi_model(model: &LlamaModelInfo, server_url: &str) -> Result<Model> {
    let reported_context_window = model
        .meta
        .as_ref()
        .and_then(|meta| meta.n_ctx.or(meta.n_ctx_train));
    let context_window: u64 = match reported_context_window {
        Some(value) if value > 0.0 => value as u64,
        _ => 128_000,
    };
    let has_image = model
        .architecture
        .as_ref()
        .and_then(|architecture| architecture.input_modalities.as_ref())
        .is_some_and(|modalities| modalities.iter().any(|modality| modality == "image"));
    let input = if has_image {
        vec![Modality::Text, Modality::Image]
    } else {
        vec![Modality::Text]
    };
    let compat = OpenAICompletionsCompat {
        supports_store: Some(false),
        supports_developer_role: Some(false),
        supports_reasoning_effort: Some(false),
        supports_usage_in_streaming: Some(false),
        supports_strict_mode: Some(false),
        max_tokens_field: Some(MaxTokensField::MaxTokens),
        ..Default::default()
    };
    Ok(Model {
        id: model.id.clone(),
        name: model.id.clone(),
        api: "openai-completions".to_string(),
        provider: LLAMA_PROVIDER_ID.to_string(),
        base_url: llama_inference_url(server_url)?,
        reasoning: false,
        thinking_level_map: None,
        input,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window,
        max_tokens: DEFAULT_MAX_TOKENS.min(context_window),
        headers: None,
        compat: Some(serde_json::to_value(&compat)?),
    })
}

/// The api-key auth handler for the `llama.cpp` provider (pi's
/// `auth.apiKey` object): `login` verifies the server and stores the URL plus an
/// optional key; `check`/`resolve` stay dormant until a URL is configured.
///
/// Holds the injected [`HttpTransport`] the login verification issues its
/// catalog request through, and the [`ExecutionEnv`] seam `login` reads
/// `process.env.LLAMA_BASE_URL` from for its prompt placeholder and fallback.
struct LlamaApiKeyAuth {
    transport: Arc<dyn HttpTransport>,
    env: Arc<dyn ExecutionEnv>,
}

impl ApiKeyAuth for LlamaApiKeyAuth {
    fn name(&self) -> &str {
        "llama.cpp server"
    }

    fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Option<Result<ApiKeyCredential, AuthFlowError>> {
        Some(self.login_inner(interaction))
    }

    fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Option<AuthCheck> {
        resolve_server_url(ctx, credential).map(|_| AuthCheck {
            source: Some(check_source(credential)),
            check_type: AuthType::ApiKey,
        })
    }

    fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, AuthFlowError> {
        let Some(server_url) = resolve_server_url(ctx, credential) else {
            return Ok(None);
        };
        let api_key = credential
            .and_then(|credential| credential.key.clone())
            .or_else(|| ctx.env("LLAMA_API_KEY"))
            .unwrap_or_else(|| "local".to_string());
        // env = { ...credential?.env, LLAMA_BASE_URL: serverUrl }.
        let mut env = credential
            .and_then(|credential| credential.env.clone())
            .unwrap_or_default();
        env.insert("LLAMA_BASE_URL".to_string(), server_url.clone());
        let base_url = llama_inference_url(&server_url)
            .map_err(|error| AuthFlowError::new(error.to_string()))?;
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(api_key),
                headers: None,
                base_url: Some(base_url),
            },
            env: Some(env),
            source: Some(check_source(credential)),
        }))
    }
}

impl LlamaApiKeyAuth {
    /// The `login` flow body (`auth.apiKey.login`): prompt for the server URL and
    /// an optional key, verify the server answers a catalog request, then return
    /// the stored `{ key, env: { LLAMA_BASE_URL } }` credential.
    fn login_inner(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthFlowError> {
        let ambient_base_url = self.env.env_var("LLAMA_BASE_URL");
        let placeholder = ambient_base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_LLAMA_SERVER_URL.to_string());
        let entered_url = interaction.prompt(AuthPrompt {
            signal: None,
            kind: AuthPromptKind::Text {
                message: "llama.cpp server URL".to_string(),
                placeholder: Some(placeholder),
            },
        })?;
        // enteredUrl.trim() || process.env.LLAMA_BASE_URL || DEFAULT_LLAMA_SERVER_URL.
        let trimmed = entered_url.trim();
        let base = if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            ambient_base_url
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_LLAMA_SERVER_URL.to_string())
        };
        let server_url = normalize_llama_server_url(&base)
            .map_err(|error| AuthFlowError::new(error.to_string()))?;
        let api_key = interaction
            .prompt(AuthPrompt {
                signal: None,
                kind: AuthPromptKind::Secret {
                    message: "API key (optional)".to_string(),
                    placeholder: None,
                },
            })?
            .trim()
            .to_string();
        let key = if api_key.is_empty() {
            None
        } else {
            Some(api_key)
        };
        let client = LlamaClient::new(self.transport.clone(), &server_url, key.clone())
            .map_err(|error| AuthFlowError::new(error.to_string()))?;
        client
            .list(LlamaListOptions {
                reload: false,
                signal: interaction.signal(),
            })
            .map_err(|error| AuthFlowError::new(error.to_string()))?;
        let mut env = BTreeMap::new();
        env.insert("LLAMA_BASE_URL".to_string(), server_url);
        Ok(ApiKeyCredential {
            key,
            env: Some(env),
        })
    }
}

/// The auth source label (`credential ? "stored credential" : "LLAMA_BASE_URL"`).
fn check_source(credential: Option<&ApiKeyCredential>) -> String {
    if credential.is_some() {
        "stored credential".to_string()
    } else {
        "LLAMA_BASE_URL".to_string()
    }
}

/// The native `llama.cpp` provider (pi's `Provider<"openai-completions">`): id /
/// name / base metadata, the api-key auth handler, the dynamically-set model
/// catalog, and `stream`/`stream_simple` dispatch.
pub struct LlamaProvider {
    id: String,
    name: String,
    base_url: String,
    auth: ProviderAuth,
    models: Mutex<Vec<Model>>,
    transport: Arc<dyn HttpTransport>,
}

impl LlamaProvider {
    /// The provider id (`provider.id`).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The provider display name (`provider.name`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The provider inference base URL (`provider.baseUrl`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The provider auth handlers (`provider.auth`).
    pub fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    /// The current known models (`provider.getModels`): the catalog last set by
    /// [`set_catalog`](Self::set_catalog) (empty before the first).
    pub fn get_models(&self) -> Vec<Model> {
        self.models.lock().unwrap().clone()
    }

    /// Replace the catalog from a router model list, keeping only `loaded` models
    /// and mapping each via [`to_pi_model`] (pi's `setCatalog`).
    pub fn set_catalog(&self, catalog: &[LlamaModelInfo], server_url: &str) -> Result<()> {
        let mut mapped = Vec::new();
        for model in catalog {
            if model.status.value == LlamaModelStatus::Loaded {
                mapped.push(to_pi_model(model, server_url)?);
            }
        }
        *self.models.lock().unwrap() = mapped;
        Ok(())
    }

    /// Restore the catalog for a dynamic refresh (`provider.refreshModels`): fetch
    /// the router catalog with the credential's server URL / key and repopulate.
    /// Skips offline refreshes and non-api-key credentials, mirroring pi's guards
    /// (see the module note on the absent `signal` field).
    pub fn refresh_models(&self, context: &RefreshContext) -> Result<()> {
        if !context.allow_network {
            return Ok(());
        }
        let Some(Credential::ApiKey(credential)) = &context.credential else {
            return Ok(());
        };
        let Some(server_url) = credential_server_url(Some(credential)) else {
            return Ok(());
        };
        let client = LlamaClient::new(self.transport.clone(), &server_url, credential.key.clone())?;
        let catalog = client.list(LlamaListOptions {
            reload: false,
            signal: None,
        })?;
        self.set_catalog(&catalog, &server_url)?;
        Ok(())
    }

    /// Stream a response for `model` (`provider.stream`), delegating to the
    /// [`pidgin_ai::compat`] registry-dispatch entrypoint.
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> Result<StreamResult, CompatError> {
        compat_stream(model, context, options, signal)
    }

    /// Stream a response from the simple options (`provider.streamSimple`). The
    /// Rust `compat` surface unifies pi's raw/simple split, so this shares
    /// [`stream`](Self::stream)'s dispatch (see the module note).
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        signal: Option<&AbortSignal>,
    ) -> Result<StreamResult, CompatError> {
        compat_stream(model, context, options, signal)
    }
}

/// The controller returned by [`create_llama_provider`]: the provider plus the
/// [`set_catalog`](Self::set_catalog) hook the `/llama` command drives to publish
/// the router's loaded models (pi's `LlamaProviderController`).
pub struct LlamaProviderController {
    /// The native `llama.cpp` provider.
    pub provider: Arc<LlamaProvider>,
}

impl LlamaProviderController {
    /// Publish the router catalog into the provider (pi's `controller.setCatalog`).
    pub fn set_catalog(&self, models: &[LlamaModelInfo], server_url: &str) -> Result<()> {
        self.provider.set_catalog(models, server_url)
    }
}

/// Build the native `llama.cpp` provider and its catalog controller
/// (`createLlamaProvider`).
///
/// `transport` backs the login verification and dynamic refresh (pi's ambient
/// `fetch`); `env` backs `login`'s `process.env.LLAMA_BASE_URL` reads. See the
/// module note on these injected seams.
pub fn create_llama_provider(
    transport: Arc<dyn HttpTransport>,
    env: Arc<dyn ExecutionEnv>,
) -> LlamaProviderController {
    let auth = ProviderAuth {
        api_key: Some(Box::new(LlamaApiKeyAuth {
            transport: transport.clone(),
            env,
        })),
        oauth: None,
    };
    let provider = LlamaProvider {
        id: LLAMA_PROVIDER_ID.to_string(),
        name: "llama.cpp".to_string(),
        // DEFAULT_LLAMA_SERVER_URL is a compile-time-known valid http URL, so its
        // inference URL never fails to normalize.
        base_url: llama_inference_url(DEFAULT_LLAMA_SERVER_URL)
            .expect("DEFAULT_LLAMA_SERVER_URL is a valid http URL"),
        auth,
        models: Mutex::new(Vec::new()),
        transport,
    };
    LlamaProviderController {
        provider: Arc::new(provider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_ai::auth::DefaultAuthContext;
    use pidgin_ai::seams::http::ScriptedTransport;
    use pidgin_ai::seams::storage::MemoryEnv;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    use super::super::client::{LlamaArchitecture, LlamaMeta, LlamaModelStatusInfo};

    /// A scripted [`AuthInteraction`] replaying queued prompt answers in order —
    /// the Rust stand-in for pi's `prompt: async () => answers.shift()!`.
    struct ScriptedInteraction {
        answers: StdMutex<VecDeque<String>>,
    }

    impl ScriptedInteraction {
        fn new(answers: Vec<String>) -> Self {
            Self {
                answers: StdMutex::new(answers.into()),
            }
        }
    }

    impl AuthInteraction for ScriptedInteraction {
        fn prompt(&self, _prompt: AuthPrompt) -> Result<String, AuthFlowError> {
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| AuthFlowError::new("no scripted answer"))
        }

        fn notify(&self, _event: pidgin_ai::auth::AuthEvent) {}
    }

    fn controller_with(transport: ScriptedTransport) -> LlamaProviderController {
        let transport: Arc<dyn HttpTransport> = Arc::new(transport);
        let env: Arc<dyn ExecutionEnv> = Arc::new(MemoryEnv::new());
        create_llama_provider(transport, env)
    }

    fn model_info(id: &str, status: LlamaModelStatus) -> LlamaModelInfo {
        LlamaModelInfo {
            id: id.to_string(),
            aliases: None,
            status: LlamaModelStatusInfo {
                value: status,
                ..Default::default()
            },
            architecture: None,
            source: None,
            meta: None,
        }
    }

    /// Mirrors the pi test "exposes only loaded models with router metadata":
    /// `set_catalog` filters to loaded models and maps router metadata, cost 0,
    /// and the `openai-completions` compat block. No HTTP.
    #[test]
    fn exposes_only_loaded_models_with_router_metadata() {
        let controller = controller_with(ScriptedTransport::new());

        let loaded = LlamaModelInfo {
            id: "loaded".to_string(),
            aliases: None,
            status: LlamaModelStatusInfo {
                value: LlamaModelStatus::Loaded,
                args: Some(vec![
                    "llama-server".to_string(),
                    "--n-gpu-layers".to_string(),
                    "999".to_string(),
                ]),
                ..Default::default()
            },
            architecture: Some(LlamaArchitecture {
                input_modalities: Some(vec!["text".to_string(), "image".to_string()]),
                output_modalities: None,
            }),
            source: None,
            meta: Some(LlamaMeta {
                n_ctx: Some(16384.0),
                n_ctx_train: Some(131072.0),
                size: None,
                ftype: None,
            }),
        };

        controller
            .set_catalog(
                &[
                    loaded,
                    model_info("unloaded", LlamaModelStatus::Unloaded),
                    model_info("loading", LlamaModelStatus::Loading),
                ],
                "http://localhost:8080",
            )
            .unwrap();

        let models = controller.provider.get_models();
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.id, "loaded");
        assert_eq!(model.name, "loaded");
        assert_eq!(model.api, "openai-completions");
        assert_eq!(model.provider, LLAMA_PROVIDER_ID);
        assert_eq!(model.base_url, "http://localhost:8080/v1");
        assert_eq!(model.context_window, 16384);
        assert_eq!(model.max_tokens, 16384);
        assert_eq!(model.input, vec![Modality::Text, Modality::Image]);
        assert!(!model.reasoning);
        assert_eq!(model.cost.input, 0.0);
        assert_eq!(model.cost.output, 0.0);
        assert_eq!(model.cost.cache_read, 0.0);
        assert_eq!(model.cost.cache_write, 0.0);
        // The openai-completions compat block serializes with pi's exact JSON keys.
        let compat = model.compat.as_ref().unwrap();
        assert_eq!(compat["supportsStore"], serde_json::json!(false));
        assert_eq!(compat["supportsDeveloperRole"], serde_json::json!(false));
        assert_eq!(compat["supportsReasoningEffort"], serde_json::json!(false));
        assert_eq!(compat["supportsUsageInStreaming"], serde_json::json!(false));
        assert_eq!(compat["supportsStrictMode"], serde_json::json!(false));
        assert_eq!(compat["maxTokensField"], serde_json::json!("max_tokens"));
    }

    /// A model with no `meta` context window falls back to 128000, capped at
    /// `DEFAULT_MAX_TOKENS`.
    #[test]
    fn defaults_context_window_when_meta_absent() {
        let controller = controller_with(ScriptedTransport::new());
        controller
            .set_catalog(
                &[model_info("bare", LlamaModelStatus::Loaded)],
                "http://localhost:8080",
            )
            .unwrap();
        let models = controller.provider.get_models();
        assert_eq!(models[0].context_window, 128_000);
        assert_eq!(models[0].max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(models[0].input, vec![Modality::Text]);
    }

    /// Mirrors the pi test "stays dormant until configured and stores URL plus
    /// optional key": `check`/`resolve` are dormant with an empty context, `login`
    /// verifies the server and stores `{ key, env }`, and `resolve` with the stored
    /// credential yields the request auth.
    #[test]
    fn stays_dormant_until_configured_and_stores_url_plus_key() {
        let empty_ctx = DefaultAuthContext::new(MemoryEnv::new());

        let scripted = ScriptedTransport::new();
        scripted.push_ok(r#"{"data":[]}"#); // login's catalog verification request.
        let controller = controller_with(scripted.clone());
        let auth = controller.provider.auth().api_key.as_ref().unwrap();

        // Dormant before any URL is configured.
        assert!(auth.check(&empty_ctx, None).is_none());
        assert!(auth.resolve(&empty_ctx, None).unwrap().is_none());

        // login prompts for the URL then the optional key.
        let interaction = ScriptedInteraction::new(vec![
            "http://localhost:8080".to_string(),
            "secret".to_string(),
        ]);
        let credential = auth.login(&interaction).unwrap().unwrap();
        let mut expected_env = BTreeMap::new();
        expected_env.insert(
            "LLAMA_BASE_URL".to_string(),
            "http://localhost:8080".to_string(),
        );
        assert_eq!(
            credential,
            ApiKeyCredential {
                key: Some("secret".to_string()),
                env: Some(expected_env.clone()),
            }
        );

        // login verified the server with the Bearer key.
        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].url, "http://localhost:8080/models");
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer secret")
        );

        // check/resolve are now configured from the stored credential.
        let check = auth.check(&empty_ctx, Some(&credential)).unwrap();
        assert_eq!(check.check_type, AuthType::ApiKey);
        assert_eq!(check.source.as_deref(), Some("stored credential"));

        let resolved = auth
            .resolve(&empty_ctx, Some(&credential))
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved,
            AuthResult {
                auth: ModelAuth {
                    api_key: Some("secret".to_string()),
                    headers: None,
                    base_url: Some("http://localhost:8080/v1".to_string()),
                },
                env: Some(expected_env),
                source: Some("stored credential".to_string()),
            }
        );
    }

    /// `resolve` falls back to `LLAMA_BASE_URL` from the ambient context (no
    /// credential) and defaults the api key to `local`.
    #[test]
    fn resolve_uses_ambient_base_url_and_local_key() {
        let ctx = DefaultAuthContext::new(
            MemoryEnv::new().with_env("LLAMA_BASE_URL", "http://example.test:9090/v1/"),
        );
        let controller = controller_with(ScriptedTransport::new());
        let auth = controller.provider.auth().api_key.as_ref().unwrap();

        let check = auth.check(&ctx, None).unwrap();
        assert_eq!(check.source.as_deref(), Some("LLAMA_BASE_URL"));

        let resolved = auth.resolve(&ctx, None).unwrap().unwrap();
        assert_eq!(resolved.auth.api_key.as_deref(), Some("local"));
        assert_eq!(
            resolved.auth.base_url.as_deref(),
            Some("http://example.test:9090/v1")
        );
        assert_eq!(resolved.source.as_deref(), Some("LLAMA_BASE_URL"));
        assert_eq!(
            resolved
                .env
                .as_ref()
                .unwrap()
                .get("LLAMA_BASE_URL")
                .map(String::as_str),
            Some("http://example.test:9090")
        );
    }

    /// `refresh_models` fetches the router catalog with the credential's server URL
    /// and republishes only the loaded models; offline/non-api-key refreshes are
    /// skipped.
    #[test]
    fn refresh_models_fetches_and_republishes_loaded() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(
            r#"{"data":[{"id":"m1","status":{"value":"loaded"}},{"id":"m2","status":{"value":"unloaded"}}]}"#,
        );
        let controller = controller_with(scripted.clone());
        let provider = &controller.provider;

        let mut env = BTreeMap::new();
        env.insert(
            "LLAMA_BASE_URL".to_string(),
            "http://localhost:8080".to_string(),
        );
        let credential = Credential::ApiKey(ApiKeyCredential {
            key: Some("secret".to_string()),
            env: Some(env),
        });

        // Offline refresh does not fetch.
        provider
            .refresh_models(&RefreshContext {
                allow_network: false,
                force: false,
                credential: Some(credential.clone()),
            })
            .unwrap();
        assert!(provider.get_models().is_empty());
        assert!(scripted.requests().is_empty());

        // Online api-key refresh fetches and republishes the loaded model.
        provider
            .refresh_models(&RefreshContext {
                allow_network: true,
                force: false,
                credential: Some(credential),
            })
            .unwrap();
        let models = provider.get_models();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "m1");
        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, "http://localhost:8080/models");
    }
}
