// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// `api/openrouter-images.ts`: the pre-start `AssistantImages` error shell, the
// request-shaping helpers, and the usage/cost math mirror the shapes in the
// chat completions dialect and the other image runtime files by design; the
// clone detector reads the shared boundary-type construction as duplicative.

//! The OpenRouter image-generation dialect — the Rust port of pi-ai's
//! `api/openrouter-images.ts` (`packages/ai/src/api/openrouter-images.ts`).
//!
//! pi builds an OpenAI Chat Completions **non-streaming** request with
//! `modalities: ["image"]` (or `["image", "text"]`), parses the returned
//! `choices[0].message.images[]` `data:` URLs into [`ImagesOutputContent`] image
//! blocks, and folds usage into pi's [`Usage`] / cost shape. It is the only real
//! HTTP file in the image runtime.
//!
//! This port drives the plain non-streaming POST through the injected
//! [`HttpTransport`] seam (the analog of pi's `new OpenAI(...)` client) and
//! reuses the ported [`error_body`](crate::utils::error_body),
//! [`headers`](crate::utils::headers), and
//! [`sanitize_unicode`](crate::utils::sanitize_unicode) helpers. pi's separate
//! lazy wrapper (`api/openrouter-images.lazy.ts`) collapses into the
//! [`OpenrouterImagesBackend`] [`ProviderImages`] adapter here — Rust has no
//! code-splitting, so the dynamic import becomes a direct call.

use std::sync::{Arc, OnceLock};

use regex::Regex;
use serde_json::{json, Value};

use crate::cost::calculate_cost_with;
use crate::seams::clock::Clock;
use crate::seams::http::{HttpRequest, HttpTransport};
use crate::seams::provider::AbortSignal;
use crate::types::{
    AssistantImages, ImagesContext, ImagesInputContent, ImagesModel, ImagesOptions,
    ImagesStopReason, Modality, ProviderImages, Usage, UsageCost,
};
use crate::utils::error_body::{
    format_provider_error, normalize_provider_error, SdkError, ThrownError,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;

/// A [`ProviderImages`] adapter that runs an OpenRouter image-generation turn
/// over an injected [`HttpTransport`], stamping the result with an injected
/// [`Clock`].
///
/// This is the collapse of pi's `api/openrouter-images.lazy.ts` wrapper: instead
/// of a dynamic import returning a `ProviderImages`, the concrete backend holds
/// the transport/clock and calls [`generate_images`] directly.
pub struct OpenrouterImagesBackend {
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl OpenrouterImagesBackend {
    /// Build a backend that performs requests over `transport` and stamps each
    /// result with `clock.now_ms()` (pi's `Date.now()`, taken through the clock
    /// seam rather than the wall clock).
    pub fn new(transport: Arc<dyn HttpTransport>, clock: Arc<dyn Clock>) -> Self {
        Self { transport, clock }
    }
}

impl ProviderImages for OpenrouterImagesBackend {
    fn generate_images(
        &self,
        model: &ImagesModel,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
        signal: Option<&AbortSignal>,
    ) -> AssistantImages {
        // `signal` is threaded through the dispatch layers (pi carries it inside
        // `ImagesOptions.signal`, which the serializable options port defers to a
        // separate seam parameter — see [`crate::types::ProviderImages`]).
        generate_images(
            self.transport.as_ref(),
            self.clock.now_ms(),
            model,
            context,
            options,
            signal,
        )
    }
}

/// Generate images through OpenRouter, pi's `generateImages`
/// (`api/openrouter-images.ts:38-119`).
///
/// `now_ms` is pi's `Date.now()` (the [`AssistantImages::timestamp`]) sourced
/// through the clock seam. `signal` is pi's `options.signal`, threaded as a
/// separate seam parameter (see [`crate::types::ProviderImages`]). Never panics:
/// request/runtime failures are encoded in the returned [`AssistantImages`] with
/// a `stopReason` of `error`/`aborted`.
pub fn generate_images(
    transport: &dyn HttpTransport,
    now_ms: i64,
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    signal: Option<&AbortSignal>,
) -> AssistantImages {
    let mut output = AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: now_ms,
    };

    match run(transport, model, context, options, signal, &mut output) {
        Ok(()) => output,
        Err(error) => {
            let error = *error;
            // pi: stopReason = options?.signal?.aborted ? "aborted" : "error".
            output.stop_reason = if signal.is_some_and(AbortSignal::is_aborted) {
                ImagesStopReason::Aborted
            } else {
                ImagesStopReason::Error
            };
            output.error_message = Some(format_provider_error(
                &normalize_provider_error(&error),
                None,
            ));
            output
        }
    }
}

/// The `try` body of pi's `generateImages`. Populates `output` on success and
/// returns the thrown error (already probed into a [`ThrownError`]) on failure,
/// letting the caller apply the shared `catch` shell.
fn run(
    transport: &dyn HttpTransport,
    model: &ImagesModel,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    signal: Option<&AbortSignal>,
    output: &mut AssistantImages,
) -> Result<(), Box<ThrownError>> {
    // pi: const apiKey = options?.apiKey; if (!apiKey) throw ...
    let api_key = options
        .and_then(|o| o.api_key.as_deref())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| thrown(format!("No API key for provider: {}", model.provider)))?;

    let request = build_request(model, context, api_key, options);

    // pi passes options.signal to the OpenAI client, which aborts the fetch. The
    // scripted transport does not observe the signal, so honor it here: an
    // already-aborted request throws the same "Request aborted" the SDK surfaces.
    if signal.is_some_and(AbortSignal::is_aborted) {
        return Err(thrown("Request aborted".to_string()));
    }

    let response = transport
        .send(&request)
        .map_err(|error| thrown(error.to_string()))?;

    if !response.is_ok() {
        // pi's OpenAI client throws an APIError carrying status + body.
        return Err(Box::new(ThrownError::Error(SdkError {
            message: format!("Request failed with status {}", response.status),
            status: Some(u32::from(response.status)),
            body: Some(response.body.clone()),
            ..SdkError::default()
        })));
    }

    let body: Value = serde_json::from_str(&response.body)
        .map_err(|error| thrown(format!("Failed to parse response: {error}")))?;

    // pi: output.responseId = imageResponse.id.
    if let Some(id) = body.get("id").and_then(Value::as_str) {
        output.response_id = Some(id.to_string());
    }
    // pi: if (imageResponse.usage) output.usage = parseUsage(usage, model).
    if let Some(usage) = body.get("usage") {
        output.usage = Some(parse_usage(usage, model));
    }

    // pi: const choice = imageResponse.choices[0].
    if let Some(choice) = body.get("choices").and_then(|c| c.get(0)) {
        let message = choice.get("message");
        // pi: text content, pushed only when a non-empty string.
        if let Some(content) = message
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
        {
            if !content.is_empty() {
                output.output.push(ImagesInputContent::Text {
                    text: content.to_string(),
                    text_signature: None,
                });
            }
        }
        // pi: for (const image of choice.message.images ?? []) { ... }.
        if let Some(images) = message
            .and_then(|m| m.get("images"))
            .and_then(Value::as_array)
        {
            for image in images {
                if let Some((mime_type, data)) = parse_data_image(image) {
                    output
                        .output
                        .push(ImagesInputContent::Image { data, mime_type });
                }
            }
        }
    }

    Ok(())
}

/// Assemble the non-streaming chat-completions POST, pi's `createClient` +
/// `buildParams` (`api/openrouter-images.ts:121-171`).
fn build_request(
    model: &ImagesModel,
    context: &ImagesContext,
    api_key: &str,
    options: Option<&ImagesOptions>,
) -> HttpRequest {
    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let body = build_params(model, context);

    let mut request = HttpRequest::post(url, body.to_string())
        .with_header("authorization", format!("Bearer {api_key}"))
        .with_header("content-type", "application/json");

    // pi createClient: defaultHeaders = providerHeadersToRecord({ ...model.headers,
    // ...optionsHeaders }). Both carriers hold plain string values here, so the
    // provider-headers null-drop is a no-op; the merge (options win per key) is
    // applied directly.
    if let Some(headers) = &model.headers {
        for (name, value) in headers {
            request = request.with_header(name.clone(), value.clone());
        }
    }
    if let Some(headers) = options.and_then(|o| o.headers.as_ref()) {
        for (name, value) in headers {
            request = request.with_header(name.clone(), value.clone());
        }
    }

    request
}

/// pi's `buildParams` (`api/openrouter-images.ts:146-171`): the non-streaming
/// chat-completions request payload with the image modalities set.
fn build_params(model: &ImagesModel, context: &ImagesContext) -> Value {
    let content: Vec<Value> = context
        .input
        .iter()
        .map(|item| match item {
            ImagesInputContent::Text { text, .. } => json!({
                "type": "text",
                "text": sanitize_surrogates(text),
            }),
            ImagesInputContent::Image { data, mime_type } => json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime_type};base64,{data}") },
            }),
        })
        .collect();

    // pi: modalities = model.output.includes("text") ? ["image","text"] : ["image"].
    let modalities: Vec<&str> = if model.output.contains(&Modality::Text) {
        vec!["image", "text"]
    } else {
        vec!["image"]
    };

    json!({
        "model": model.id,
        "messages": [{ "role": "user", "content": content }],
        "stream": false,
        "modalities": modalities,
    })
}

/// pi's inline `data:` URL parse (`api/openrouter-images.ts:100-110`): reads the
/// `image_url` (string or `{ url }`), keeps only `data:` URLs, and splits out
/// the mime type + base64 payload.
fn parse_data_image(image: &Value) -> Option<(String, String)> {
    let image_url = match image.get("image_url") {
        Some(Value::String(url)) => url.as_str(),
        Some(Value::Object(object)) => object.get("url").and_then(Value::as_str)?,
        _ => return None,
    };
    if !image_url.starts_with("data:") {
        return None;
    }
    let captures = data_url_regex().captures(image_url)?;
    Some((
        captures.get(1)?.as_str().to_string(),
        captures.get(2)?.as_str().to_string(),
    ))
}

/// pi's `/^data:([^;]+);base64,(.+)$/`, compiled once.
fn data_url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^data:([^;]+);base64,(.+)$").expect("valid data-url regex"))
}

/// pi's `parseUsage` (`api/openrouter-images.ts:173-215`): folds OpenRouter's
/// token counts into pi's [`Usage`] / cost shape.
fn parse_usage(raw: &Value, model: &ImagesModel) -> Usage {
    let get = |path: &[&str]| -> u64 {
        let mut cur = raw;
        for key in path {
            match cur.get(key) {
                Some(next) => cur = next,
                None => return 0,
            }
        }
        cur.as_u64().unwrap_or(0)
    };

    let prompt_tokens = get(&["prompt_tokens"]);
    let reported_cached = get(&["prompt_tokens_details", "cached_tokens"]);
    let cache_write = get(&["prompt_tokens_details", "cache_write_tokens"]);
    // pi: cacheWrite > 0 ? max(0, reportedCached - cacheWrite) : reportedCached.
    let cache_read = if cache_write > 0 {
        reported_cached.saturating_sub(cache_write)
    } else {
        reported_cached
    };
    let input = prompt_tokens
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    let output = get(&["completion_tokens"]);

    // pi inlines the `(rate / 1e6) * tokens` breakdown; it is byte-for-byte the
    // crate-wide [`calculate_cost_with`] path for an image [`ModelCost`] (no
    // tiers, no 1h cache-write), so route through the shared helper like every
    // other dialect does.
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read + cache_write,
        cost: UsageCost::default(),
    };
    usage.cost = calculate_cost_with(&model.cost, &usage);
    usage
}

/// Build a boxed [`ThrownError`] from a plain error message, mirroring a thrown
/// JS `Error(message)` that `normalizeProviderError` probes into a message-only
/// normalized error. Boxed to keep the `run` result small (clippy
/// `result_large_err`).
fn thrown(message: String) -> Box<ThrownError> {
    Box::new(ThrownError::Error(SdkError {
        message,
        ..SdkError::default()
    }))
}

#[cfg(test)]
mod tests;
