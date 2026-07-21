// straitjacket-allow-file[:duplication] — these tests transcribe pi's
// `test/openrouter-images.test.ts` fixtures verbatim: the `ImagesModel` literals
// and the mocked chat-completions response body are near-identical JSON walls by
// design, and the clone detector reads them as duplicates. They are distinct,
// load-bearing wire fixtures kept faithful to pi's cases.
//! Unit tests for the OpenRouter image dialect, mirroring pi's
//! `packages/ai/test/openrouter-images.test.ts` cases. pi mocks the `openai`
//! client; this port injects a [`ScriptedTransport`] in its place.

use super::*;

use std::collections::BTreeMap;

use crate::seams::http::ScriptedTransport;
use crate::types::ModelCost;

const NOW_MS: i64 = 1_700_000_000_000;

/// The mocked chat-completions response body pi's fake `openai` client returns.
fn mock_response_body() -> String {
    json!({
        "id": "img-1",
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 34,
            "prompt_tokens_details": { "cached_tokens": 0 }
        },
        "choices": [
            {
                "message": {
                    "content": "Here is your image.",
                    "images": [{ "image_url": "data:image/png;base64,ZmFrZS1wbmc=" }]
                }
            }
        ]
    })
    .to_string()
}

fn cost() -> ModelCost {
    ModelCost {
        input: 0.015,
        output: 0.03,
        cache_read: 0.0,
        cache_write: 0.0,
        tiers: None,
    }
}

fn text_and_image_model() -> ImagesModel {
    ImagesModel {
        id: "google/gemini-3.1-flash-image-preview".into(),
        name: "Gemini 3.1 Flash Image Preview".into(),
        api: "openrouter-images".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        thinking_level_map: None,
        input: vec![Modality::Text, Modality::Image],
        cost: cost(),
        headers: Some(BTreeMap::from([(
            "HTTP-Referer".to_string(),
            "https://example.com".to_string(),
        )])),
        output: vec![Modality::Text, Modality::Image],
    }
}

fn image_only_model() -> ImagesModel {
    ImagesModel {
        id: "black-forest-labs/flux.2-pro".into(),
        name: "FLUX.2 Pro".into(),
        api: "openrouter-images".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        thinking_level_map: None,
        input: vec![Modality::Text, Modality::Image],
        cost: cost(),
        headers: None,
        output: vec![Modality::Image],
    }
}

fn context() -> ImagesContext {
    ImagesContext {
        input: vec![ImagesInputContent::Text {
            text: "Generate a dog".into(),
            text_signature: None,
        }],
    }
}

fn options_with_key() -> ImagesOptions {
    ImagesOptions {
        api_key: Some("test".into()),
        ..ImagesOptions::default()
    }
}

#[test]
fn returns_text_plus_images_in_final_output() {
    let transport = ScriptedTransport::new();
    transport.push_ok(mock_response_body());

    let model = text_and_image_model();
    let output = generate_images(
        &transport,
        NOW_MS,
        &model,
        &context(),
        Some(&options_with_key()),
        None,
    );

    assert_eq!(output.stop_reason, ImagesStopReason::Stop);
    assert_eq!(output.response_id.as_deref(), Some("img-1"));
    assert_eq!(
        output.output[0],
        ImagesInputContent::Text {
            text: "Here is your image.".into(),
            text_signature: None,
        }
    );
    assert_eq!(
        output.output[1],
        ImagesInputContent::Image {
            mime_type: "image/png".into(),
            data: "ZmFrZS1wbmc=".into(),
        }
    );

    // pi inspects the request the fake client saw. Here it is the request body.
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.ends_with("/chat/completions"));
    let params: Value = serde_json::from_str(requests[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(params["stream"], json!(false));
    assert_eq!(params["modalities"], json!(["image", "text"]));
    assert_eq!(
        params["messages"][0]["content"][0],
        json!({ "type": "text", "text": "Generate a dog" })
    );
    // The provider default header rides along, as pi's `defaultHeaders` do.
    assert_eq!(
        requests[0].headers.get("HTTP-Referer").map(String::as_str),
        Some("https://example.com")
    );
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some("Bearer test")
    );
}

#[test]
fn passes_through_abort_signal_and_returns_aborted_result() {
    let transport = ScriptedTransport::new();
    // No scripted response: an aborted request must never reach the transport.
    let signal = AbortSignal::aborted();

    let model = image_only_model();
    let output = generate_images(
        &transport,
        NOW_MS,
        &model,
        &context(),
        Some(&options_with_key()),
        Some(&signal),
    );

    assert_eq!(output.stop_reason, ImagesStopReason::Aborted);
    assert_eq!(output.error_message.as_deref(), Some("Request aborted"));
    // pi asserts the abort signal reached the client; the Rust analog is that the
    // aborted request short-circuits before any transport call.
    assert!(transport.requests().is_empty());
}

#[test]
fn generate_images_resolves_the_final_assistant_images_result() {
    let transport = ScriptedTransport::new();
    transport.push_ok(mock_response_body());

    let model = image_only_model();
    let output = generate_images(
        &transport,
        NOW_MS,
        &model,
        &context(),
        Some(&options_with_key()),
        None,
    );

    assert!(output
        .output
        .iter()
        .any(|item| matches!(item, ImagesInputContent::Image { .. })));
}

#[test]
fn missing_api_key_returns_error_result() {
    let transport = ScriptedTransport::new();
    let model = image_only_model();
    let output = generate_images(
        &transport,
        NOW_MS,
        &model,
        &context(),
        Some(&ImagesOptions::default()),
        None,
    );
    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        output.error_message.as_deref(),
        Some("No API key for provider: openrouter")
    );
}

#[test]
fn parses_usage_into_pi_shape() {
    let transport = ScriptedTransport::new();
    transport.push_ok(mock_response_body());
    let model = image_only_model();
    let output = generate_images(
        &transport,
        NOW_MS,
        &model,
        &context(),
        Some(&options_with_key()),
        None,
    );
    let usage = output.usage.expect("usage parsed");
    assert_eq!(usage.input, 12);
    assert_eq!(usage.output, 34);
    assert_eq!(usage.cache_read, 0);
    assert_eq!(usage.total_tokens, 46);
}
