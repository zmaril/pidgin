//! Node-API surface for the faux provider (`registerFauxProvider`).
//!
//! This exposes the Rust faux provider ([`pidgin_ai::providers::faux`]) to pi's
//! faux-driven tests. It is the `FauxCore` half of pi's `createFauxCore`
//! (`vendor/pi/packages/ai/src/providers/faux.ts`): the deterministic delta
//! streaming and prompt-cache accounting, driven from JS.
//!
//! # Why no threadsafe callback is needed here
//!
//! `notes/startup/testing-strategy.md` §5 flags "driving Rust streaming from a JS
//! callback" as the trickiest seam. This design sidesteps it: pi's response queue
//! is a list of *fixed messages or JS factory functions*. The JS shim keeps the
//! queue and resolves the next step **in JavaScript** — calling a factory closure
//! directly, no callback into Rust — then hands the already-resolved
//! `AssistantMessage` to [`FauxCore::stream_resolved`]. Rust owns only the
//! deterministic, stateless-per-call streaming plus the cross-call prompt cache
//! and call count. So the JS-closure case that would otherwise require a
//! `ThreadsafeFunction` is served without one; the boundary is plain synchronous
//! JSON calls, mirroring the Stage-2 Anthropic shim.

// straitjacket-allow-file[:duplication] — the napi entry points share one faithful
// parse-JSON / build-seams / call-provider / serialize shape at the Node boundary;
// the near-identical method bodies mirror pi's surface and are kept distinct.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::Deserialize;

use pidgin_ai::providers::faux::{FauxModelDefinition, FauxProvider, RegisterFauxProviderOptions};
use pidgin_ai::seams::clock::FakeClock;
use pidgin_ai::seams::provider::{AbortSignal, Provider};
use pidgin_ai::types::{AssistantMessage, Context, Modality, ModelCost, StreamOptions};

/// JSON shape of pi's `RegisterFauxProviderOptions` (`faux.ts:105-114`), parsed
/// at the boundary and mapped onto the builder options.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct OptionsJson {
    api: Option<String>,
    provider: Option<String>,
    models: Option<Vec<ModelDefJson>>,
    tokens_per_second: Option<f64>,
    token_size: Option<TokenSizeJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TokenSizeJson {
    min: Option<u32>,
    max: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelDefJson {
    id: String,
    name: Option<String>,
    reasoning: Option<bool>,
    input: Option<Vec<Modality>>,
    cost: Option<ModelCost>,
    context_window: Option<u32>,
    max_tokens: Option<u32>,
}

fn build_options(json: &str) -> Result<RegisterFauxProviderOptions> {
    let parsed: OptionsJson = if json.trim().is_empty() {
        OptionsJson::default()
    } else {
        serde_json::from_str(json)
            .map_err(|e| Error::from_reason(format!("invalid faux options: {e}")))?
    };
    let token_size = parsed.token_size.unwrap_or_default();
    Ok(RegisterFauxProviderOptions {
        api: parsed.api,
        provider: parsed.provider,
        models: parsed.models.map(|defs| {
            defs.into_iter()
                .map(|d| FauxModelDefinition {
                    id: d.id,
                    name: d.name,
                    reasoning: d.reasoning,
                    input: d.input,
                    cost: d.cost,
                    context_window: d.context_window.map(u64::from),
                    max_tokens: d.max_tokens.map(u64::from),
                })
                .collect()
        }),
        tokens_per_second: parsed.tokens_per_second,
        token_size_min: token_size.min.map(u64::from),
        token_size_max: token_size.max.map(u64::from),
    })
}

fn parse_context(json: &str) -> Result<Context> {
    serde_json::from_str(json).map_err(|e| Error::from_reason(format!("invalid context: {e}")))
}

fn parse_options(json: Option<String>) -> Result<Option<StreamOptions>> {
    match json {
        None => Ok(None),
        Some(s) if s.trim().is_empty() || s == "null" => Ok(None),
        Some(s) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| Error::from_reason(format!("invalid stream options: {e}"))),
    }
}

/// The Rust-backed faux core, exposed to JavaScript as `FauxCore`.
///
/// The JS `registerFauxProvider` shim constructs one of these, keeps the response
/// queue in JS, and calls [`FauxCore::stream_resolved`] (or
/// [`FauxCore::empty_queue_result`]) per stream. Cross-call state (call count and
/// prompt cache) lives in the wrapped [`FauxProvider`].
#[napi(js_name = "FauxCore")]
pub struct FauxCore {
    inner: FauxProvider,
    /// The settable clock shared with `inner`; JS pushes `Date.now()` into it via
    /// [`FauxCore::set_now_ms`] so the empty-queue/aborted timestamps Rust stamps
    /// track JS-controlled time (vitest fake timers and real timers alike).
    clock: FakeClock,
}

#[napi]
impl FauxCore {
    /// Build a faux core from pi's `RegisterFauxProviderOptions`, JSON-encoded.
    #[napi(constructor)]
    pub fn new(options_json: String) -> Result<Self> {
        let (inner, clock) = FauxProvider::with_fake_clock(build_options(&options_json)?);
        Ok(Self { inner, clock })
    }

    /// Set the `now` (epoch milliseconds) the provider reads when stamping the
    /// empty-queue/aborted message timestamps. The JS shim calls this with
    /// `Date.now()` before each stream so those timestamps track JS time.
    #[napi(js_name = "setNowMs")]
    pub fn set_now_ms(&self, now_ms: i64) {
        self.clock.set_now_ms(now_ms);
    }

    /// The provider's api id (pi's `core.api`).
    #[napi(js_name = "api")]
    pub fn api(&self) -> String {
        self.inner.api().to_string()
    }

    /// The model catalog as a JSON array (pi's `core.models`).
    #[napi(js_name = "modelsJson")]
    pub fn models_json(&self) -> Result<String> {
        serde_json::to_string(self.inner.models()).map_err(|e| Error::from_reason(e.to_string()))
    }

    /// pi's `getModel()`: the model with `id`, or the first model when `id` is
    /// omitted. Returns `null` when no model matches.
    #[napi(js_name = "getModelJson")]
    pub fn get_model_json(&self, id: Option<String>) -> Result<Option<String>> {
        match self.inner.get_model(id.as_deref()) {
            Some(model) => serde_json::to_string(&model)
                .map(Some)
                .map_err(|e| Error::from_reason(e.to_string())),
            None => Ok(None),
        }
    }

    /// Increment and return the call count (pi's `state.callCount++`). The JS
    /// shim calls this before resolving a response factory, matching pi's order.
    #[napi(js_name = "bumpCallCount")]
    pub fn bump_call_count(&self) -> i64 {
        self.inner.bump_call_count() as i64
    }

    /// The current call count without incrementing (pi's `state.callCount`).
    #[napi(js_name = "callCount")]
    pub fn call_count(&self) -> i64 {
        self.inner.call_count() as i64
    }

    /// Stream an already-resolved response message, returning the
    /// `{ events, message }` result as JSON. The JS shim passes the message it
    /// popped (or computed from a factory); Rust applies pi's clone + usage
    /// estimate + delta streaming. `aborted` reproduces the pre-aborted signal
    /// path (pi's `signal.aborted`).
    #[napi(js_name = "streamResolved")]
    pub fn stream_resolved(
        &self,
        model_json: String,
        context_json: String,
        options_json: Option<String>,
        message_json: String,
        aborted: bool,
    ) -> Result<String> {
        let model = serde_json::from_str(&model_json)
            .map_err(|e| Error::from_reason(format!("invalid model: {e}")))?;
        let context = parse_context(&context_json)?;
        let options = parse_options(options_json)?;
        let message: AssistantMessage = serde_json::from_str(&message_json)
            .map_err(|e| Error::from_reason(format!("invalid message: {e}")))?;
        let signal = if aborted {
            Some(AbortSignal::aborted())
        } else {
            None
        };
        let result = self.inner.stream_resolved(
            &model,
            &context,
            options.as_ref(),
            message,
            signal.as_ref(),
        );
        serde_json::to_string(&result).map_err(|e| Error::from_reason(e.to_string()))
    }

    /// The result pi streams when the response queue is empty (an `error`-stop
    /// message with usage estimated), as JSON.
    #[napi(js_name = "emptyQueueResult")]
    pub fn empty_queue_result(
        &self,
        model_json: String,
        context_json: String,
        options_json: Option<String>,
    ) -> Result<String> {
        let model = serde_json::from_str(&model_json)
            .map_err(|e| Error::from_reason(format!("invalid model: {e}")))?;
        let context = parse_context(&context_json)?;
        let options = parse_options(options_json)?;
        let result = self
            .inner
            .empty_queue_result(&model, &context, options.as_ref());
        serde_json::to_string(&result).map_err(|e| Error::from_reason(e.to_string()))
    }
}
