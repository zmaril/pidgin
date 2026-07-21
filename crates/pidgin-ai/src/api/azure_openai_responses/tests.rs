// straitjacket-allow-file:duplication — these tests mirror pi's Azure
// fixtures verbatim: the `response.*` named-event objects and per-test model
// literals are walls of near-identical JSON by design, kept faithful to pi's
// `azure-openai-*.test.ts` cases, and the clone detector reads them as
// duplicates.
//! Tests for the Azure OpenAI Responses wrapper, mirroring pi's
//! `azure-openai-base-url.test.ts` (URL normalization + prompt-cache clamp +
//! store:false) and `azure-openai-responses-reasoning-replay.test.ts` (the
//! encrypted_content backfill through the shared stream core).

use super::*;
use crate::api::openai_responses::OpenAIResponsesModel;
use crate::api::openai_responses_shared::{
    convert_responses_messages, process_responses_stream, ResponsesStreamOptions,
};
use crate::types::{
    AssistantMessage, Context, Message, Modality, ModelCost, UserContent, UserMessage, UserRole,
};
use serde_json::{json, Value};

fn azure_model() -> OpenAIResponsesModel {
    OpenAIResponsesModel {
        id: "gpt-5-mini".to_string(),
        api: "azure-openai-responses".to_string(),
        provider: "azure-openai-responses".to_string(),
        base_url: "https://example.invalid".to_string(),
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        reasoning: true,
        thinking_level_map: None,
        input: vec![Modality::Text],
        headers: None,
        compat: None,
    }
}

// ---------------------------------------------------------------------------
// Base URL normalization (azure-openai-base-url.test.ts)
// ---------------------------------------------------------------------------

#[test]
fn normalizes_cognitive_services_root_to_openai_v1() {
    assert_eq!(
        normalize_azure_base_url("https://marc-quicktests-resource.cognitiveservices.azure.com")
            .unwrap(),
        "https://marc-quicktests-resource.cognitiveservices.azure.com/openai/v1"
    );
}

#[test]
fn normalizes_microsoft_foundry_root_to_openai_v1() {
    assert_eq!(
        normalize_azure_base_url("https://marc-quicktests-resource.ai.azure.com").unwrap(),
        "https://marc-quicktests-resource.ai.azure.com/openai/v1"
    );
}

#[test]
fn normalizes_azure_openai_root_to_openai_v1() {
    assert_eq!(
        normalize_azure_base_url("https://my-resource.openai.azure.com").unwrap(),
        "https://my-resource.openai.azure.com/openai/v1"
    );
}

#[test]
fn normalizes_openai_path_to_openai_v1() {
    assert_eq!(
        normalize_azure_base_url("https://my-resource.cognitiveservices.azure.com/openai").unwrap(),
        "https://my-resource.cognitiveservices.azure.com/openai/v1"
    );
}

#[test]
fn preserves_openai_v1_endpoints() {
    assert_eq!(
        normalize_azure_base_url("https://my-resource.cognitiveservices.azure.com/openai/v1")
            .unwrap(),
        "https://my-resource.cognitiveservices.azure.com/openai/v1"
    );
}

#[test]
fn normalizes_openai_v1_responses_to_openai_v1() {
    assert_eq!(
        normalize_azure_base_url("https://my-resource.services.ai.azure.com/openai/v1/responses")
            .unwrap(),
        "https://my-resource.services.ai.azure.com/openai/v1"
    );
}

#[test]
fn preserves_non_azure_proxy_paths() {
    assert_eq!(
        normalize_azure_base_url("https://my-proxy.example.com/v1").unwrap(),
        "https://my-proxy.example.com/v1"
    );
}

#[test]
fn strips_query_params_on_azure_hosts() {
    assert_eq!(
        normalize_azure_base_url(
            "https://my-resource.openai.azure.com/openai?api-version=2024-12-01"
        )
        .unwrap(),
        "https://my-resource.openai.azure.com/openai/v1"
    );
}

#[test]
fn preserves_query_params_on_non_azure_proxy() {
    assert_eq!(
        normalize_azure_base_url("https://my-proxy.example.com/v1?custom=true").unwrap(),
        "https://my-proxy.example.com/v1?custom=true"
    );
}

#[test]
fn errors_on_invalid_url() {
    let err = normalize_azure_base_url("not-a-url").unwrap_err();
    assert!(err.contains("Invalid Azure OpenAI base URL"));
}

#[test]
fn builds_default_base_url_from_resource_name() {
    let options = AzureOpenAIResponsesOptions {
        azure_resource_name: Some("my-resource".to_string()),
        ..Default::default()
    };
    let config = resolve_azure_config(&azure_model(), &options).unwrap();
    assert_eq!(
        config.base_url,
        "https://my-resource.openai.azure.com/openai/v1"
    );
    assert_eq!(config.api_version, DEFAULT_AZURE_API_VERSION);
}

// ---------------------------------------------------------------------------
// build_params (prompt-cache clamp + store:false + deployment name)
// ---------------------------------------------------------------------------

fn user_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".to_string()),
            timestamp: 0,
        })],
        tools: None,
    }
}

#[test]
fn build_params_clamps_prompt_cache_key_and_disables_store() {
    let options = AzureOpenAIResponsesOptions {
        session_id: Some("x".repeat(67)),
        ..Default::default()
    };
    let params = build_params(&azure_model(), &user_context(), &options, "my-deployment");
    assert_eq!(params["prompt_cache_key"], json!("x".repeat(64)));
    assert_eq!(params["store"], json!(false));
    // The deployment name (not model.id) is the wire `model`.
    assert_eq!(params["model"], json!("my-deployment"));
}

#[test]
fn resolve_deployment_name_prefers_explicit_then_map_then_model_id() {
    let model = azure_model();
    let explicit = AzureOpenAIResponsesOptions {
        azure_deployment_name: Some("explicit-dep".to_string()),
        ..Default::default()
    };
    assert_eq!(resolve_deployment_name(&model, &explicit), "explicit-dep");

    let mapped = AzureOpenAIResponsesOptions {
        deployment_name_map: Some("gpt-5-mini=mapped-dep,other=x".to_string()),
        ..Default::default()
    };
    assert_eq!(resolve_deployment_name(&model, &mapped), "mapped-dep");

    let fallback = AzureOpenAIResponsesOptions::default();
    assert_eq!(resolve_deployment_name(&model, &fallback), "gpt-5-mini");
}

// ---------------------------------------------------------------------------
// Reasoning replay (azure-openai-responses-reasoning-replay.test.ts)
// ---------------------------------------------------------------------------

fn replay_events(done_item: Value, completed_item: Value) -> Vec<Value> {
    let id = done_item
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();
    vec![
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "sequence_number": 0,
            "item": { "type": "reasoning", "id": id, "summary": [] }
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 1,
            "item": done_item
        }),
        json!({
            "type": "response.completed",
            "sequence_number": 2,
            "response": { "id": "resp_test", "status": "completed", "output": [completed_item] }
        }),
    ]
}

fn replayed_reasoning(model: &OpenAIResponsesModel, output: &AssistantMessage) -> Value {
    let messages = vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("first".to_string()),
            timestamp: 0,
        }),
        Message::Assistant(output.clone()),
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("follow-up".to_string()),
            timestamp: 1,
        }),
    ];
    let input =
        convert_responses_messages(model, &messages, None, &["azure-openai-responses"], true);
    input
        .into_iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .expect("replayed reasoning item present")
}

#[test]
fn preserves_existing_encrypted_content_from_output_item_done() {
    let model = azure_model();
    let done_item = json!({
        "type": "reasoning",
        "id": "rs_done",
        "summary": [],
        "encrypted_content": "from-output-item-done"
    });
    let completed_item = json!({
        "type": "reasoning",
        "id": "rs_done",
        "summary": [],
        "encrypted_content": "from-response-completed"
    });

    let outcome = process_responses_stream(
        &replay_events(done_item, completed_item),
        &model,
        &ResponsesStreamOptions::default(),
        0,
    );
    let reasoning = replayed_reasoning(&model, &outcome.message);
    assert_eq!(reasoning["id"], json!("rs_done"));
    assert_eq!(
        reasoning["encrypted_content"],
        json!("from-output-item-done")
    );
}

#[test]
fn fills_encrypted_content_when_output_item_done_omitted_it() {
    let model = azure_model();
    let done_item = json!({
        "type": "reasoning",
        "id": "rs_missing",
        "summary": []
    });
    let completed_item = json!({
        "type": "reasoning",
        "id": "rs_missing",
        "summary": [],
        "encrypted_content": "from-response-completed"
    });

    let outcome = process_responses_stream(
        &replay_events(done_item, completed_item),
        &model,
        &ResponsesStreamOptions::default(),
        0,
    );
    let reasoning = replayed_reasoning(&model, &outcome.message);
    assert_eq!(reasoning["id"], json!("rs_missing"));
    assert_eq!(
        reasoning["encrypted_content"],
        json!("from-response-completed")
    );
}
