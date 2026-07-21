//! The Google Vertex AI [`Provider`] backend: the transport-aware adapter that
//! binds the ported `google-vertex` driver into the provider registry's
//! [`ApiRouting`](crate::providers::ApiRouting).
//!
//! The dialect algorithm — Express (API-key) request assembly, `x-goog-api-key`
//! header injection, and the shared Google stream decode — is ported at
//! [`crate::api::google_vertex`] and [`crate::api::google_shared`]. This module
//! is pure wiring: it adapts the generic [`Provider`] seam (which speaks
//! [`Model<Value>`](crate::types::Model) and [`StreamOptions`]) onto the driver's
//! typed [`stream`](crate::api::google_vertex::driver::stream) entry point (which
//! speaks [`GoogleModel`] and [`GoogleRequestOptions`]), threading an injected
//! [`HttpTransport`] and [`Clock`] so a live Vertex turn runs without wall-clock
//! or ambient-network access.
//!
//! # Auth scope
//!
//! This backend wires **both** Vertex auth paths. The resolved
//! `StreamOptions.api_key` is run through
//! [`resolve_api_key`](crate::api::google_vertex::resolve_api_key); a real key
//! drives the **Vertex Express (API-key) path** (`x-goog-api-key`). When it
//! resolves to nothing — an empty key, the `gcp-vertex-credentials` marker, or a
//! `<placeholder>` — the backend falls back to the **ADC / service-account path**:
//! it reads the `GOOGLE_APPLICATION_CREDENTIALS` keyfile, mints (and caches) a
//! Google OAuth2 access token via [`adc`], and drives the regional
//! `projects/{project}/locations/{location}` endpoint with
//! `Authorization: Bearer <token>`. Only that one ADC source is wired (the one
//! pi's Vertex runtime reads); the broader ADC chain is deferred (see
//! [`GoogleVertexBackend::resolve_adc`]). When neither an API key nor a
//! service-account keyfile (+ project + location) resolves, the driver surfaces a
//! pre-start "No API key" error rather than a live request.

// straitjacket-allow-file:duplication — the pre-start error-shell scaffolding
// (empty `AssistantMessage` + zeroed `Usage`) and the `reserialize_model` /
// `StreamOptions` bridging mirror the identical wiring in the google-generative-ai
// backend by design; the clone detector reads the shared boundary-type
// construction as duplicative.
// straitjacket-allow-file:file-size — TODO(straitjacket): this file is over the
// 1500-line ceiling. Declared explicitly so it suppresses only file-size, not every
// rule. The overrun is the buffered + incremental reasoning-lowering paths and their
// param-exact tests (Express key vs ADC Bearer credential resolution is threaded
// through each entry point, so the streaming paths cannot collapse into the buffered
// ones). Remove once the wiring is split into a submodule (see PR follow-up).

use std::sync::Arc;

use crate::api::google_shared::{
    vertex_thinking_option, GoogleEffort, GoogleModel, GoogleRequestOptions, GoogleThinkingOption,
};
use crate::api::google_vertex::{
    adc, driver, resolve_api_key, resolve_credentials_path, resolve_location, resolve_project,
    GoogleVertexClientOptions, VertexRequestCredential, API,
};
use crate::providers::clamp_thinking_level;
use crate::seams::clock::Clock;
use crate::seams::http::HttpTransport;
use crate::seams::provider::{AbortSignal, Provider, StreamResult};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Model, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, Usage, UsageCost,
};
use crate::utils::sse::AssistantEventReader;

/// The api id this backend serves, pi's `google-vertex` [`Api`] discriminant.
pub const GOOGLE_VERTEX_API: &str = API;

/// A [`Provider`] backend that runs a Google Vertex AI turn — Express (API-key)
/// or ADC / service-account (Bearer) — over an injected [`HttpTransport`],
/// sourcing the request timestamp from an injected [`Clock`].
///
/// Constructed via [`GoogleVertexBackend::new`] and installed as
/// [`ApiRouting::Single`](crate::providers::ApiRouting::Single) by
/// [`builtin_providers_with_transport`](crate::providers::builtin_providers_with_transport).
pub struct GoogleVertexBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
    /// The ADC / service-account access-token cache, reused across turns so a
    /// minted Bearer token is re-minted only as it nears expiry
    /// ([`adc::TokenCache`]). Idle on the Express (API-key) path.
    token_cache: adc::TokenCache,
}

impl GoogleVertexBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// message with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self {
            transport,
            clock,
            token_cache: adc::TokenCache::new(),
        }
    }
}

impl Provider for GoogleVertexBackend {
    fn api(&self) -> &str {
        GOOGLE_VERTEX_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&StreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        // Re-present the untyped boundary `Model<Value>` as the driver's typed
        // `GoogleModel` via serde. A malformed model is surfaced as a clean
        // pre-start error event, never a panic.
        let mut typed_model: GoogleModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Google model is not compatible with google-vertex: {error}"),
                )
            }
        };

        // A per-request base-URL override targets the driver's request URL at the
        // right host. `applyAuth` has already applied any per-credential
        // `auth.baseUrl` onto `model.base_url`; this honors an explicit
        // `StreamOptions.base_url` on top of it.
        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        // Resolve the credential: a Vertex Express key drives the `x-goog-api-key`
        // path; absent that, a service-account keyfile (+ project + location)
        // drives the ADC Bearer path (the token minted/cached here, then threaded
        // into the driver's request assembler).
        let timestamp = self.clock.now_ms();
        let api_key = resolve_api_key_from(options);
        let adc = match self.resolve_adc(&api_key, options, timestamp) {
            Ok(adc) => adc,
            Err(message) => return error_result(model, timestamp, message),
        };
        let credential = credential_ref(&api_key, &adc);
        let headers = options.and_then(|o| o.headers.clone()).unwrap_or_default();
        let request_options = request_options_from(model, options);

        // The buffered driver performs a single synchronous request with no
        // in-flight window to observe an abort against (pi aborts an async SSE
        // read); `signal` is accepted for seam parity and left unobserved here.
        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            credential,
            &headers,
            &request_options,
            timestamp,
        )
    }

    fn stream_incremental<'a>(
        &'a self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> AssistantEventReader<'a> {
        // Reasoning requested: lower it onto the incremental request exactly as the
        // buffered `stream_simple` does (via the shared `lower_simple_options`),
        // then run the driver's incremental `stream_streaming` entry point. This is
        // full pi parity — pi has a single streaming path whose `streamSimple`
        // thinking shaping (`google-vertex.ts:301-335`) is applied regardless of
        // buffered vs incremental. The vertex driver already reads
        // `request_options.thinking` off `GoogleRequestOptions` in both `stream`
        // and `stream_streaming`, so the lowering lives in the backend as it does
        // for the buffered path (#340) — no driver change is needed. Credential
        // resolution (Express key vs ADC Bearer) is identical to the raw
        // `stream_incremental` / [`stream`](Self::stream) paths.
        if let Some(simple) = options.filter(|o| o.reasoning.is_some()) {
            let mut typed_model: GoogleModel = match reserialize_model(model) {
                Ok(typed_model) => typed_model,
                Err(error) => {
                    return AssistantEventReader::from_buffered(error_result(
                        model,
                        self.clock.now_ms(),
                        format!("Google model is not compatible with google-vertex: {error}"),
                    ));
                }
            };
            if let Some(base_url) = simple.base.base_url.as_ref() {
                typed_model.base_url = base_url.clone();
            }
            let timestamp = self.clock.now_ms();
            let api_key = resolve_api_key_from(Some(&simple.base));
            let adc = match self.resolve_adc(&api_key, Some(&simple.base), timestamp) {
                Ok(adc) => adc,
                // Mirror `stream`'s pre-start error shape as a replayed reader.
                Err(message) => {
                    return AssistantEventReader::from_buffered(error_result(
                        model, timestamp, message,
                    ));
                }
            };
            let credential = credential_ref(&api_key, &adc);
            let headers = simple.base.headers.clone().unwrap_or_default();
            let request_options = lower_simple_options(model, &typed_model, Some(simple));

            return driver::stream_streaming(
                self.transport.as_ref(),
                &typed_model,
                context,
                credential,
                &headers,
                &request_options,
                timestamp,
            );
        }

        // No reasoning: byte-identical to the pre-widening base incremental path.
        // Same model/options assembly as `stream`, but the request runs through
        // the driver's incremental `stream_streaming` entry point: the returned
        // reader pulls one chunk at a time off the transport, so a streaming
        // transport surfaces real per-frame timing while the buffered `stream`
        // path is left untouched.
        let options = options.map(|o| &o.base);
        let mut typed_model: GoogleModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                // Mirror `stream`'s pre-start error shape as a replayed reader.
                return AssistantEventReader::from_buffered(error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Google model is not compatible with google-vertex: {error}"),
                ));
            }
        };

        if let Some(base_url) = options.and_then(|o| o.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let timestamp = self.clock.now_ms();
        let api_key = resolve_api_key_from(options);
        let adc = match self.resolve_adc(&api_key, options, timestamp) {
            Ok(adc) => adc,
            // Mirror `stream`'s pre-start error shape as a replayed reader.
            Err(message) => {
                return AssistantEventReader::from_buffered(error_result(model, timestamp, message))
            }
        };
        let credential = credential_ref(&api_key, &adc);
        let headers = options.and_then(|o| o.headers.clone()).unwrap_or_default();
        let request_options = request_options_from(model, options);

        driver::stream_streaming(
            self.transport.as_ref(),
            &typed_model,
            context,
            credential,
            &headers,
            &request_options,
            timestamp,
        )
    }

    /// Lower the simple, level-based options onto the Vertex request, mirroring
    /// pi's `streamSimple` (`google-vertex.ts:301-335`).
    ///
    /// The seam's `reasoning` level is model-clamped (pi's `clampThinkingLevel`),
    /// then mapped `off ⇒ high` (google-specific: thinking stays ON, unlike the
    /// openai dialects that omit) and lowered by [`vertex_thinking_option`] into
    /// either the `thinkingLevel` path (Gemini-3-Pro/Flash — NO Gemma-4 gate,
    /// unlike gen-ai) or the `thinkingBudget` path (vertex's tables, which have no
    /// flash-lite branch and default to `-1`).
    ///
    /// When no `reasoning` is requested, this sets `thinking: { enabled: false }`
    /// (pi `:307-311`): for a non-reasoning model that emits no `thinkingConfig`
    /// (byte-identical to the raw request), and for a reasoning model it emits
    /// pi's `getDisabledThinkingConfig`. Credential resolution (Express key vs ADC
    /// Bearer) is identical to the raw [`stream`](Self::stream) path.
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<&SimpleStreamOptions>,
        _signal: Option<&AbortSignal>,
    ) -> StreamResult {
        let mut typed_model: GoogleModel = match reserialize_model(model) {
            Ok(typed_model) => typed_model,
            Err(error) => {
                return error_result(
                    model,
                    self.clock.now_ms(),
                    format!("Google model is not compatible with google-vertex: {error}"),
                )
            }
        };

        let base = options.map(|o| &o.base);
        if let Some(base_url) = base.and_then(|b| b.base_url.as_ref()) {
            typed_model.base_url = base_url.clone();
        }

        let timestamp = self.clock.now_ms();
        let api_key = resolve_api_key_from(base);
        let adc = match self.resolve_adc(&api_key, base, timestamp) {
            Ok(adc) => adc,
            Err(message) => return error_result(model, timestamp, message),
        };
        let credential = credential_ref(&api_key, &adc);
        let headers = base.and_then(|b| b.headers.clone()).unwrap_or_default();

        let request_options = lower_simple_options(model, &typed_model, options);

        driver::stream(
            self.transport.as_ref(),
            &typed_model,
            context,
            credential,
            &headers,
            &request_options,
            timestamp,
        )
    }
}

/// Lower the simple, level-based options onto the driver's
/// [`GoogleRequestOptions`], setting the `thinking` field per pi's `streamSimple`
/// (`google-vertex.ts:301-335`).
///
/// Shared by the buffered [`GoogleVertexBackend::stream_simple`] and the
/// incremental [`GoogleVertexBackend::stream_incremental`] reasoning path so both
/// lower reasoning identically — the single source of truth pi keeps in its one
/// `streamSimple` path (pi applies the same shaping regardless of buffered vs
/// incremental, having a single streaming path).
///
/// The seam's `reasoning` level is model-clamped (pi's `clampThinkingLevel`), then
/// mapped `off ⇒ high` (google-specific: thinking stays ON, unlike the openai
/// dialects that omit) and lowered by [`vertex_thinking_option`] into either the
/// `thinkingLevel` path (Gemini-3-Pro/Flash — NO Gemma-4 gate, unlike gen-ai) or
/// the `thinkingBudget` path (vertex's tables, which have no flash-lite branch and
/// default to `-1`). When no `reasoning` is requested, this sets
/// `thinking: { enabled: false }` (pi `:307-311`): for a non-reasoning model that
/// emits no `thinkingConfig` (byte-identical to the raw request), and for a
/// reasoning model it emits pi's `getDisabledThinkingConfig`.
fn lower_simple_options(
    model: &Model,
    typed_model: &GoogleModel,
    options: Option<&SimpleStreamOptions>,
) -> GoogleRequestOptions {
    let mut request_options = request_options_from(model, options.map(|o| &o.base));
    request_options.thinking = Some(match options.and_then(|o| o.reasoning) {
        Some(reasoning) => {
            let clamped = clamp_thinking_level(model, widen_thinking_level(reasoning));
            let effort = GoogleEffort::from_clamped(clamped);
            vertex_thinking_option(
                &typed_model.id,
                effort,
                options.and_then(|o| o.thinking_budgets.as_ref()),
            )
        }
        // No reasoning: pi's `thinking: { enabled: false }`.
        None => GoogleThinkingOption::default(),
    });
    request_options
}

/// Widen a caller's [`ThinkingLevel`] to the model-level [`ModelThinkingLevel`]
/// that [`clamp_thinking_level`] expects (pi's `SimpleStreamOptions.reasoning`,
/// which extends the base ladder with `off`; a requested level is never `off`).
fn widen_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// A resolved ADC / service-account credential: the minted Bearer token plus the
/// project + location that place the regional Vertex endpoint.
struct AdcResolved {
    token: String,
    project: String,
    location: String,
}

impl GoogleVertexBackend {
    /// Resolve the ADC / service-account credential when no Express API key is
    /// present, minting (and caching) a Bearer token from the
    /// `GOOGLE_APPLICATION_CREDENTIALS` service-account keyfile.
    ///
    /// Returns `Ok(None)` — deferring to the driver's "No API key" error — when an
    /// API key is present (Express wins), or when the ADC inputs are not fully
    /// configured (no keyfile path, or no project/location). Returns `Err` when
    /// the configured keyfile cannot be read/parsed or the token mint fails, so a
    /// misconfigured service account surfaces a real diagnostic rather than a bare
    /// "No API key".
    ///
    // Follow-up (#297): only the GOOGLE_APPLICATION_CREDENTIALS
    // service-account keyfile is resolved here — the one ADC source pi's Vertex
    // runtime wires (`buildGoogleAuthOptions`). google-auth-library's broader ADC
    // chain is NOT ported and is deferred: the gcloud well-known ADC file
    // (~/.config/gcloud/application_default_credentials.json), the GCE/GKE
    // metadata server, workload-identity federation, and service-account
    // impersonation. When none of the supported inputs resolve, the caller
    // surfaces pi's "No API key for provider" error rather than silently
    // attempting one of those sources.
    fn resolve_adc(
        &self,
        api_key: &Option<String>,
        options: Option<&StreamOptions>,
        now_ms: i64,
    ) -> Result<Option<AdcResolved>, String> {
        if api_key.is_some() {
            return Ok(None);
        }
        let client_options = client_options_from(options);
        let keyfile_path = match resolve_credentials_path(&client_options) {
            Some(path) => path,
            None => return Ok(None),
        };
        let project = match resolve_project(&client_options) {
            Ok(project) => project,
            Err(_) => return Ok(None),
        };
        let location = match resolve_location(&client_options) {
            Ok(location) => location,
            Err(_) => return Ok(None),
        };
        // pi hands the SDK the keyFilename and the SDK's node fs reads it; read the
        // same keyfile here (the credential acquisition sits outside pi's seam).
        let contents = std::fs::read_to_string(&keyfile_path).map_err(|error| {
            format!("failed to read GOOGLE_APPLICATION_CREDENTIALS ({keyfile_path}): {error}")
        })?;
        let key = adc::parse_service_account_key(&contents)?;
        let token = self
            .token_cache
            .get_or_mint(self.transport.as_ref(), &key, now_ms)?;
        Ok(Some(AdcResolved {
            token,
            project,
            location,
        }))
    }
}

/// Borrow the resolved credentials into the driver's [`VertexRequestCredential`]:
/// an Express API key wins; else a resolved ADC Bearer token; else `None` (the
/// driver's "No API key" error).
fn credential_ref<'a>(
    api_key: &'a Option<String>,
    adc: &'a Option<AdcResolved>,
) -> Option<VertexRequestCredential<'a>> {
    if let Some(api_key) = api_key {
        return Some(VertexRequestCredential::ApiKey(api_key));
    }
    if let Some(adc) = adc {
        return Some(VertexRequestCredential::Bearer {
            token: &adc.token,
            project: &adc.project,
            location: &adc.location,
        });
    }
    None
}

/// Build the [`GoogleVertexClientOptions`] the resolution helpers read, carrying
/// the request's `api_key` and the scoped `env` (pi's `ProviderEnv`) the Vertex
/// project/location/credentials resolution consults.
fn client_options_from(options: Option<&StreamOptions>) -> GoogleVertexClientOptions {
    GoogleVertexClientOptions {
        api_key: options.and_then(|o| o.api_key.clone()),
        env: options.and_then(|o| o.env.clone()).unwrap_or_default(),
        ..GoogleVertexClientOptions::default()
    }
}

/// Resolve the effective Vertex Express API key from [`StreamOptions`], applying
/// pi's `resolveApiKey` trimming/discarding (`google-vertex.ts:409`): an empty
/// string, the `gcp-vertex-credentials` marker, or a `<placeholder>` collapses to
/// `None` (the ADC path). Only `options.api_key` participates — project/location
/// are ADC inputs and are not read on the Express path.
fn resolve_api_key_from(options: Option<&StreamOptions>) -> Option<String> {
    resolve_api_key(&client_options_from(options))
}

/// Map [`StreamOptions`] onto the driver's [`GoogleRequestOptions`], threading the
/// #192 request-shaping fields with pi's Google precedence.
///
/// pi's Vertex `buildParams` (`google-vertex.ts:442`) reads only
/// `options.temperature` and `options.maxTokens`, mapping them into
/// `generationConfig.temperature` / `generationConfig.maxOutputTokens`; there is
/// no model temperature in pi's Vertex request. Precedence here mirrors that:
/// - `temperature` comes solely from `StreamOptions.temperature` (pi has no model
///   default to fall back to).
/// - `max_tokens` prefers `StreamOptions.max_tokens`; when the caller omits it we
///   fall back to the model's `maxTokens` default (`> 0`), the pidgin seam's
///   stand-in for the `streamSimple`/`buildBaseOptions` layer pi fills it from.
/// - `metadata` is intentionally NOT threaded: the Google dialect never consumes
///   it in pi (only anthropic reads `metadata.user_id`), so mapping it into the
///   request would diverge from pi.
///
/// `model` is the boundary [`Model`] (carrying the `maxTokens` default); the
/// per-request shaping fields come from `options`.
fn request_options_from(model: &Model, options: Option<&StreamOptions>) -> GoogleRequestOptions {
    let temperature = options.and_then(|o| o.temperature);
    let max_tokens = options
        .and_then(|o| o.max_tokens)
        .or_else(|| (model.max_tokens > 0).then_some(model.max_tokens));
    GoogleRequestOptions {
        temperature,
        max_tokens,
        tool_choice: None,
        thinking: None,
        aborted: false,
    }
}

/// Re-present a `Model<Value>` as a [`GoogleModel`] via a serde JSON round-trip,
/// decoding the lenient Google model slice the driver reads.
fn reserialize_model(model: &Model) -> Result<GoogleModel, serde_json::Error> {
    let json = serde_json::to_value(model)?;
    serde_json::from_value(json)
}

/// A single-`error`-event result for a failure before the driver's stream start
/// (an undecodable model), matching the registry's and driver's pre-start error
/// shape.
fn error_result(model: &Model, timestamp: i64, message: String) -> StreamResult {
    let error = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Error,
        error_message: Some(message),
        timestamp,
    };
    StreamResult {
        events: vec![AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: error.clone(),
        }],
        message: error,
    }
}

#[cfg(test)]
pub(crate) fn hello_sse_body() -> String {
    // One `?alt=sse` frame: a single "Hello" text part with a STOP finish and
    // usage metadata. Mirrors the shape the Vertex streamGenerateContent endpoint
    // returns and the ported decoder's own fixtures.
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]},\
\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\
\"candidatesTokenCount\":1,\"totalTokenCount\":2}}\n\n"
        .to_string()
}

#[cfg(test)]
pub(crate) fn multi_frame_hello_sse_body() -> String {
    // Three `?alt=sse` frames streaming "He" / "llo" / "!" as text deltas into a
    // single accumulating block, the last carrying the STOP finish + usage. Split
    // per frame it exercises multi-chunk incremental timing.
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"He\"}]}}]}\n\n\
data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"llo\"}]}}]}\n\n\
data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}]},\
\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\
\"candidatesTokenCount\":3,\"totalTokenCount\":4}}\n\n"
        .to_string()
}

/// The Vertex Express request URL for the default (placeholder-base-URL) model:
/// the `@google/genai` SDK's `aiplatform.googleapis.com` endpoint with the `v1`
/// version and the `publishers/google/models/{model}` resource path.
#[cfg(test)]
pub(crate) const EXPRESS_STREAM_URL: &str =
    "https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse";

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use std::collections::BTreeMap;
    use std::io;
    use std::time::{Duration, Instant};

    use crate::seams::clock::FakeClock;
    use crate::seams::http::{HttpRequest, HttpResponse, HttpStreamResponse, ScriptedTransport};
    use crate::types::ContentBlock;

    /// A neutral google-vertex `Model<Value>` targeting `base_url`. The backend
    /// re-serializes this into [`GoogleModel`]. The default `base_url` carries the
    /// `{location}` template placeholder, so it resolves to the SDK's Express
    /// endpoint (no custom base URL).
    fn vertex_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "gemini-3-flash-preview",
            "name": "Gemini 3 Flash Preview",
            "api": "google-vertex",
            "provider": "google-vertex",
            "baseUrl": base_url,
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap()
    }

    /// The default Vertex model whose base URL still carries the `{location}`
    /// placeholder, so the driver falls back to the SDK Express endpoint.
    fn default_model() -> Model {
        vertex_model(
            "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
        )
    }

    fn user_context() -> Context {
        serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap()
    }

    fn scripted_hello() -> (ScriptedTransport, Arc<dyn HttpTransport>) {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(hello_sse_body());
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        (scripted, transport)
    }

    fn fake_clock() -> Arc<dyn Clock> {
        Arc::new(FakeClock::new(1_700_000_000_000))
    }

    // (a) Drives the backend end to end through ScriptedTransport (default
    // features): the `hello` fixture yields a single "Hello" text block.
    #[test]
    fn backend_streams_hello() {
        let (_scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&default_model(), &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(
            result.message.content,
            vec![ContentBlock::Text {
                text: "Hello".to_string(),
                text_signature: None,
            }]
        );
    }

    // (b) The request carries `x-goog-api-key: <key>` and the Vertex Express
    // `publishers/google/models/{model}:streamGenerateContent?alt=sse` URL under
    // the SDK's `aiplatform.googleapis.com/v1` endpoint.
    #[test]
    fn backend_request_carries_api_key_and_express_url() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&default_model(), &user_context(), Some(&options), None);

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].url, EXPRESS_STREAM_URL);
        assert_eq!(
            requests[0]
                .headers
                .get("x-goog-api-key")
                .map(String::as_str),
            Some("AIzaSyExampleRealisticLookingApiKey123456")
        );
        assert!(!requests[0].headers.contains_key("authorization"));
    }

    // A per-request `base_url` override (no embedded version) targets the request
    // at the right host, with the `v1` version and the publishers resource path.
    #[test]
    fn backend_honors_stream_options_base_url() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            base_url: Some("https://proxy.test".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&default_model(), &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://proxy.test/v1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
        );
    }

    // A per-request `base_url` override that already carries a version segment
    // suppresses the appended `v1`, matching the SDK's `baseUrlIncludesApiVersion`.
    #[test]
    fn backend_base_url_with_version_suppresses_appended_version() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            base_url: Some("https://proxy.test/v1".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&default_model(), &user_context(), Some(&options), None);

        assert_eq!(
            scripted.requests()[0].url,
            "https://proxy.test/v1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
        );
    }

    // (c) A non-2xx create surfaces the API's error body through the error event.
    #[test]
    fn backend_non_2xx_surfaces_error_body() {
        let scripted = ScriptedTransport::new();
        scripted.push_response(Ok(HttpResponse {
            status: 400,
            headers: std::collections::BTreeMap::new(),
            body: json!({ "error": { "code": 400, "message": "API key not valid" } }).to_string(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&default_model(), &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("400 API key not valid")
        );
    }

    // A missing credential surfaces the exact pi error, no panic and no request.
    #[test]
    fn backend_missing_api_key_errors_without_request() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let result = backend.stream(&default_model(), &user_context(), None, None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("No API key for provider: google-vertex")
        );
        assert!(scripted.requests().is_empty());
    }

    // The `gcp-vertex-credentials` ADC marker resolves to no Express key, and with
    // no `GOOGLE_APPLICATION_CREDENTIALS` keyfile in the env the ADC / service-
    // account path has nothing to resolve, so the backend surfaces the same "No
    // API key" error with no request on the wire.
    #[test]
    fn backend_adc_marker_without_keyfile_errors_without_request() {
        let scripted = ScriptedTransport::new();
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let options = StreamOptions {
            api_key: Some("gcp-vertex-credentials".to_string()),
            ..StreamOptions::default()
        };
        let result = backend.stream(&default_model(), &user_context(), Some(&options), None);

        assert_eq!(result.message.stop_reason, StopReason::Error);
        assert_eq!(
            result.message.error_message.as_deref(),
            Some("No API key for provider: google-vertex")
        );
        assert!(scripted.requests().is_empty());
    }

    /// A scripted transport pre-loaded with the multi-frame SSE body.
    fn scripted_multi_frame() -> (ScriptedTransport, Arc<dyn HttpTransport>) {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(multi_frame_hello_sse_body());
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        (scripted, transport)
    }

    // Incremental over the one-chunk ScriptedTransport (default `send_streaming`)
    // yields the SAME events and final message as the buffered `stream`, and
    // builds the same threaded request.
    #[test]
    fn backend_stream_incremental_matches_buffered_over_scripted() {
        let model = default_model();
        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };

        let (_scripted_buffered, transport_buffered) = scripted_multi_frame();
        let backend_buffered = GoogleVertexBackend::new(transport_buffered, fake_clock());
        let buffered = backend_buffered.stream(&model, &user_context(), Some(&options), None);

        let (scripted, transport) = scripted_multi_frame();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let mut reader = backend.stream_incremental(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(options.clone())),
            None,
        );
        let events: Vec<AssistantMessageEvent> = reader.by_ref().collect();

        assert_eq!(events, buffered.events);
        assert_eq!(
            reader.result().and_then(|r| r.as_ref().ok()),
            Some(&buffered.message)
        );
        assert_eq!(
            buffered.message.content,
            vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                text_signature: None,
            }]
        );

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, EXPRESS_STREAM_URL);
        assert_eq!(
            requests[0]
                .headers
                .get("x-goog-api-key")
                .map(String::as_str),
            Some("AIzaSyExampleRealisticLookingApiKey123456")
        );
    }

    /// A transport whose `send_streaming` splits the SSE body into one chunk per
    /// frame and sleeps `delay` before yielding each, so the reader's per-frame
    /// PULL timing is observable. Its buffered `send` returns the whole body.
    struct SleepingStreamTransport {
        body: String,
        delay: Duration,
    }

    struct SleepingChunks {
        frames: std::vec::IntoIter<Vec<u8>>,
        delay: Duration,
    }

    impl Iterator for SleepingChunks {
        type Item = io::Result<Vec<u8>>;

        fn next(&mut self) -> Option<Self::Item> {
            let bytes = self.frames.next()?;
            std::thread::sleep(self.delay);
            Some(Ok(bytes))
        }
    }

    impl HttpTransport for SleepingStreamTransport {
        fn send(&self, _request: &HttpRequest) -> io::Result<HttpResponse> {
            Ok(HttpResponse::ok(self.body.clone()))
        }

        fn send_streaming(&self, _request: &HttpRequest) -> io::Result<HttpStreamResponse<'_>> {
            let frames: Vec<Vec<u8>> = self
                .body
                .split("\n\n")
                .filter(|part| !part.is_empty())
                .map(|part| format!("{part}\n\n").into_bytes())
                .collect();
            Ok(HttpStreamResponse {
                status: 200,
                headers: BTreeMap::new(),
                chunks: Box::new(SleepingChunks {
                    frames: frames.into_iter(),
                    delay: self.delay,
                }),
            })
        }
    }

    // Over a per-frame sleeping transport, the yielded events span multiple
    // sleeping chunks -- non-zero inter-event spread -- while resolving to the
    // same "Hello!" message as the buffered path.
    #[test]
    fn backend_stream_incremental_streams_with_inter_event_spread() {
        let delay = Duration::from_millis(12);
        let transport: Arc<dyn HttpTransport> = Arc::new(SleepingStreamTransport {
            body: multi_frame_hello_sse_body(),
            delay,
        });
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };

        let mut reader = backend.stream_incremental(
            &default_model(),
            &user_context(),
            Some(&SimpleStreamOptions::from_base(options.clone())),
            None,
        );
        let start = Instant::now();
        let mut stamped: Vec<(Duration, AssistantMessageEvent)> = Vec::new();
        for event in reader.by_ref() {
            stamped.push((start.elapsed(), event));
        }

        assert!(matches!(
            stamped.last().map(|(_, e)| e),
            Some(AssistantMessageEvent::Done { .. })
        ));
        assert_eq!(
            reader
                .result()
                .and_then(|r| r.as_ref().ok())
                .map(|m| m.content.clone()),
            Some(vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                text_signature: None,
            }])
        );

        assert!(stamped.len() >= 2);
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread from per-frame streaming, got {spread:?}",
        );
    }

    // #192: `StreamOptions.temperature` and `StreamOptions.max_tokens` thread into
    // the outgoing `generationConfig` (`config.temperature` /
    // `config.maxOutputTokens`); `metadata` is per pi NOT mapped for Vertex.
    #[test]
    fn backend_threads_stream_options_into_generation_config() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        let mut metadata = BTreeMap::new();
        metadata.insert("user_id".to_string(), json!("u-123"));
        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            temperature: Some(0.42),
            max_tokens: Some(1234),
            metadata: Some(metadata),
            ..StreamOptions::default()
        };
        backend.stream(&default_model(), &user_context(), Some(&options), None);

        let requests = scripted.requests();
        let body: serde_json::Value =
            serde_json::from_str(requests[0].body.as_deref().expect("body")).expect("json body");
        let config = &body["config"];
        assert_eq!(config["temperature"], json!(0.42));
        assert_eq!(config["maxOutputTokens"], json!(1234));
        // metadata is not consumed by the Google dialect in pi.
        assert!(config.get("metadata").is_none());
        assert!(!requests[0].body.as_deref().unwrap().contains("user_id"));
    }

    // #192 precedence: with no `StreamOptions.max_tokens`, the model's `maxTokens`
    // default fills `maxOutputTokens`; a `StreamOptions.max_tokens` overrides it.
    #[test]
    fn backend_max_tokens_prefers_stream_options_over_model_default() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());

        // No StreamOptions.max_tokens -> model default (8192) is used.
        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };
        backend.stream(&default_model(), &user_context(), Some(&options), None);
        let body: serde_json::Value =
            serde_json::from_str(scripted.requests()[0].body.as_deref().expect("body"))
                .expect("json body");
        assert_eq!(body["config"]["maxOutputTokens"], json!(8192));
    }

    // --- streamSimple reasoning lowering (google-vertex.ts:301-584) ---

    /// A reasoning-enabled vertex `Model<Value>` with the given `id`, over the
    /// Express (placeholder-base-URL) endpoint. With no `thinkingLevelMap`,
    /// `clampThinkingLevel` treats `off..high` as all supported.
    fn vertex_reasoning_model(id: &str) -> Model {
        serde_json::from_value(json!({
            "id": id,
            "name": id,
            "api": "google-vertex",
            "provider": "google-vertex",
            "baseUrl": "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap()
    }

    fn express_options() -> StreamOptions {
        StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        }
    }

    fn request_body(scripted: &ScriptedTransport) -> serde_json::Value {
        serde_json::from_str(scripted.requests()[0].body.as_deref().expect("body"))
            .expect("json body")
    }

    fn simple_with_reasoning(level: ThinkingLevel) -> SimpleStreamOptions {
        SimpleStreamOptions::new(express_options(), Some(level), None)
    }

    // A reasoning level on a budget model lowers to `thinkingConfig.thinkingBudget`
    // = pi's vertex `getGoogleBudget` value (2.5-pro / high = 32768) with
    // `includeThoughts: true` (google-vertex.ts:328-333, :563-570).
    #[test]
    fn stream_simple_budget_model_lowers_thinking_budget() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model = vertex_reasoning_model("gemini-2.5-pro");

        backend.stream_simple(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::High)),
            None,
        );

        let thinking = &request_body(&scripted)["config"]["thinkingConfig"];
        assert_eq!(thinking["includeThoughts"], json!(true));
        assert_eq!(thinking["thinkingBudget"], json!(32768));
        assert!(thinking.get("thinkingLevel").is_none());
    }

    // A reasoning level on a gemini-3/level model lowers to
    // `thinkingConfig.thinkingLevel` = pi's `getGemini3ThinkingLevel` enum string
    // (google-vertex.ts:318-325, :528-552). The vertex `THINKING_LEVEL_MAP` enum
    // serializes to the same wire strings as gen-ai's pass-through.
    #[test]
    fn stream_simple_level_model_lowers_thinking_level() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model = vertex_reasoning_model("gemini-3-flash-preview");

        backend.stream_simple(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::High)),
            None,
        );

        let thinking = &request_body(&scripted)["config"]["thinkingConfig"];
        assert_eq!(thinking["includeThoughts"], json!(true));
        assert_eq!(thinking["thinkingLevel"], json!("HIGH"));
        assert!(thinking.get("thinkingBudget").is_none());
    }

    // vertex-specific: a non-2.5, non-gemini-3 budget model falls through both
    // tables to the `-1` (dynamic) default — unlike gen-ai there is NO flash-lite
    // table (google-vertex.ts:554-584).
    #[test]
    fn stream_simple_budget_default_is_negative_one() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model = vertex_reasoning_model("gemini-2.0-flash");

        backend.stream_simple(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::High)),
            None,
        );

        assert_eq!(
            request_body(&scripted)["config"]["thinkingConfig"]["thinkingBudget"],
            json!(-1),
        );
    }

    // google-specific: when `clampThinkingLevel` yields `off`, pi maps `off ⇒ high`
    // rather than omitting (google-vertex.ts:315). The request carries the HIGH
    // budget, NOT a disabled/omitted thinking config.
    #[test]
    fn stream_simple_off_clamp_maps_to_high_not_omitted() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model: Model = serde_json::from_value(json!({
            "id": "gemini-2.5-flash",
            "name": "Gemini 2.5 Flash",
            "api": "google-vertex",
            "provider": "google-vertex",
            "baseUrl": "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
            "reasoning": true,
            "thinkingLevelMap": {
                "minimal": null, "low": null, "medium": null, "high": null,
            },
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap();

        backend.stream_simple(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::Minimal)),
            None,
        );

        let thinking = &request_body(&scripted)["config"]["thinkingConfig"];
        assert_eq!(thinking["includeThoughts"], json!(true));
        // vertex 2.5-flash high budget, proving off => high (not omitted).
        assert_eq!(thinking["thinkingBudget"], json!(24576));
    }

    // No reasoning on a NON-reasoning model emits no `thinkingConfig`: the request
    // is byte-identical to the raw `stream` path.
    #[test]
    fn stream_simple_no_reasoning_is_byte_identical_to_raw() {
        let model: Model = serde_json::from_value(json!({
            "id": "gemini-2.5-flash",
            "name": "Gemini 2.5 Flash",
            "api": "google-vertex",
            "provider": "google-vertex",
            "baseUrl": "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap();
        let base = express_options();

        let (scripted_raw, transport_raw) = scripted_hello();
        let backend_raw = GoogleVertexBackend::new(transport_raw, fake_clock());
        backend_raw.stream(&model, &user_context(), Some(&base), None);

        let (scripted_simple, transport_simple) = scripted_hello();
        let backend_simple = GoogleVertexBackend::new(transport_simple, fake_clock());
        backend_simple.stream_simple(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(base.clone())),
            None,
        );

        assert_eq!(
            scripted_raw.requests()[0].body,
            scripted_simple.requests()[0].body,
        );
        assert!(request_body(&scripted_simple)["config"]
            .get("thinkingConfig")
            .is_none());
    }

    // No reasoning on a REASONING model emits pi's `getDisabledThinkingConfig`
    // (2.5 => `thinkingBudget: 0`), matching pi's `thinking: { enabled: false }`
    // path (google-vertex.ts:307-311, :481-482, :524-525).
    #[test]
    fn stream_simple_no_reasoning_on_reasoning_model_emits_disabled_config() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model = vertex_reasoning_model("gemini-2.5-flash");

        backend.stream_simple(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(express_options())),
            None,
        );

        assert_eq!(
            request_body(&scripted)["config"]["thinkingConfig"],
            json!({ "thinkingBudget": 0 }),
        );
    }

    // --- incremental streamSimple reasoning lowering (mirrors the buffered
    // `stream_simple` lowering above; pi applies the same shaping on its single
    // streaming path regardless of buffered vs incremental) ---

    /// Drive `stream_incremental` to completion so the request is placed on the
    /// scripted transport, discarding the streamed events.
    fn drain_incremental(reader: &mut AssistantEventReader<'_>) {
        reader.by_ref().for_each(drop);
    }

    // Incremental: a reasoning level on a budget model lowers to
    // `thinkingConfig.thinkingBudget` = pi's vertex `getGoogleBudget` (2.5-pro /
    // high = 32768) with `includeThoughts: true`, exactly as the buffered path.
    #[test]
    fn stream_incremental_budget_model_lowers_thinking_budget() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model = vertex_reasoning_model("gemini-2.5-pro");

        let mut reader = backend.stream_incremental(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::High)),
            None,
        );
        drain_incremental(&mut reader);

        let thinking = &request_body(&scripted)["config"]["thinkingConfig"];
        assert_eq!(thinking["includeThoughts"], json!(true));
        assert_eq!(thinking["thinkingBudget"], json!(32768));
        assert!(thinking.get("thinkingLevel").is_none());
    }

    // Incremental: a reasoning level on a gemini-3/level model lowers to
    // `thinkingConfig.thinkingLevel` (pi's `getGemini3ThinkingLevel` enum), not a
    // budget.
    #[test]
    fn stream_incremental_level_model_lowers_thinking_level() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model = vertex_reasoning_model("gemini-3-flash-preview");

        let mut reader = backend.stream_incremental(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::High)),
            None,
        );
        drain_incremental(&mut reader);

        let thinking = &request_body(&scripted)["config"]["thinkingConfig"];
        assert_eq!(thinking["includeThoughts"], json!(true));
        assert_eq!(thinking["thinkingLevel"], json!("HIGH"));
        assert!(thinking.get("thinkingBudget").is_none());
    }

    // Incremental vertex-specific: a non-2.5, non-gemini-3 budget model falls
    // through both tables to the `-1` (dynamic) default — no flash-lite table.
    #[test]
    fn stream_incremental_budget_default_is_negative_one() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model = vertex_reasoning_model("gemini-2.0-flash");

        let mut reader = backend.stream_incremental(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::High)),
            None,
        );
        drain_incremental(&mut reader);

        assert_eq!(
            request_body(&scripted)["config"]["thinkingConfig"]["thinkingBudget"],
            json!(-1),
        );
    }

    // Incremental google-specific: a clamp to `off` maps `off ⇒ high` rather than
    // omitting — the streamed request carries the HIGH budget, NOT a
    // disabled/omitted thinking config (same as the buffered path).
    #[test]
    fn stream_incremental_off_clamp_maps_to_high_not_omitted() {
        let (scripted, transport) = scripted_hello();
        let backend = GoogleVertexBackend::new(transport, fake_clock());
        let model: Model = serde_json::from_value(json!({
            "id": "gemini-2.5-flash",
            "name": "Gemini 2.5 Flash",
            "api": "google-vertex",
            "provider": "google-vertex",
            "baseUrl": "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
            "reasoning": true,
            "thinkingLevelMap": {
                "minimal": null, "low": null, "medium": null, "high": null,
            },
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap();

        let mut reader = backend.stream_incremental(
            &model,
            &user_context(),
            Some(&simple_with_reasoning(ThinkingLevel::Minimal)),
            None,
        );
        drain_incremental(&mut reader);

        let thinking = &request_body(&scripted)["config"]["thinkingConfig"];
        assert_eq!(thinking["includeThoughts"], json!(true));
        // vertex 2.5-flash high budget, proving off => high (not omitted).
        assert_eq!(thinking["thinkingBudget"], json!(24576));
    }

    // Incremental: no reasoning is byte-identical to the raw incremental path (no
    // `thinkingConfig` emitted), so the widening is inert absent a reasoning level.
    #[test]
    fn stream_incremental_no_reasoning_is_byte_identical_to_raw() {
        let model: Model = serde_json::from_value(json!({
            "id": "gemini-2.5-flash",
            "name": "Gemini 2.5 Flash",
            "api": "google-vertex",
            "provider": "google-vertex",
            "baseUrl": "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap();
        let base = express_options();

        let (scripted_raw, transport_raw) = scripted_hello();
        let backend_raw = GoogleVertexBackend::new(transport_raw, fake_clock());
        let mut reader_raw = backend_raw.stream_incremental(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(base.clone())),
            None,
        );
        drain_incremental(&mut reader_raw);

        let (scripted_simple, transport_simple) = scripted_hello();
        let backend_simple = GoogleVertexBackend::new(transport_simple, fake_clock());
        let mut reader_simple = backend_simple.stream_incremental(
            &model,
            &user_context(),
            Some(&SimpleStreamOptions::from_base(base.clone())),
            None,
        );
        drain_incremental(&mut reader_simple);

        assert_eq!(
            scripted_raw.requests()[0].body,
            scripted_simple.requests()[0].body,
        );
        assert!(request_body(&scripted_simple)["config"]
            .get("thinkingConfig")
            .is_none());
    }
}

/// A loopback integration test over the real `reqwest`-backed transport, gated
/// behind `native-http` (the default build stays reqwest-free). It stands up a
/// one-shot HTTP server on `127.0.0.1` serving the `hello` SSE body and drives
/// the backend through [`ReqwestTransport`] with `.no_proxy()` (required in the
/// sandbox).
#[cfg(all(test, feature = "native-http"))]
mod native_http_tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use serde_json::json;

    use crate::seams::clock::FakeClock;
    use crate::seams::http_reqwest::ReqwestTransport;
    use crate::types::ContentBlock;

    /// Read one HTTP/1.1 request off `stream` up to the header terminator, then
    /// drain any declared body so the client's write completes cleanly.
    fn drain_request(stream: &mut TcpStream) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos;
            }
            let n = stream.read(&mut tmp).expect("read request");
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_length = header_text
            .split("\r\n")
            .find_map(|line| {
                line.split_once(':').and_then(|(k, v)| {
                    (k.trim().eq_ignore_ascii_case("content-length"))
                        .then(|| v.trim().parse::<usize>().unwrap_or(0))
                })
            })
            .unwrap_or(0);
        let mut body_len = buf.len().saturating_sub(header_end + 4);
        while body_len < content_length {
            let n = stream.read(&mut tmp).expect("read body");
            if n == 0 {
                break;
            }
            body_len += n;
        }
    }

    /// A Vertex model whose base URL points at the loopback server (already
    /// carrying the `/v1` version so no further version segment is appended).
    fn loopback_model(base_url: &str) -> Model {
        serde_json::from_value(json!({
            "id": "gemini-3-flash-preview",
            "name": "Gemini 3 Flash Preview",
            "api": "google-vertex",
            "provider": "google-vertex",
            "baseUrl": base_url,
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000000,
            "maxTokens": 8192,
        }))
        .unwrap()
    }

    fn user_context() -> Context {
        serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "Hi", "timestamp": 0 }],
        }))
        .unwrap()
    }

    #[test]
    fn backend_runs_over_reqwest_loopback() {
        let body = hello_sse_body();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("local addr"));

        let server_body = body.clone();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            drain_request(&mut stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                server_body.len(),
                server_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().ok();
        });

        let transport: Arc<dyn HttpTransport> =
            Arc::new(ReqwestTransport::builder().no_proxy().build());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        let backend = GoogleVertexBackend::new(transport, clock);

        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };

        let result = backend.stream(
            &loopback_model(&base_url),
            &user_context(),
            Some(&options),
            None,
        );
        handle.join().expect("server thread");

        assert_eq!(result.message.stop_reason, StopReason::Stop);
        assert_eq!(
            result.message.content,
            vec![ContentBlock::Text {
                text: "Hello".to_string(),
                text_signature: None,
            }]
        );
    }

    // Incremental over a real reqwest loopback whose server writes each SSE frame
    // with a bounded sleep between flushes: the reader's per-frame PULL surfaces
    // that spacing as a non-zero inter-event spread at the consumer.
    #[test]
    fn backend_stream_incremental_over_reqwest_loopback_has_spread() {
        use std::time::{Duration, Instant};

        let delay = Duration::from_millis(20);
        let frames: Vec<String> = multi_frame_hello_sse_body()
            .split("\n\n")
            .filter(|part| !part.is_empty())
            .map(|part| format!("{part}\n\n"))
            .collect();
        let total_len: usize = frames.iter().map(|f| f.len()).sum();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("local addr"));

        let server_frames = frames.clone();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            drain_request(&mut stream);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total_len}\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).expect("write head");
            stream.flush().ok();
            for (i, frame) in server_frames.iter().enumerate() {
                if i > 0 {
                    thread::sleep(delay);
                }
                stream.write_all(frame.as_bytes()).expect("write frame");
                stream.flush().ok();
            }
        });

        let transport: Arc<dyn HttpTransport> =
            Arc::new(ReqwestTransport::builder().no_proxy().build());
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(1_700_000_000_000));
        let backend = GoogleVertexBackend::new(transport, clock);

        let options = StreamOptions {
            api_key: Some("AIzaSyExampleRealisticLookingApiKey123456".to_string()),
            ..StreamOptions::default()
        };

        let mut reader = backend.stream_incremental(
            &loopback_model(&base_url),
            &user_context(),
            Some(&SimpleStreamOptions::from_base(options.clone())),
            None,
        );
        let start = Instant::now();
        let mut stamped: Vec<(Duration, AssistantMessageEvent)> = Vec::new();
        for event in reader.by_ref() {
            stamped.push((start.elapsed(), event));
        }
        handle.join().expect("server thread");

        assert!(matches!(
            stamped.last().map(|(_, e)| e),
            Some(AssistantMessageEvent::Done { .. })
        ));
        assert_eq!(
            reader
                .result()
                .and_then(|r| r.as_ref().ok())
                .map(|m| m.content.clone()),
            Some(vec![ContentBlock::Text {
                text: "Hello!".to_string(),
                text_signature: None,
            }])
        );

        assert!(stamped.len() >= 2);
        let spread = stamped.last().unwrap().0 - stamped.first().unwrap().0;
        assert!(
            spread >= delay,
            "expected non-zero inter-event spread over reqwest loopback, got {spread:?}",
        );
    }
}
