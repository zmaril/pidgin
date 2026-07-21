// straitjacket-allow-file:duplication — these tests transcribe pi's Google
// model fixtures verbatim: the hand-built `GoogleModel` helper and the asserted
// config/chunk JSON literals are near-identical to the sibling Google test files
// by design, and the clone detector reads them as duplicates. They are distinct,
// load-bearing fixtures kept parallel to pi's test cases.
//! Unit tests for the direct Gemini client-config builder and the napi decode
//! entry point. The stream-decode assertions live with the shared helper
//! (`google_shared/tests.rs`); here we cover the `createClient` config shape
//! unique to this driver and the JSON boundary wrapper.

use super::*;
use crate::types::ModelCost;

fn model(base_url: &str, headers: Option<BTreeMap<String, String>>) -> GoogleModel {
    GoogleModel {
        id: "gemini-2.5-flash".to_string(),
        api: "google-generative-ai".to_string(),
        provider: "google".to_string(),
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
        headers,
    }
}

#[test]
fn client_config_carries_api_key_and_omits_http_options_without_base_url() {
    let options = GoogleClientOptions {
        api_key: Some("AIzaKey".to_string()),
        headers: BTreeMap::new(),
    };
    let config = build_client_config(&model("", None), &options);
    assert_eq!(config["apiKey"], serde_json::json!("AIzaKey"));
    assert!(config.get("httpOptions").is_none());
}

#[test]
fn client_config_sets_base_url_and_suppresses_version_path() {
    let options = GoogleClientOptions {
        api_key: Some("AIzaKey".to_string()),
        headers: BTreeMap::new(),
    };
    let config = build_client_config(&model("https://proxy.example.com/v1", None), &options);
    let http = &config["httpOptions"];
    assert_eq!(
        http["baseUrl"],
        serde_json::json!("https://proxy.example.com/v1")
    );
    assert_eq!(http["apiVersion"], serde_json::json!(""));
}

#[test]
fn client_config_merges_model_and_option_headers() {
    let mut model_headers = BTreeMap::new();
    model_headers.insert("x-model".to_string(), "m".to_string());
    let mut option_headers = BTreeMap::new();
    option_headers.insert("x-opt".to_string(), "o".to_string());

    let options = GoogleClientOptions {
        api_key: Some("AIzaKey".to_string()),
        headers: option_headers,
    };
    let config = build_client_config(&model("", Some(model_headers)), &options);
    let headers = &config["httpOptions"]["headers"];
    assert_eq!(headers["x-model"], serde_json::json!("m"));
    assert_eq!(headers["x-opt"], serde_json::json!("o"));
}

#[test]
fn parse_stream_to_json_round_trips() {
    let chunks = serde_json::json!([{
        "candidates": [{
            "content": { "parts": [{ "text": "hello" }] },
            "finishReason": "STOP",
        }],
        "usageMetadata": { "promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2 },
    }])
    .to_string();
    let model_json = serde_json::json!({
        "id": "gemini-2.5-flash",
        "provider": "google",
        "api": "google-generative-ai",
        "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0 },
    })
    .to_string();

    let out = parse_stream_to_json(&chunks, &model_json, 0).expect("json");
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["message"]["stopReason"], serde_json::json!("stop"));
    assert_eq!(
        parsed["message"]["content"][0]["text"],
        serde_json::json!("hello")
    );
    // The event stream terminates in a `done` event.
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(events.last().unwrap()["type"], serde_json::json!("done"));
}
