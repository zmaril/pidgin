//! Management client for a `llama.cpp` router server — a faithful port of
//! pi-coding-agent's `extensions/llama/client.ts`.
//!
//! Mirrors pi symbol-for-symbol: the [`LlamaModelStatus`] / [`LlamaModelInfo`] /
//! [`LlamaModelsResponse`] / [`LlamaModelEvent`] / [`LlamaProgress`] shapes, the
//! pure [`format_bytes`], [`normalize_llama_server_url`] and
//! [`llama_inference_url`] helpers, and the [`LlamaClient`] catalog/load/unload/
//! download requests including the `unload_and_wait` / `load_and_wait` /
//! `download_and_wait` polling loops and their `parse_load_progress` /
//! `parse_download_progress` progress parsers.
//!
//! Where pi calls `fetch`, this port issues the request through the injected
//! [`HttpTransport`] seam (`crates/pidgin-ai/src/seams/http.rs`) so tests can
//! script canned responses exactly as pi's `vi.stubGlobal("fetch")` does.
//!
//! # SSE deferral
//!
//! pi's `watch` / `loadAndWait` / `downloadAndWait` consume a live
//! `text/event-stream` via `response.body.getReader()`, decoding
//! `\n\n`-delimited `data:` frames. The buffered [`HttpTransport`] seam returns a
//! complete `String` body and cannot stream (see
//! `crates/pidgin-ai/src/seams/http_reqwest.rs`, "Buffered, not streaming"), so
//! the streaming reader is abstracted behind the local [`LlamaEventStream`] seam
//! and its concrete network-backed implementation is deferred. The catalog
//! polling that runs alongside the stream still flows through [`HttpTransport`].
// straitjacket-allow-file:duplication

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use pidgin_ai::seams::http::{HttpRequest, HttpTransport};
use pidgin_ai::seams::provider::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The lifecycle state of a router-managed model (`LlamaModelStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlamaModelStatus {
    /// Not loaded (`"unloaded"`).
    #[default]
    Unloaded,
    /// Loading into memory (`"loading"`).
    Loading,
    /// Loaded and ready to serve (`"loaded"`).
    Loaded,
    /// Downloading model weights (`"downloading"`).
    Downloading,
    /// Idle/sleeping (`"sleeping"`).
    Sleeping,
}

/// A model's `status` object (`LlamaModelInfo["status"]`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LlamaModelStatusInfo {
    /// The lifecycle state.
    pub value: LlamaModelStatus,
    /// The `llama-server` argv the router launched the model with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Whether the model failed to load/serve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<bool>,
    /// The `llama-server` process exit code, when it exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Per-file download progress, keyed by filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<HashMap<String, LlamaProgressEntry>>,
}

/// A single `{ done, total }` byte-progress entry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LlamaProgressEntry {
    /// Bytes completed.
    pub done: f64,
    /// Total bytes.
    pub total: f64,
}

/// A model's `architecture` metadata (`LlamaModelInfo["architecture"]`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LlamaArchitecture {
    /// Accepted input modalities (e.g. `["text", "image"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    /// Produced output modalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
}

/// A model's `meta` metadata (`LlamaModelInfo["meta"]`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LlamaMeta {
    /// Configured context window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ctx: Option<f64>,
    /// Context window the model was trained with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ctx_train: Option<f64>,
    /// On-disk size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// The GGUF file type (e.g. `Q4_K_M`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ftype: Option<String>,
}

/// A single entry in the router model catalog (`LlamaModelInfo`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelInfo {
    /// The model id.
    pub id: String,
    /// Alternate ids the model answers to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    /// The lifecycle status.
    pub status: LlamaModelStatusInfo,
    /// Architecture metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<LlamaArchitecture>,
    /// The model source (e.g. a Hugging Face id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Model metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<LlamaMeta>,
}

impl LlamaModelInfo {
    /// Build the synthetic `{ id, status: { value: "loaded" } }` entry pi returns
    /// when an SSE event reports the model loaded but the catalog has not yet
    /// listed it.
    fn loaded(id: &str) -> Self {
        Self {
            id: id.to_string(),
            aliases: None,
            status: LlamaModelStatusInfo {
                value: LlamaModelStatus::Loaded,
                ..Default::default()
            },
            architecture: None,
            source: None,
            meta: None,
        }
    }
}

/// The `/models` catalog response envelope (`LlamaModelsResponse`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelsResponse {
    /// The catalog entries.
    pub data: Vec<LlamaModelInfo>,
    /// The OpenAI-style `object` discriminant, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

/// A model lifecycle event from the management SSE stream (`LlamaModelEvent`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlamaModelEvent {
    /// The model the event concerns.
    pub model: String,
    /// The event name (e.g. `model_status`, `download_progress`).
    pub event: String,
    /// The event payload, shape depending on `event`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A human-facing progress update (`LlamaProgress`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlamaProgress {
    /// The status line to display.
    pub message: String,
    /// Fractional completion in `[0, 1]`, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    /// A supplemental detail line (e.g. `1.00 GiB / 2.00 GiB`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Extract the best error message from `payload.error.message`, falling back to
/// `fallback` (`errorMessage`).
fn error_message(payload: Option<&Value>, fallback: &str) -> String {
    let Some(Value::Object(map)) = payload else {
        return fallback.to_string();
    };
    let Some(Value::Object(error)) = map.get("error") else {
        return fallback.to_string();
    };
    match error.get("message") {
        Some(Value::String(message)) if !message.is_empty() => message.clone(),
        _ => fallback.to_string(),
    }
}

/// Whether `value` looks like a `LlamaModelInfo`: an object with a string `id`
/// and a string `status.value` (`isModelInfo`).
fn is_model_info(value: &Value) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    let id_is_string = matches!(map.get("id"), Some(Value::String(_)));
    let status_value_is_string = matches!(
        map.get("status").and_then(|status| status.get("value")),
        Some(Value::String(_))
    );
    id_is_string && status_value_is_string
}

/// Parse a model-load progress payload into a [`LlamaProgress`]
/// (`parseLoadProgress`).
fn parse_load_progress(data: Option<&Value>) -> Option<LlamaProgress> {
    let progress = data?.as_object()?.get("progress")?.as_object()?;
    let stage = match progress.get("current") {
        Some(Value::String(current)) => Some(current.clone()),
        _ => match progress.get("stage") {
            Some(Value::String(stage)) => Some(stage.clone()),
            _ => None,
        },
    };
    let stages: Vec<String> = match progress.get("stages") {
        Some(Value::Array(entries)) => entries
            .iter()
            .filter_map(|entry| entry.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let stage_ratio = progress
        .get("value")
        .and_then(Value::as_f64)
        .map(|value| value.clamp(0.0, 1.0));
    let mut ratio = stage_ratio;
    if let Some(stage) = &stage {
        if !stages.is_empty() {
            if let Some(index) = stages.iter().position(|candidate| candidate == stage) {
                ratio = Some((index as f64 + stage_ratio.unwrap_or(0.0)) / stages.len() as f64);
            }
        }
    }
    Some(LlamaProgress {
        message: match &stage {
            Some(stage) => format!("Loading {}", stage.replace('_', " ")),
            None => "Loading model".to_string(),
        },
        ratio,
        detail: None,
    })
}

/// Parse a download progress payload (a map of `{ done, total }` entries) into a
/// [`LlamaProgress`] (`parseDownloadProgress`).
fn parse_download_progress(data: Option<&Value>) -> Option<LlamaProgress> {
    let entries = data?.as_object()?;
    let mut done = 0.0_f64;
    let mut total = 0.0_f64;
    for value in entries.values() {
        let Some(entry) = value.as_object() else {
            continue;
        };
        let (Some(entry_done), Some(entry_total)) = (
            entry.get("done").and_then(Value::as_f64),
            entry.get("total").and_then(Value::as_f64),
        ) else {
            continue;
        };
        done += entry_done;
        total += entry_total;
    }
    if total <= 0.0 {
        return None;
    }
    Some(LlamaProgress {
        message: "Downloading model".to_string(),
        ratio: Some(done / total),
        detail: Some(format!("{} / {}", format_bytes(done), format_bytes(total))),
    })
}

/// Format a byte count with binary units (`formatBytes`).
pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 {
        return format!("{bytes} B");
    }
    let units = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes / 1024.0;
    let mut unit = units[0];
    let mut index = 1;
    while index < units.len() && value >= 1024.0 {
        value /= 1024.0;
        unit = units[index];
        index += 1;
    }
    if value >= 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

/// Normalize a management server URL, stripping hash/search/trailing-slash and a
/// trailing `/v1`, and rejecting non-`http(s)` schemes
/// (`normalizeLlamaServerUrl`).
pub fn normalize_llama_server_url(value: &str) -> Result<String> {
    let value = value.trim();
    let colon = value
        .find(':')
        .ok_or_else(|| anyhow!("Server URL must use http or https"))?;
    let scheme = value[..colon].to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        bail!("Server URL must use http or https");
    }
    let after_scheme = value[colon + 1..]
        .strip_prefix("//")
        .ok_or_else(|| anyhow!("Server URL must use http or https"))?;
    // Split authority from the remainder at the first `/`, `?` or `#`.
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    let remainder = &after_scheme[authority_end..];
    // The pathname runs up to the search (`?`) or hash (`#`), both of which pi
    // clears. An absent path normalizes to `/`, as `new URL` does.
    let path_end = remainder.find(['?', '#']).unwrap_or(remainder.len());
    let pathname = &remainder[..path_end];
    let pathname = if pathname.is_empty() { "/" } else { pathname };
    // pathname.replace(/\/+$/, "").replace(/\/v1$/, "") || "/"
    let trimmed = pathname.trim_end_matches('/');
    let without_v1 = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let pathname = if without_v1.is_empty() {
        "/"
    } else {
        without_v1
    };
    // url.toString() then strip a single trailing slash.
    let rebuilt = format!("{scheme}://{authority}{pathname}");
    Ok(rebuilt.strip_suffix('/').unwrap_or(&rebuilt).to_string())
}

/// The inference base URL for a management server (`llamaInferenceUrl`): the
/// normalized server URL with `/v1` appended.
pub fn llama_inference_url(server_url: &str) -> Result<String> {
    Ok(format!("{}/v1", normalize_llama_server_url(server_url)?))
}

/// A cooperative source of parsed `llama.cpp` management SSE events.
///
/// pi's `watch` / `loadAndWait` / `downloadAndWait` read the live
/// `text/event-stream` from `/models/sse` via `response.body.getReader()`,
/// decoding `\n\n`-delimited `data:` frames into [`LlamaModelEvent`]s. The
/// buffered [`HttpTransport`] seam cannot stream (see
/// `crates/pidgin-ai/src/seams/http_reqwest.rs`, "Buffered, not streaming"), so
/// this local seam stands in for the streaming reader: [`next_event`] yields the
/// next available event, or `Ok(None)` when none is currently available, letting
/// the caller interleave authoritative catalog polling between drains.
///
/// TODO(port): streaming SSE transport deferred — a concrete network-backed
/// implementation needs an incremental HTTP body reader the [`HttpTransport`]
/// seam does not expose yet; mirror the feature-gated precedent in
/// `crates/pidgin-ai/src/seams/http_reqwest.rs`. Frame decoding is already
/// ported in [`parse_sse_frame`], so a concrete impl only supplies the byte
/// stream. Tests drive a scripted in-memory stream.
///
/// [`next_event`]: LlamaEventStream::next_event
pub trait LlamaEventStream {
    /// Return the next parsed event, or `Ok(None)` when no event is currently
    /// available.
    fn next_event(&mut self) -> std::io::Result<Option<LlamaModelEvent>>;
}

/// Decode one SSE frame (the text between `\n\n` boundaries) into a
/// [`LlamaModelEvent`], returning `None` for a frame with no `data:` lines or a
/// malformed payload — pi keeps catalog polling authoritative and ignores such
/// frames (`watch`'s frame loop).
pub fn parse_sse_frame(frame: &str) -> Option<LlamaModelEvent> {
    let data = frame
        .split('\n')
        .filter(|line| line.starts_with("data:"))
        .map(|line| line[5..].trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return None;
    }
    // Deserialization enforces pi's `typeof event.model === "string" &&
    // typeof event.event === "string"` guard: both fields are required strings.
    serde_json::from_str::<LlamaModelEvent>(&data).ok()
}

/// A management client for a `llama.cpp` router server backed by the injected
/// [`HttpTransport`] seam (`LlamaClient`).
pub struct LlamaClient {
    transport: Arc<dyn HttpTransport>,
    /// The normalized management server URL (pi's readonly `serverUrl`).
    pub server_url: String,
    api_key: Option<String>,
    unload_poll_interval: Duration,
    load_poll_interval: Duration,
    download_poll_interval: Duration,
}

impl LlamaClient {
    /// Build a client that issues requests through `transport` against
    /// `server_url` (normalized via [`normalize_llama_server_url`]), optionally
    /// authenticated with `api_key`.
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        server_url: &str,
        api_key: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            transport,
            server_url: normalize_llama_server_url(server_url)?,
            api_key,
            unload_poll_interval: Duration::from_millis(100),
            load_poll_interval: Duration::from_millis(250),
            download_poll_interval: Duration::from_millis(500),
        })
    }

    /// Override the `unload_and_wait` / `load_and_wait` / `download_and_wait`
    /// polling intervals. Defaults match pi (100 ms / 250 ms / 500 ms); tests set
    /// them to zero so the polling loops run without real waits.
    pub fn with_poll_intervals(
        mut self,
        unload: Duration,
        load: Duration,
        download: Duration,
    ) -> Self {
        self.unload_poll_interval = unload;
        self.load_poll_interval = load;
        self.download_poll_interval = download;
        self
    }

    /// Issue `method path` (with an optional JSON `body`) and return the decoded
    /// payload, mapping HTTP errors to pi's messages (`request`).
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
        signal: Option<&AbortSignal>,
    ) -> Result<Option<Value>> {
        if signal.is_some_and(AbortSignal::is_aborted) {
            bail!("The operation was aborted");
        }
        let has_body = body.is_some();
        let mut request = HttpRequest {
            method: method.to_string(),
            url: format!("{}{}", self.server_url, path),
            headers: BTreeMap::new(),
            body,
        };
        if has_body {
            request
                .headers
                .insert("content-type".to_string(), "application/json".to_string());
        }
        if let Some(api_key) = &self.api_key {
            request
                .headers
                .insert("authorization".to_string(), format!("Bearer {api_key}"));
        }
        let response = self
            .transport
            .send(&request)
            .map_err(|error| anyhow!(error.to_string()))?;
        let payload: Option<Value> = serde_json::from_str(&response.body).ok();
        if !response.is_ok() {
            let fallback = format!("llama.cpp returned HTTP {}", response.status);
            bail!(error_message(payload.as_ref(), &fallback));
        }
        Ok(payload)
    }

    /// Fetch the model catalog, validating router-mode shape (`list`).
    pub fn list(&self, options: LlamaListOptions) -> Result<Vec<LlamaModelInfo>> {
        let path = if options.reload {
            "/models?reload=1"
        } else {
            "/models"
        };
        let payload = self
            .request("GET", path, None, options.signal)?
            .unwrap_or(Value::Null);
        let Value::Object(map) = &payload else {
            bail!("llama.cpp returned an invalid model catalog");
        };
        let Some(Value::Array(data)) = map.get("data") else {
            bail!("llama.cpp returned an invalid model catalog");
        };
        if !data.iter().all(is_model_info) {
            bail!("Server is not running in llama.cpp router mode");
        }
        let mut models = Vec::with_capacity(data.len());
        for value in data {
            let model: LlamaModelInfo = serde_json::from_value(value.clone())
                .map_err(|error| anyhow!(error.to_string()))?;
            models.push(model);
        }
        Ok(models)
    }

    /// Request that the router load `model` (`load`).
    pub fn load(&self, model: &str, signal: Option<&AbortSignal>) -> Result<()> {
        self.request(
            "POST",
            "/models/load",
            Some(json!({ "model": model }).to_string()),
            signal,
        )?;
        Ok(())
    }

    /// Request that the router unload `model` (`unload`).
    pub fn unload(&self, model: &str, signal: Option<&AbortSignal>) -> Result<()> {
        self.request(
            "POST",
            "/models/unload",
            Some(json!({ "model": model }).to_string()),
            signal,
        )?;
        Ok(())
    }

    /// Unload `model` and poll the catalog until it reports unloaded or absent
    /// (`unloadAndWait`).
    pub fn unload_and_wait(&self, model: &str, signal: Option<&AbortSignal>) -> Result<()> {
        self.unload(model, signal)?;
        loop {
            let models = self.list(LlamaListOptions {
                reload: false,
                signal,
            })?;
            match models.iter().find(|candidate| candidate.id == model) {
                None => return Ok(()),
                Some(entry) if entry.status.value == LlamaModelStatus::Unloaded => return Ok(()),
                _ => {}
            }
            sleep(self.unload_poll_interval, signal)?;
        }
    }

    /// Request that the router download `model` (`download`).
    pub fn download(&self, model: &str, signal: Option<&AbortSignal>) -> Result<()> {
        self.request(
            "POST",
            "/models",
            Some(json!({ "model": model }).to_string()),
            signal,
        )?;
        Ok(())
    }

    /// Forward each event from `stream` to `on_event` until the stream ends or
    /// `signal` aborts (`watch`).
    ///
    /// TODO(port): streaming SSE transport deferred — driven by the
    /// [`LlamaEventStream`] seam rather than a live `/models/sse` reader; see the
    /// seam's docs.
    pub fn watch(
        &self,
        stream: &mut dyn LlamaEventStream,
        mut on_event: impl FnMut(LlamaModelEvent),
        signal: Option<&AbortSignal>,
    ) -> Result<()> {
        loop {
            if signal.is_some_and(AbortSignal::is_aborted) {
                break;
            }
            match stream
                .next_event()
                .map_err(|error| anyhow!(error.to_string()))?
            {
                Some(event) => on_event(event),
                None => break,
            }
        }
        Ok(())
    }

    /// Load `model`, draining SSE events from `stream` while polling the catalog,
    /// and resolve with the loaded catalog entry (`loadAndWait`).
    ///
    /// TODO(port): streaming SSE transport deferred — SSE events arrive via the
    /// [`LlamaEventStream`] seam; the authoritative catalog polling flows through
    /// [`HttpTransport`].
    pub fn load_and_wait(
        &self,
        stream: &mut dyn LlamaEventStream,
        model: &str,
        mut on_progress: impl FnMut(LlamaProgress),
        signal: Option<&AbortSignal>,
    ) -> Result<LlamaModelInfo> {
        let mut event_loaded = false;
        let mut event_error: Option<String> = None;
        self.load(model, signal)?;
        on_progress(LlamaProgress {
            message: "Loading model".to_string(),
            ratio: None,
            detail: None,
        });
        loop {
            if signal.is_some_and(AbortSignal::is_aborted) {
                bail!("Cancelled");
            }
            while let Some(event) = stream
                .next_event()
                .map_err(|error| anyhow!(error.to_string()))?
            {
                if event.model != model {
                    continue;
                }
                if event.event != "model_status" && event.event != "status_change" {
                    continue;
                }
                let status = event
                    .data
                    .as_ref()
                    .and_then(|data| data.get("status"))
                    .and_then(Value::as_str);
                if status == Some("loaded") {
                    event_loaded = true;
                }
                if status == Some("unloaded") {
                    event_error = Some("Model failed to load".to_string());
                }
                if let Some(progress) = parse_load_progress(event.data.as_ref()) {
                    on_progress(progress);
                }
            }
            let models = self.list(LlamaListOptions {
                reload: false,
                signal,
            })?;
            let entry = models.into_iter().find(|candidate| candidate.id == model);
            if let Some(entry) = &entry {
                if entry.status.value == LlamaModelStatus::Loaded {
                    return Ok(entry.clone());
                }
            }
            if event_loaded && entry.is_none() {
                return Ok(LlamaModelInfo::loaded(model));
            }
            let failed = entry
                .as_ref()
                .is_some_and(|entry| entry.status.failed == Some(true));
            if failed || event_error.is_some() {
                let exit_code = entry.as_ref().and_then(|entry| entry.status.exit_code);
                match exit_code {
                    None => bail!(event_error
                        .clone()
                        .unwrap_or_else(|| "Model failed to load".to_string())),
                    Some(exit_code) => bail!("Model exited with code {exit_code}"),
                }
            }
            sleep(self.load_poll_interval, signal)?;
        }
    }

    /// Download `model`, draining SSE events from `stream` while polling the
    /// catalog, and resolve with the reloaded catalog (`downloadAndWait`).
    ///
    /// TODO(port): streaming SSE transport deferred — SSE events arrive via the
    /// [`LlamaEventStream`] seam; the authoritative catalog polling flows through
    /// [`HttpTransport`].
    pub fn download_and_wait(
        &self,
        stream: &mut dyn LlamaEventStream,
        model: &str,
        mut on_progress: impl FnMut(LlamaProgress),
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<LlamaModelInfo>> {
        let mut finished = false;
        let mut failure: Option<String> = None;
        let mut saw_downloading = false;
        let mut polls = 0_u32;
        self.download(model, signal)?;
        on_progress(LlamaProgress {
            message: "Downloading model".to_string(),
            ratio: None,
            detail: None,
        });
        loop {
            if signal.is_some_and(AbortSignal::is_aborted) {
                bail!("Cancelled");
            }
            while let Some(event) = stream
                .next_event()
                .map_err(|error| anyhow!(error.to_string()))?
            {
                if event.model != model {
                    continue;
                }
                match event.event.as_str() {
                    "download_finished" => finished = true,
                    "download_failed" => {
                        failure = Some(error_message(event.data.as_ref(), "Download failed"));
                    }
                    "download_progress" => {
                        saw_downloading = true;
                        if let Some(progress) = parse_download_progress(event.data.as_ref()) {
                            on_progress(progress);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(failure) = &failure {
                bail!(failure.clone());
            }
            let models = self.list(LlamaListOptions {
                reload: false,
                signal,
            })?;
            polls += 1;
            let entry = models.into_iter().find(|candidate| candidate.id == model);
            let downloading = entry
                .as_ref()
                .is_some_and(|entry| entry.status.value == LlamaModelStatus::Downloading);
            if downloading {
                saw_downloading = true;
                let progress_value = entry
                    .as_ref()
                    .and_then(|entry| entry.status.progress.as_ref())
                    .map(|progress| serde_json::to_value(progress).unwrap_or(Value::Null));
                if let Some(progress) = parse_download_progress(progress_value.as_ref()) {
                    on_progress(progress);
                }
            } else if finished || (entry.is_some() && (saw_downloading || polls >= 2)) {
                return self.list(LlamaListOptions {
                    reload: true,
                    signal,
                });
            }
            sleep(self.download_poll_interval, signal)?;
        }
    }
}

/// Options for [`LlamaClient::list`] (pi's `{ reload?, signal? }`).
#[derive(Default)]
pub struct LlamaListOptions<'a> {
    /// Whether to force a catalog reload (`?reload=1`).
    pub reload: bool,
    /// An optional abort signal.
    pub signal: Option<&'a AbortSignal>,
}

/// Pause for `duration`, honoring `signal` before and after the wait (`sleep`).
///
/// The synchronous, buffered seam cannot interrupt an in-flight wait the way
/// pi's promise-based `sleep` aborts mid-timeout, so this checks the signal on
/// both sides of the pause; polling loops keep the interval short.
fn sleep(duration: Duration, signal: Option<&AbortSignal>) -> Result<()> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        bail!("Cancelled");
    }
    if !duration.is_zero() {
        std::thread::sleep(duration);
    }
    if signal.is_some_and(AbortSignal::is_aborted) {
        bail!("Cancelled");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_ai::seams::http::{HttpResponse, ScriptedTransport};
    use std::collections::VecDeque;

    /// A scripted, in-memory [`LlamaEventStream`] that replays queued events then
    /// reports the stream drained — the Rust-side stand-in for pi's live SSE
    /// reader.
    struct ScriptedEventStream {
        events: VecDeque<LlamaModelEvent>,
    }

    impl ScriptedEventStream {
        fn new(events: Vec<LlamaModelEvent>) -> Self {
            Self {
                events: events.into(),
            }
        }
    }

    impl LlamaEventStream for ScriptedEventStream {
        fn next_event(&mut self) -> std::io::Result<Option<LlamaModelEvent>> {
            Ok(self.events.pop_front())
        }
    }

    fn event(model: &str, name: &str, data: Value) -> LlamaModelEvent {
        LlamaModelEvent {
            model: model.to_string(),
            event: name.to_string(),
            data: Some(data),
        }
    }

    fn client(transport: &ScriptedTransport, api_key: Option<&str>) -> LlamaClient {
        let transport: Arc<dyn HttpTransport> = Arc::new(transport.clone());
        LlamaClient::new(
            transport,
            "http://localhost:8080",
            api_key.map(str::to_string),
        )
        .unwrap()
        .with_poll_intervals(Duration::ZERO, Duration::ZERO, Duration::ZERO)
    }

    /// Mirrors the pi test "normalizes management and inference URLs".
    #[test]
    fn normalizes_management_and_inference_urls() {
        assert_eq!(
            normalize_llama_server_url("http://127.0.0.1:8080/v1/").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_llama_server_url("https://example.com/prefix/v1").unwrap(),
            "https://example.com/prefix"
        );
        let error = normalize_llama_server_url("file:///tmp/llama").unwrap_err();
        assert!(error.to_string().contains("http or https"));

        // llamaInferenceUrl appends /v1 to the normalized server URL.
        assert_eq!(
            llama_inference_url("http://127.0.0.1:8080/v1/").unwrap(),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(
            llama_inference_url("https://example.com/prefix").unwrap(),
            "https://example.com/prefix/v1"
        );
    }

    /// `format_bytes` renders binary units exactly as pi does.
    #[test]
    fn format_bytes_uses_binary_units() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1024.0), "1.00 KiB");
        assert_eq!(format_bytes(1536.0), "1.50 KiB");
        assert_eq!(format_bytes(10.0 * 1024.0), "10.0 KiB");
        assert_eq!(format_bytes(1024.0 * 1024.0), "1.00 MiB");
        assert_eq!(format_bytes(5.0 * 1024.0 * 1024.0 * 1024.0), "5.00 GiB");
    }

    /// `parse_load_progress` blends the stage index and stage ratio.
    #[test]
    fn parse_load_progress_blends_stage_and_ratio() {
        let data = json!({
            "progress": {
                "current": "loading_weights",
                "stages": ["loading_weights", "warmup"],
                "value": 0.5,
            }
        });
        let progress = parse_load_progress(Some(&data)).unwrap();
        assert_eq!(progress.message, "Loading loading weights");
        assert!((progress.ratio.unwrap() - 0.25).abs() < 1e-9);

        // No progress object -> None; a bare payload -> "Loading model".
        assert_eq!(parse_load_progress(Some(&json!({}))), None);
        assert_eq!(parse_load_progress(None), None);
        let bare = json!({ "progress": { "value": 0.4 } });
        let progress = parse_load_progress(Some(&bare)).unwrap();
        assert_eq!(progress.message, "Loading model");
        assert!((progress.ratio.unwrap() - 0.4).abs() < 1e-9);
    }

    /// `parse_download_progress` sums entries and formats the detail line.
    #[test]
    fn parse_download_progress_sums_and_formats() {
        let data = json!({
            "a": { "done": 512, "total": 1024 },
            "b": { "done": 512, "total": 1024 },
        });
        let progress = parse_download_progress(Some(&data)).unwrap();
        assert_eq!(progress.message, "Downloading model");
        assert!((progress.ratio.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(progress.detail.as_deref(), Some("1.00 KiB / 2.00 KiB"));

        // A zero/empty total yields no progress.
        assert_eq!(parse_download_progress(Some(&json!({}))), None);
    }

    /// `list`/`load`/`unload` issue the expected requests through the transport.
    #[test]
    fn lists_loads_and_unloads_via_transport() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(r#"{"data":[{"id":"m1","status":{"value":"loaded"}}]}"#);
        scripted.push_ok("{}");
        scripted.push_ok("{}");
        scripted.push_ok(r#"{"data":[]}"#);
        let llama = client(&scripted, Some("secret"));

        let models = llama
            .list(LlamaListOptions {
                reload: false,
                signal: None,
            })
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "m1");
        assert_eq!(models[0].status.value, LlamaModelStatus::Loaded);

        llama.load("m1", None).unwrap();
        llama.unload("m1", None).unwrap();
        llama
            .list(LlamaListOptions {
                reload: true,
                signal: None,
            })
            .unwrap();

        let requests = scripted.requests();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].url, "http://localhost:8080/models");
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer secret")
        );
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].url, "http://localhost:8080/models/load");
        assert_eq!(
            requests[1].headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(requests[1].body.as_deref(), Some(r#"{"model":"m1"}"#));
        assert_eq!(requests[2].url, "http://localhost:8080/models/unload");
        assert_eq!(requests[3].url, "http://localhost:8080/models?reload=1");
    }

    /// A non-router or malformed catalog surfaces pi's messages.
    #[test]
    fn list_rejects_invalid_and_non_router_catalogs() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(r#"{"nope":true}"#);
        let llama = client(&scripted, None);
        let error = llama
            .list(LlamaListOptions::default())
            .unwrap_err()
            .to_string();
        assert_eq!(error, "llama.cpp returned an invalid model catalog");

        let scripted = ScriptedTransport::new();
        scripted.push_ok(r#"{"data":[{"id":"m1"}]}"#);
        let llama = client(&scripted, None);
        let error = llama
            .list(LlamaListOptions::default())
            .unwrap_err()
            .to_string();
        assert_eq!(error, "Server is not running in llama.cpp router mode");
    }

    /// An HTTP error surfaces the payload's `error.message`.
    #[test]
    fn request_error_surfaces_payload_error_message() {
        let scripted = ScriptedTransport::new();
        scripted.push_response(Ok(HttpResponse {
            status: 500,
            headers: BTreeMap::new(),
            body: r#"{"error":{"message":"router offline"}}"#.to_string(),
        }));
        let llama = client(&scripted, None);
        let error = llama.load("m1", None).unwrap_err().to_string();
        assert_eq!(error, "router offline");
    }

    /// `unload_and_wait` polls until the catalog reports the model unloaded.
    #[test]
    fn unload_and_wait_polls_until_unloaded() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok("{}"); // unload
        scripted.push_ok(r#"{"data":[{"id":"m1","status":{"value":"loading"}}]}"#);
        scripted.push_ok(r#"{"data":[{"id":"m1","status":{"value":"unloaded"}}]}"#);
        let llama = client(&scripted, None);
        llama.unload_and_wait("m1", None).unwrap();
        // unload + two catalog polls.
        assert_eq!(scripted.requests().len(), 3);
    }

    /// `watch` forwards every event from the stream to the callback.
    #[test]
    fn watch_forwards_events() {
        let scripted = ScriptedTransport::new();
        let llama = client(&scripted, None);
        let mut stream = ScriptedEventStream::new(vec![
            event("m1", "model_status", json!({ "status": "loading" })),
            event("m1", "model_status", json!({ "status": "loaded" })),
        ]);
        let mut seen = Vec::new();
        llama
            .watch(&mut stream, |event| seen.push(event.event), None)
            .unwrap();
        assert_eq!(seen, vec!["model_status", "model_status"]);
    }

    /// `load_and_wait` resolves once the catalog reports the model loaded.
    #[test]
    fn load_and_wait_resolves_on_loaded_catalog() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok("{}"); // load
        scripted.push_ok(r#"{"data":[{"id":"m1","status":{"value":"loaded"}}]}"#);
        let llama = client(&scripted, None);
        let mut stream = ScriptedEventStream::new(vec![]);

        let mut progresses = Vec::new();
        let result = {
            let on_progress = |progress: LlamaProgress| progresses.push(progress);
            llama
                .load_and_wait(&mut stream, "m1", on_progress, None)
                .unwrap()
        };
        assert_eq!(result.id, "m1");
        assert_eq!(result.status.value, LlamaModelStatus::Loaded);
        assert_eq!(progresses[0].message, "Loading model");
    }

    /// `load_and_wait` resolves on an SSE `loaded` event when the catalog is
    /// still empty, returning the synthetic entry.
    #[test]
    fn load_and_wait_resolves_on_event_when_absent() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok("{}"); // load
        scripted.push_ok(r#"{"data":[]}"#);
        let llama = client(&scripted, None);
        let mut stream = ScriptedEventStream::new(vec![event(
            "m1",
            "model_status",
            json!({ "status": "loaded" }),
        )]);

        let mut progresses = Vec::new();
        let result = {
            let on_progress = |progress: LlamaProgress| progresses.push(progress);
            llama
                .load_and_wait(&mut stream, "m1", on_progress, None)
                .unwrap()
        };
        assert_eq!(result.id, "m1");
        assert_eq!(result.status.value, LlamaModelStatus::Loaded);
    }

    /// `load_and_wait` reports a failing model's exit code.
    #[test]
    fn load_and_wait_fails_on_exit_code() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok("{}"); // load
        scripted.push_ok(
            r#"{"data":[{"id":"m1","status":{"value":"loading","failed":true,"exit_code":1}}]}"#,
        );
        let llama = client(&scripted, None);
        let mut stream = ScriptedEventStream::new(vec![]);

        let error = {
            let on_progress = |_progress: LlamaProgress| {};
            llama
                .load_and_wait(&mut stream, "m1", on_progress, None)
                .unwrap_err()
        };
        assert_eq!(error.to_string(), "Model exited with code 1");
    }

    /// `download_and_wait` reports progress from an SSE event and resolves with
    /// the reloaded catalog once the download finishes.
    #[test]
    fn download_and_wait_reports_progress_and_resolves() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok("{}"); // download
        scripted.push_ok(r#"{"data":[{"id":"m1","status":{"value":"loaded"}}]}"#); // poll
        scripted.push_ok(r#"{"data":[{"id":"m1","status":{"value":"loaded"}}]}"#); // reload result
        let llama = client(&scripted, None);
        let mut stream = ScriptedEventStream::new(vec![
            event(
                "m1",
                "download_progress",
                json!({ "a": { "done": 512, "total": 1024 } }),
            ),
            event("m1", "download_finished", json!({})),
        ]);

        let mut progresses = Vec::new();
        let models = {
            let on_progress = |progress: LlamaProgress| progresses.push(progress);
            llama
                .download_and_wait(&mut stream, "m1", on_progress, None)
                .unwrap()
        };
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "m1");
        assert_eq!(progresses[0].message, "Downloading model");
        let downloading = progresses
            .iter()
            .find(|progress| progress.detail.is_some())
            .expect("a download progress update with a detail line");
        assert_eq!(downloading.detail.as_deref(), Some("512 B / 1.00 KiB"));
        assert!((downloading.ratio.unwrap() - 0.5).abs() < 1e-9);
    }

    /// `download_and_wait` surfaces an SSE `download_failed` event.
    #[test]
    fn download_and_wait_fails_on_event() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok("{}"); // download
        let llama = client(&scripted, None);
        let mut stream = ScriptedEventStream::new(vec![event(
            "m1",
            "download_failed",
            json!({ "error": { "message": "disk full" } }),
        )]);

        let error = {
            let on_progress = |_progress: LlamaProgress| {};
            llama
                .download_and_wait(&mut stream, "m1", on_progress, None)
                .unwrap_err()
        };
        assert_eq!(error.to_string(), "disk full");
    }

    /// `parse_sse_frame` decodes `data:` lines and ignores malformed frames.
    #[test]
    fn parse_sse_frame_decodes_data_lines() {
        let frame = "event: model_status\ndata: {\"model\":\"m1\",\"event\":\"model_status\"}";
        let parsed = parse_sse_frame(frame).unwrap();
        assert_eq!(parsed.model, "m1");
        assert_eq!(parsed.event, "model_status");

        assert_eq!(parse_sse_frame("event: ping"), None);
        assert_eq!(parse_sse_frame("data: not json"), None);
    }
}
