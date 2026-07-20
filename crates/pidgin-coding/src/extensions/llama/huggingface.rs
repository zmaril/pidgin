//! Hugging Face Hub client for the `llama.cpp` extension — a faithful port of
//! pi-coding-agent's `extensions/llama/huggingface.ts`.
//!
//! Mirrors pi symbol-for-symbol: the [`HuggingFaceModel`],
//! [`HuggingFaceQuantization`] and [`HuggingFaceModelDetails`] shapes, the
//! [`find_hugging_face_token`] token-file lookup order, and the
//! [`HuggingFaceClient`] search / details requests including the GGUF sibling
//! parsing, shard-size aggregation and `Q4_K_M`-first quantization ordering.
//!
//! Where pi calls `fetch`, this port issues the request through the injected
//! [`HttpTransport`] seam (`crates/pidgin-ai/src/seams/http.rs`) so tests can
//! script canned responses exactly as pi's `vi.stubGlobal("fetch")` does.
// straitjacket-allow-file:duplication

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, bail, Result};
use pidgin_ai::seams::http::{HttpRequest, HttpTransport};
use pidgin_ai::seams::provider::AbortSignal;
use regex::Regex;
use serde::{Serialize, Serializer};
use serde_json::Value;

/// The default Hugging Face Hub base URL (`DEFAULT_HUGGING_FACE_URL`).
pub const DEFAULT_HUGGING_FACE_URL: &str = "https://huggingface.co";

/// `Number.MAX_SAFE_INTEGER`, the sentinel pi sorts unknown-size quantizations
/// behind.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `QUANTIZATION_PATTERN` — extracts a quantization label (e.g. `Q4_K_M`,
/// `IQ4_XS`, `BF16`) from a GGUF filename stem. Ported verbatim from pi with the
/// `iu` flags mapped to Rust's default-Unicode matcher plus `(?i)`.
fn quantization_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:^|[-_.])((?:UD-)?(?:IQ\d(?:_[A-Z0-9]+)+|Q\d(?:_[A-Z0-9]+)+|BF16|F16|F32|MXFP\d(?:_[A-Z0-9]+)*))$",
        )
        .expect("valid quantization regex")
    })
}

/// `SHARD_SUFFIX_PATTERN` — matches a `-00001-of-00002` shard suffix at the end
/// of a GGUF filename stem.
fn shard_suffix_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"-\d{5}-of-\d{5}$").expect("valid shard suffix regex"))
}

/// A Hugging Face model search result (`HuggingFaceModel`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HuggingFaceModel {
    /// The model id (`owner/name`).
    pub id: String,
    /// The download count, `0` when the payload omits a numeric `downloads`.
    pub downloads: u64,
}

/// A single GGUF quantization available for a model (`HuggingFaceQuantization`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HuggingFaceQuantization {
    /// The uppercased quantization label (e.g. `Q4_K_M`).
    pub name: String,
    /// Aggregate byte size across the quantization's shards, absent when any
    /// shard omitted its size (`size?: number`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

/// Whether a model gates its downloads (`gated: false | "auto" | "manual"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HuggingFaceGated {
    /// Not gated (`false`).
    No,
    /// Automatically granted on request (`"auto"`).
    Auto,
    /// Manually granted by the model owner (`"manual"`).
    Manual,
}

impl Serialize for HuggingFaceGated {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            HuggingFaceGated::No => serializer.serialize_bool(false),
            HuggingFaceGated::Auto => serializer.serialize_str("auto"),
            HuggingFaceGated::Manual => serializer.serialize_str("manual"),
        }
    }
}

/// Detailed metadata for a model (`HuggingFaceModelDetails`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HuggingFaceModelDetails {
    /// The model id.
    pub id: String,
    /// The model's gating requirement.
    pub gated: HuggingFaceGated,
    /// Available quantizations, `Q4_K_M` first then ascending by size.
    pub quantizations: Vec<HuggingFaceQuantization>,
}

/// Extract the best error message from a JSON payload, falling back to `fallback`
/// (`payloadError`).
fn payload_error(payload: Option<&Value>, fallback: &str) -> String {
    match payload {
        Some(Value::Object(map)) => match map.get("error") {
            Some(Value::String(error)) if !error.is_empty() => error.clone(),
            _ => fallback.to_string(),
        },
        _ => fallback.to_string(),
    }
}

/// Parse a rate-limit delay (seconds) from a `RateLimit` header value
/// (`parseRateLimitDelay`).
fn parse_rate_limit_delay(value: Option<&str>) -> Option<f64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?:^|;)t=(\d+)").expect("valid rate-limit regex"));
    let value = value?;
    let captures = re.captures(value)?;
    captures.get(1)?.as_str().parse::<f64>().ok()
}

/// Read a token from `path`, trimming whitespace and treating empty as absent
/// (`readToken`).
fn read_token(path: &str) -> Option<String> {
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// Locate a Hugging Face access token (`findHuggingFaceToken`).
///
/// Checks `HF_TOKEN` first, then the token files in pi's order: `HF_TOKEN_PATH`,
/// `HF_HOME/token`, `XDG_CACHE_HOME/huggingface/token`, and finally
/// `~/.cache/huggingface/token`. The home directory is read from `$HOME`
/// (`os.homedir()`), independent of the supplied `env` map, matching pi.
pub fn find_hugging_face_token(env: &HashMap<String, String>) -> Option<String> {
    let from_environment = env.get("HF_TOKEN").map(|value| value.trim());
    if let Some(from_environment) = from_environment {
        if !from_environment.is_empty() {
            return Some(from_environment.to_string());
        }
    }

    let mut paths: Vec<String> = Vec::new();
    if let Some(path) = env.get("HF_TOKEN_PATH") {
        paths.push(path.clone());
    }
    if let Some(hf_home) = env.get("HF_HOME") {
        paths.push(join_path(&[hf_home, "token"]));
    }
    if let Some(xdg_cache) = env.get("XDG_CACHE_HOME") {
        paths.push(join_path(&[xdg_cache, "huggingface", "token"]));
    }
    if let Ok(home) = std::env::var("HOME") {
        paths.push(join_path(&[&home, ".cache", "huggingface", "token"]));
    }

    let mut seen: Vec<&str> = Vec::new();
    for path in &paths {
        if seen.contains(&path.as_str()) {
            continue;
        }
        seen.push(path.as_str());
        if let Some(token) = read_token(path) {
            return Some(token);
        }
    }
    None
}

/// Join path segments with the platform separator (`path.join`).
fn join_path(segments: &[&str]) -> String {
    let mut path = std::path::PathBuf::new();
    for segment in segments {
        path.push(segment);
    }
    path.to_string_lossy().into_owned()
}

/// A Hugging Face Hub client backed by the injected [`HttpTransport`] seam
/// (`HuggingFaceClient`).
pub struct HuggingFaceClient {
    transport: Arc<dyn HttpTransport>,
    token: Option<String>,
    base_url: String,
}

impl HuggingFaceClient {
    /// Build a client that issues requests through `transport`, optionally
    /// authenticated with `token`. `base_url` defaults to
    /// [`DEFAULT_HUGGING_FACE_URL`] and has any trailing slashes trimmed.
    pub fn new(
        transport: Arc<dyn HttpTransport>,
        token: Option<String>,
        base_url: Option<String>,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_HUGGING_FACE_URL.to_string());
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            transport,
            token,
            base_url,
        }
    }

    /// Issue a GET to `path` and return the decoded JSON payload, mapping HTTP
    /// errors to pi's messages (`request`).
    fn request(&self, path: &str, signal: Option<&AbortSignal>) -> Result<Value> {
        if signal.is_some_and(AbortSignal::is_aborted) {
            bail!("The operation was aborted");
        }
        let mut request = HttpRequest::get(format!("{}{}", self.base_url, path));
        if let Some(token) = &self.token {
            request = request.with_header("authorization", format!("Bearer {token}"));
        }
        let response = self
            .transport
            .send(&request)
            .map_err(|error| anyhow!(error.to_string()))?;
        let payload: Option<Value> = serde_json::from_str(&response.body).ok();
        if !response.is_ok() {
            let fallback = format!("Hugging Face returned HTTP {}", response.status);
            if response.status == 429 {
                let delay = response
                    .headers
                    .get("retry-after")
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| *value != 0.0)
                    .or_else(|| {
                        parse_rate_limit_delay(
                            response.headers.get("ratelimit").map(String::as_str),
                        )
                    });
                match delay {
                    Some(delay) => bail!("Hugging Face rate limit reached; retry in {delay}s"),
                    None => bail!("Hugging Face rate limit reached"),
                }
            }
            bail!(payload_error(payload.as_ref(), &fallback));
        }
        Ok(payload.unwrap_or(Value::Null))
    }

    /// Search the Hub for GGUF models matching `query` (`search`).
    pub fn search(
        &self,
        query: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<Vec<HuggingFaceModel>> {
        let params = encode_query(&[
            ("search", query),
            ("filter", "gguf"),
            ("sort", "downloads"),
            ("direction", "-1"),
            ("limit", "20"),
        ]);
        let payload = self.request(&format!("/api/models?{params}"), signal)?;
        let Value::Array(values) = payload else {
            bail!("Hugging Face returned invalid search results");
        };
        let mut models = Vec::new();
        for value in values {
            let Value::Object(model) = value else {
                continue;
            };
            let Some(Value::String(id)) = model.get("id") else {
                continue;
            };
            let downloads = model.get("downloads").and_then(Value::as_u64).unwrap_or(0);
            models.push(HuggingFaceModel {
                id: id.clone(),
                downloads,
            });
        }
        Ok(models)
    }

    /// Fetch detailed metadata (gating + quantizations) for model `id`
    /// (`details`).
    pub fn details(
        &self,
        id: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<HuggingFaceModelDetails> {
        let encoded_id = id
            .split('/')
            .map(encode_uri_component)
            .collect::<Vec<_>>()
            .join("/");
        let payload = self.request(&format!("/api/models/{encoded_id}?blobs=true"), signal)?;
        let Value::Object(model) = payload else {
            bail!("Hugging Face returned invalid model details");
        };

        // Aggregate shard sizes per quantization, preserving first-seen order.
        let mut sizes: Vec<(String, ShardSize)> = Vec::new();
        if let Some(Value::Array(siblings)) = model.get("siblings") {
            for value in siblings {
                let Value::Object(file) = value else {
                    continue;
                };
                let Some(Value::String(rfilename)) = file.get("rfilename") else {
                    continue;
                };
                if !rfilename.to_lowercase().ends_with(".gguf") {
                    continue;
                }
                let filename = rfilename.rsplit('/').next().unwrap_or(rfilename);
                if filename.to_lowercase().starts_with("mmproj") {
                    continue;
                }
                let stem = &filename[..filename.len() - 5];
                let stem = shard_suffix_pattern().replace(stem, "");
                let Some(quantization) = quantization_pattern()
                    .captures(&stem)
                    .and_then(|captures| captures.get(1))
                    .map(|group| group.as_str().to_uppercase())
                else {
                    continue;
                };
                let index = match sizes.iter().position(|(name, _)| *name == quantization) {
                    Some(index) => index,
                    None => {
                        sizes.push((
                            quantization,
                            ShardSize {
                                total: 0.0,
                                complete: true,
                            },
                        ));
                        sizes.len() - 1
                    }
                };
                let entry = &mut sizes[index].1;
                match file.get("size").and_then(Value::as_f64) {
                    Some(size) => entry.total += size,
                    None => entry.complete = false,
                }
            }
        }

        let mut quantizations: Vec<HuggingFaceQuantization> = sizes
            .into_iter()
            .map(|(name, size)| HuggingFaceQuantization {
                name,
                size: if size.complete {
                    Some(size.total)
                } else {
                    None
                },
            })
            .collect();
        quantizations.sort_by(|left, right| {
            if left.name == "Q4_K_M" {
                return std::cmp::Ordering::Less;
            }
            if right.name == "Q4_K_M" {
                return std::cmp::Ordering::Greater;
            }
            let left_size = left.size.unwrap_or(MAX_SAFE_INTEGER);
            let right_size = right.size.unwrap_or(MAX_SAFE_INTEGER);
            match left_size
                .partial_cmp(&right_size)
                .unwrap_or(std::cmp::Ordering::Equal)
            {
                std::cmp::Ordering::Equal => left.name.cmp(&right.name),
                other => other,
            }
        });

        let id = match model.get("id") {
            Some(Value::String(model_id)) => model_id.clone(),
            _ => id.to_string(),
        };
        let gated = match model.get("gated") {
            Some(Value::String(value)) if value == "auto" => HuggingFaceGated::Auto,
            Some(Value::String(value)) if value == "manual" => HuggingFaceGated::Manual,
            _ => HuggingFaceGated::No,
        };
        Ok(HuggingFaceModelDetails {
            id,
            gated,
            quantizations,
        })
    }
}

/// Running per-quantization shard-size accumulator.
struct ShardSize {
    total: f64,
    complete: bool,
}

/// Percent-encode a single path segment, mirroring `encodeURIComponent`.
fn encode_uri_component(segment: &str) -> String {
    let mut encoded = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Serialize key/value pairs as an `application/x-www-form-urlencoded` query
/// string, mirroring `URLSearchParams` (space becomes `+`).
fn encode_query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                encode_form_component(key),
                encode_form_component(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode a form component, encoding space as `+` like `URLSearchParams`.
fn encode_form_component(component: &str) -> String {
    let mut encoded = String::new();
    for byte in component.bytes() {
        match byte {
            b' ' => encoded.push('+'),
            b if b.is_ascii_alphanumeric() || matches!(b, b'*' | b'-' | b'.' | b'_') => {
                encoded.push(b as char)
            }
            b => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use pidgin_ai::seams::http::{HttpResponse, ScriptedTransport};
    use std::collections::BTreeMap;

    /// Mirrors the pi test "searches Hugging Face and reads quantizations plus
    /// access requirements" (`test/llama-extension.test.ts`).
    #[test]
    fn searches_hugging_face_and_reads_quantizations_and_access() {
        let transport = ScriptedTransport::new();
        transport.push_ok(r#"[{"id":"owner/model-GGUF","downloads":1200}]"#);
        transport.push_ok(
            r#"{
                "id": "owner/model-GGUF",
                "gated": "manual",
                "siblings": [
                    {"rfilename": "model-Q5_K_M.gguf", "size": 6000},
                    {"rfilename": "model-Q4_K_M-00001-of-00002.gguf", "size": 2000},
                    {"rfilename": "model-Q4_K_M-00002-of-00002.gguf", "size": 3000},
                    {"rfilename": "mmproj-F16.gguf", "size": 1000}
                ]
            }"#,
        );
        let transport: Arc<dyn HttpTransport> = Arc::new(transport.clone());
        let client = HuggingFaceClient::new(
            transport,
            Some("hf-secret".to_string()),
            Some("http://localhost".to_string()),
        );

        let results = client.search("qwen coder", None).unwrap();
        assert_eq!(
            results,
            vec![HuggingFaceModel {
                id: "owner/model-GGUF".to_string(),
                downloads: 1200,
            }]
        );

        let details = client.details("owner/model-GGUF", None).unwrap();
        assert_eq!(
            details,
            HuggingFaceModelDetails {
                id: "owner/model-GGUF".to_string(),
                gated: HuggingFaceGated::Manual,
                quantizations: vec![
                    HuggingFaceQuantization {
                        name: "Q4_K_M".to_string(),
                        size: Some(5000.0),
                    },
                    HuggingFaceQuantization {
                        name: "Q5_K_M".to_string(),
                        size: Some(6000.0),
                    },
                ],
            }
        );

        let env = HashMap::from([("HF_TOKEN".to_string(), " hf-secret ".to_string())]);
        assert_eq!(find_hugging_face_token(&env), Some("hf-secret".to_string()));
    }

    /// The search request carries the auth header and the expected query params.
    #[test]
    fn search_request_carries_auth_and_query_params() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok("[]");
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let client = HuggingFaceClient::new(
            transport,
            Some("hf-secret".to_string()),
            Some("http://localhost/".to_string()),
        );
        client.search("qwen coder", None).unwrap();

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer hf-secret")
        );
        // Trailing slash on the base URL is trimmed.
        assert!(request.url.starts_with("http://localhost/api/models?"));
        assert!(request.url.contains("search=qwen+coder"));
        assert!(request.url.contains("filter=gguf"));
        assert!(request.url.contains("sort=downloads"));
        assert!(request.url.contains("direction=-1"));
        assert!(request.url.contains("limit=20"));
    }

    /// Unknown-size shards drop the aggregate size but keep the quantization.
    #[test]
    fn missing_shard_size_marks_quantization_incomplete() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(
            r#"{
                "id": "owner/model",
                "siblings": [
                    {"rfilename": "model-Q6_K-00001-of-00002.gguf", "size": 100},
                    {"rfilename": "model-Q6_K-00002-of-00002.gguf"}
                ]
            }"#,
        );
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let client = HuggingFaceClient::new(transport, None, Some("http://localhost".to_string()));

        let details = client.details("owner/model", None).unwrap();
        assert_eq!(details.gated, HuggingFaceGated::No);
        assert_eq!(
            details.quantizations,
            vec![HuggingFaceQuantization {
                name: "Q6_K".to_string(),
                size: None,
            }]
        );
    }

    /// `Q4_K_M` always sorts first; the rest ascend by size then name.
    #[test]
    fn quantizations_sort_q4_k_m_first_then_by_size() {
        let scripted = ScriptedTransport::new();
        scripted.push_ok(
            r#"{
                "id": "owner/model",
                "siblings": [
                    {"rfilename": "model-Q8_0.gguf", "size": 8000},
                    {"rfilename": "model-Q5_K_M.gguf", "size": 5000},
                    {"rfilename": "model-Q4_K_M.gguf", "size": 4000},
                    {"rfilename": "model-Q3_K_S.gguf", "size": 3000}
                ]
            }"#,
        );
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let client = HuggingFaceClient::new(transport, None, Some("http://localhost".to_string()));

        let details = client.details("owner/model", None).unwrap();
        let names: Vec<&str> = details
            .quantizations
            .iter()
            .map(|quantization| quantization.name.as_str())
            .collect();
        assert_eq!(names, vec!["Q4_K_M", "Q3_K_S", "Q5_K_M", "Q8_0"]);
    }

    /// A 429 with a `retry-after` header surfaces pi's rate-limit message.
    #[test]
    fn rate_limit_error_includes_retry_delay() {
        let scripted = ScriptedTransport::new();
        scripted.push_response(Ok(HttpResponse {
            status: 429,
            headers: BTreeMap::from([("retry-after".to_string(), "12".to_string())]),
            body: String::new(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let client = HuggingFaceClient::new(transport, None, Some("http://localhost".to_string()));

        let error = client.search("q", None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Hugging Face rate limit reached; retry in 12s"
        );
    }

    /// A non-2xx error surfaces the payload's `error` string.
    #[test]
    fn http_error_surfaces_payload_error() {
        let scripted = ScriptedTransport::new();
        scripted.push_response(Ok(HttpResponse {
            status: 404,
            headers: BTreeMap::new(),
            body: r#"{"error":"Repository not found"}"#.to_string(),
        }));
        let transport: Arc<dyn HttpTransport> = Arc::new(scripted.clone());
        let client = HuggingFaceClient::new(transport, None, Some("http://localhost".to_string()));

        let error = client.details("owner/missing", None).unwrap_err();
        assert_eq!(error.to_string(), "Repository not found");
    }

    /// `find_hugging_face_token` walks the token files in pi's order.
    #[test]
    fn find_token_reads_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let hf_home = dir.path().join("hf_home");
        std::fs::create_dir_all(&hf_home).unwrap();
        let hf_home_token = hf_home.join("token");
        std::fs::write(&hf_home_token, "  home-token  \n").unwrap();

        let xdg = dir.path().join("xdg");
        std::fs::create_dir_all(xdg.join("huggingface")).unwrap();
        std::fs::write(xdg.join("huggingface").join("token"), "xdg-token").unwrap();

        // HF_HOME/token wins over XDG_CACHE_HOME because it comes first in order.
        let env = HashMap::from([
            (
                "HF_HOME".to_string(),
                hf_home.to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME".to_string(),
                xdg.to_string_lossy().into_owned(),
            ),
        ]);
        assert_eq!(
            find_hugging_face_token(&env),
            Some("home-token".to_string())
        );

        // HF_TOKEN_PATH takes precedence over every other file.
        let explicit = dir.path().join("explicit-token");
        std::fs::write(&explicit, "explicit-token").unwrap();
        let env = HashMap::from([
            (
                "HF_TOKEN_PATH".to_string(),
                explicit.to_string_lossy().into_owned(),
            ),
            (
                "HF_HOME".to_string(),
                hf_home.to_string_lossy().into_owned(),
            ),
        ]);
        assert_eq!(
            find_hugging_face_token(&env),
            Some("explicit-token".to_string())
        );

        // HF_TOKEN short-circuits the file lookup entirely.
        let env = HashMap::from([("HF_TOKEN".to_string(), " env-token ".to_string())]);
        assert_eq!(find_hugging_face_token(&env), Some("env-token".to_string()));

        // A configured-but-absent file path yields no token. (The final HOME
        // fallback is environment-dependent, so we assert on the file read.)
        let missing = dir.path().join("does-not-exist");
        assert_eq!(read_token(&missing.to_string_lossy()), None);
    }
}
