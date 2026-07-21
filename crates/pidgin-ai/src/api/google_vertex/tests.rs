// straitjacket-allow-file[:duplication] — these tests transcribe pi's Vertex
// api-key-resolution fixtures verbatim: the per-case option/model literals and
// the asserted client-config objects are near-identical by design, and the clone
// detector reads them as duplicates. They are distinct, load-bearing fixtures
// kept parallel to pi's test cases.
//! Unit tests for the Vertex auth-shape logic, porting the assertions from pi's
//! `packages/ai/test/google-vertex-api-key-resolution.test.ts`.
//!
//! pi mocks the `@google/genai` `GoogleGenAI` constructor and asserts on the
//! config object it receives. In pidgin's pure-function model the equivalent is
//! asserting on the [`build_client_config`] output — the value the driver would
//! hand to that constructor.

use super::*;
use crate::types::ModelCost;

fn vertex_model(base_url: &str) -> GoogleModel {
    GoogleModel {
        id: "gemini-3-flash-preview".to_string(),
        api: "google-vertex".to_string(),
        provider: "google-vertex".to_string(),
        base_url: base_url.to_string(),
        reasoning: true,
        input: vec![],
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
    }
}

/// The default Vertex model base URL still carries the `{location}` template
/// placeholder, so it resolves to no custom base URL (httpOptions omitted).
fn default_model() -> GoogleModel {
    vertex_model(
        "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}",
    )
}

fn opts() -> GoogleVertexClientOptions {
    GoogleVertexClientOptions::default()
}

#[test]
fn falls_back_to_adc_when_api_key_is_placeholder_marker() {
    let mut options = opts();
    options.api_key = Some("<authenticated>".to_string());
    options.project = Some("test-project".to_string());
    options.location = Some("us-central1".to_string());

    let config = build_client_config(&default_model(), &options).expect("config");
    assert_eq!(config["vertexai"], json!(true));
    assert_eq!(config["project"], json!("test-project"));
    assert_eq!(config["location"], json!("us-central1"));
    assert_eq!(config["apiVersion"], json!("v1"));
    assert!(config.get("apiKey").is_none());
}

#[test]
fn falls_back_to_adc_when_api_key_is_gcp_marker() {
    let mut options = opts();
    options.api_key = Some("gcp-vertex-credentials".to_string());
    options.project = Some("test-project".to_string());
    options.location = Some("us-central1".to_string());

    let config = build_client_config(&default_model(), &options).expect("config");
    assert_eq!(config["vertexai"], json!(true));
    assert_eq!(config["project"], json!("test-project"));
    assert_eq!(config["location"], json!("us-central1"));
    assert_eq!(config["apiVersion"], json!("v1"));
    assert!(config.get("apiKey").is_none());
}

#[test]
fn falls_back_to_adc_when_no_api_key_supplied() {
    // pi's env-placeholder case: GOOGLE_CLOUD_API_KEY is a compat/getModel concern,
    // not read by the driver; with no options.apiKey the driver takes the ADC path.
    let mut options = opts();
    options.project = Some("test-project".to_string());
    options.location = Some("us-central1".to_string());

    let config = build_client_config(&default_model(), &options).expect("config");
    assert_eq!(config["vertexai"], json!(true));
    assert_eq!(config["project"], json!("test-project"));
    assert_eq!(config["location"], json!("us-central1"));
    assert_eq!(config["apiVersion"], json!("v1"));
    assert!(config.get("apiKey").is_none());
}

#[test]
fn uses_api_key_client_for_real_api_keys() {
    let mut options = opts();
    options.api_key = Some("AIzaSyExampleRealisticLookingApiKey123456".to_string());

    let config = build_client_config(&default_model(), &options).expect("config");
    assert_eq!(config["vertexai"], json!(true));
    assert_eq!(
        config["apiKey"],
        json!("AIzaSyExampleRealisticLookingApiKey123456")
    );
    assert_eq!(config["apiVersion"], json!("v1"));
    assert!(config.get("project").is_none());
    assert!(config.get("location").is_none());
}

#[test]
fn does_not_forward_generated_base_url_placeholders() {
    let mut options = opts();
    options.project = Some("test-project".to_string());
    options.location = Some("us-central1".to_string());

    let config = build_client_config(&default_model(), &options).expect("config");
    assert!(config.get("httpOptions").is_none());
}

#[test]
fn forwards_custom_base_url_to_adc_client() {
    let mut options = opts();
    options.project = Some("test-project".to_string());
    options.location = Some("us-central1".to_string());

    let config =
        build_client_config(&vertex_model("https://proxy.example.com"), &options).expect("config");
    assert_eq!(config["vertexai"], json!(true));
    assert_eq!(config["project"], json!("test-project"));
    assert_eq!(config["location"], json!("us-central1"));
    assert_eq!(config["apiVersion"], json!("v1"));
    let http = &config["httpOptions"];
    assert_eq!(http["baseUrl"], json!("https://proxy.example.com"));
    assert_eq!(http["baseUrlResourceScope"], json!("COLLECTION"));
    assert!(http.get("apiVersion").is_none());
}

#[test]
fn forwards_custom_base_url_to_api_key_client() {
    let mut options = opts();
    options.api_key = Some("AIzaSyExampleRealisticLookingApiKey123456".to_string());

    let config =
        build_client_config(&vertex_model("https://proxy.example.com"), &options).expect("config");
    assert_eq!(config["vertexai"], json!(true));
    assert_eq!(
        config["apiKey"],
        json!("AIzaSyExampleRealisticLookingApiKey123456")
    );
    assert_eq!(config["apiVersion"], json!("v1"));
    let http = &config["httpOptions"];
    assert_eq!(http["baseUrl"], json!("https://proxy.example.com"));
    assert_eq!(http["baseUrlResourceScope"], json!("COLLECTION"));
}

#[test]
fn does_not_append_api_version_when_base_url_includes_one() {
    let mut options = opts();
    options.project = Some("test-project".to_string());
    options.location = Some("us-central1".to_string());

    let config = build_client_config(
        &vertex_model("https://proxy.example.com/v1/projects/test-project/locations/global"),
        &options,
    )
    .expect("config");
    let http = &config["httpOptions"];
    assert_eq!(
        http["baseUrl"],
        json!("https://proxy.example.com/v1/projects/test-project/locations/global")
    );
    assert_eq!(http["baseUrlResourceScope"], json!("COLLECTION"));
    assert_eq!(http["apiVersion"], json!(""));
}

// ---------------------------------------------------------------------------
// resolveApiKey / ADC error paths (local coverage of the auth helpers)
// ---------------------------------------------------------------------------

#[test]
fn resolve_api_key_discards_empty_marker_and_placeholder() {
    assert!(resolve_api_key(&opts()).is_none());

    let mut blank = opts();
    blank.api_key = Some("   ".to_string());
    assert!(resolve_api_key(&blank).is_none());

    let mut marker = opts();
    marker.api_key = Some("gcp-vertex-credentials".to_string());
    assert!(resolve_api_key(&marker).is_none());

    let mut placeholder = opts();
    placeholder.api_key = Some("<authenticated>".to_string());
    assert!(resolve_api_key(&placeholder).is_none());

    let mut real = opts();
    real.api_key = Some("  AIzaReal  ".to_string());
    assert_eq!(resolve_api_key(&real).as_deref(), Some("AIzaReal"));
}

#[test]
fn adc_path_errors_without_project_or_location() {
    let err = build_client_config(&default_model(), &opts()).unwrap_err();
    assert!(err.contains("project ID"));

    let mut with_project = opts();
    with_project.project = Some("p".to_string());
    let err = build_client_config(&default_model(), &with_project).unwrap_err();
    assert!(err.contains("location"));
}

#[test]
fn adc_reads_project_location_and_credentials_from_env() {
    let mut options = opts();
    options.env.insert(
        "GOOGLE_CLOUD_PROJECT".to_string(),
        "env-project".to_string(),
    );
    options.env.insert(
        "GOOGLE_CLOUD_LOCATION".to_string(),
        "europe-west4".to_string(),
    );
    options.env.insert(
        "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
        "/creds/key.json".to_string(),
    );

    let config = build_client_config(&default_model(), &options).expect("config");
    assert_eq!(config["project"], json!("env-project"));
    assert_eq!(config["location"], json!("europe-west4"));
    assert_eq!(
        config["googleAuthOptions"],
        json!({ "keyFilename": "/creds/key.json" })
    );
}

// ---------------------------------------------------------------------------
// ADC / service-account request assembly (the Bearer wire format the driver
// puts on the wire once `super::adc` has minted a token)
// ---------------------------------------------------------------------------

#[test]
fn assemble_adc_request_targets_regional_endpoint_with_bearer() {
    let request = client::assemble_adc_request(
        &default_model(),
        "{}".to_string(),
        "ya29.minted-token",
        "test-project",
        "us-central1",
        &BTreeMap::new(),
    );

    assert_eq!(request.method, "POST");
    // No apiKey => the regional `{location}-aiplatform` host with the full
    // `projects/{project}/locations/{location}` resource prefix.
    assert_eq!(
        request.url,
        "https://us-central1-aiplatform.googleapis.com/v1/projects/test-project/locations/us-central1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
    );
    // The minted token is sent as `Authorization: Bearer`, not `x-goog-api-key`.
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer ya29.minted-token")
    );
    assert!(!request.headers.contains_key("x-goog-api-key"));
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
}

#[test]
fn assemble_adc_request_honors_custom_base_url_over_regional_host() {
    // A custom `model.baseUrl` (no `{location}` placeholder) overrides the host
    // but keeps the ADC `projects/{project}/locations/{location}` prefix and the
    // appended `v1` version (COLLECTION scope).
    let request = client::assemble_adc_request(
        &vertex_model("https://proxy.example.com"),
        "{}".to_string(),
        "ya29.minted-token",
        "test-project",
        "us-central1",
        &BTreeMap::new(),
    );

    assert_eq!(
        request.url,
        "https://proxy.example.com/v1/projects/test-project/locations/us-central1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
    );
}
