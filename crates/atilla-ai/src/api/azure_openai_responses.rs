//! Azure OpenAI **Responses API** request shaping, ported from pi-ai's
//! `packages/ai/src/api/azure-openai-responses.ts` at pinned commit `3da591ab`.
//!
//! Azure is a thin wrapper over the shared Responses core
//! ([`crate::api::openai_responses_shared`]): the only Azure-specific request
//! shaping is base-URL/deployment-name resolution, the `v1` default API version,
//! the min-output clamp, and the `AZURE_TOOL_CALL_PROVIDERS` set. The stream
//! processor, message conversion, and tool conversion are all reused unchanged.
//!
//! Two pieces of pi's `stream()` are intentionally out of scope at this pure
//! boundary (matching how the OpenAI module drops `PI_CACHE_RETENTION` env
//! plumbing): the `AZURE_OPENAI_*` environment-variable fallbacks and the Azure
//! SDK client construction. [`resolve_azure_config`] and
//! [`resolve_deployment_name`] therefore take their inputs explicitly.

use serde_json::{json, Map, Value};

use crate::api::openai_responses::OpenAIResponsesModel;
use crate::api::openai_responses_shared::{convert_responses_messages, convert_responses_tools};
use crate::types::{Context, ModelThinkingLevel};

/// pi's `AZURE_TOOL_CALL_PROVIDERS` (`azure-openai-responses.ts:20`).
pub const AZURE_TOOL_CALL_PROVIDERS: [&str; 4] = [
    "openai",
    "openai-codex",
    "opencode",
    "azure-openai-responses",
];

/// pi's `DEFAULT_AZURE_API_VERSION` (`azure-openai-responses.ts:19`).
pub const DEFAULT_AZURE_API_VERSION: &str = "v1";

/// OpenAI Responses rejects `max_output_tokens` below 16.
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// Azure-specific request options. As with the OpenAI module, this is the subset
/// read at this boundary; env fallbacks are resolved by the caller.
#[derive(Debug, Clone, Default)]
pub struct AzureOpenAIResponsesOptions {
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub session_id: Option<String>,
    pub azure_api_version: Option<String>,
    pub azure_base_url: Option<String>,
    pub azure_resource_name: Option<String>,
    pub azure_deployment_name: Option<String>,
    /// Explicit `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` contents (`modelId=deployment,...`).
    pub deployment_name_map: Option<String>,
}

fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        Some(key.to_string())
    } else {
        Some(
            key.chars()
                .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
                .collect(),
        )
    }
}

/// Parse the `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` string (`modelId=deployment,...`)
/// into pairs (pi's `parseDeploymentNameMap`).
fn parse_deployment_name_map(value: Option<&str>) -> Vec<(String, String)> {
    let mut map = Vec::new();
    let Some(value) = value else {
        return map;
    };
    for entry in value.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((model_id, deployment)) = trimmed.split_once('=') {
            let model_id = model_id.trim();
            let deployment = deployment.trim();
            if model_id.is_empty() || deployment.is_empty() {
                continue;
            }
            map.push((model_id.to_string(), deployment.to_string()));
        }
    }
    map
}

/// Resolve the Azure deployment name (pi's `resolveDeploymentName`): an explicit
/// deployment name wins, then a mapping entry for `model.id`, else `model.id`.
pub fn resolve_deployment_name(
    model: &OpenAIResponsesModel,
    options: &AzureOpenAIResponsesOptions,
) -> String {
    if let Some(name) = &options.azure_deployment_name {
        return name.clone();
    }
    let map = parse_deployment_name_map(options.deployment_name_map.as_deref());
    map.into_iter()
        .find(|(id, _)| id == &model.id)
        .map(|(_, deployment)| deployment)
        .unwrap_or_else(|| model.id.clone())
}

/// The resolved Azure client configuration (pi's `resolveAzureConfig`), sans env
/// fallbacks.
#[derive(Debug, Clone, PartialEq)]
pub struct AzureConfig {
    pub base_url: String,
    pub api_version: String,
}

/// Resolve the Azure base URL and API version (pi's `resolveAzureConfig`). Base
/// URL priority: explicit `azure_base_url`, then `azure_resource_name`, then
/// `model.base_url`.
pub fn resolve_azure_config(
    model: &OpenAIResponsesModel,
    options: &AzureOpenAIResponsesOptions,
) -> Result<AzureConfig, String> {
    let api_version = options
        .azure_api_version
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());

    let mut resolved: Option<String> = options
        .azure_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if resolved.is_none() {
        if let Some(resource) = options
            .azure_resource_name
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            resolved = Some(build_default_base_url(resource));
        }
    }
    if resolved.is_none() && !model.base_url.is_empty() {
        resolved = Some(model.base_url.clone());
    }

    let Some(resolved) = resolved else {
        return Err("Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.".to_string());
    };

    Ok(AzureConfig {
        base_url: normalize_azure_base_url(&resolved)?,
        api_version,
    })
}

fn build_default_base_url(resource_name: &str) -> String {
    format!("https://{resource_name}.openai.azure.com/openai/v1")
}

/// Normalize an Azure base URL (pi's `normalizeAzureBaseUrl`): Azure hosts whose
/// path is empty / `/openai` / `/openai/v1/responses` are rewritten to
/// `/openai/v1` (query stripped) so the SDK can append
/// `/deployments/<model>/...`; everything else is preserved. Returns an error on
/// an unparseable URL.
pub fn normalize_azure_base_url(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return Err(format!("Invalid Azure OpenAI base URL: {base_url}"));
    };
    if scheme.is_empty() || rest.is_empty() {
        return Err(format!("Invalid Azure OpenAI base URL: {base_url}"));
    }

    // Authority runs up to the first '/' or '?'.
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[auth_end..];
    let host_port = &rest[..auth_end];
    if host_port.is_empty() {
        return Err(format!("Invalid Azure OpenAI base URL: {base_url}"));
    }

    let (path, query) = match authority.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (authority, None),
    };

    // Hostname: strip any userinfo and port for the Azure-host suffix check.
    let host_no_userinfo = host_port.rsplit('@').next().unwrap_or(host_port);
    let hostname = host_no_userinfo
        .split(':')
        .next()
        .unwrap_or(host_no_userinfo);

    let is_azure_host = hostname.ends_with(".openai.azure.com")
        || hostname.ends_with(".cognitiveservices.azure.com")
        || hostname.ends_with(".ai.azure.com");
    let normalized_path = path.trim_end_matches('/');

    if is_azure_host
        && (normalized_path.is_empty()
            || normalized_path == "/openai"
            || normalized_path == "/openai/v1/responses")
    {
        return Ok(format!("{scheme}://{host_port}/openai/v1"));
    }

    let mut out = format!("{scheme}://{host_port}{path}");
    if let Some(query) = query {
        out.push('?');
        out.push_str(query);
    }
    Ok(out.trim_end_matches('/').to_string())
}

/// Build the Azure Responses `ResponseCreateParams` payload (pi's `buildParams`).
pub fn build_params(
    model: &OpenAIResponsesModel,
    context: &Context,
    options: &AzureOpenAIResponsesOptions,
    deployment_name: &str,
) -> Value {
    let messages = convert_responses_messages(
        model,
        &context.messages,
        context.system_prompt.as_deref(),
        &AZURE_TOOL_CALL_PROVIDERS,
        true,
    );

    let mut params = Map::new();
    params.insert(
        "model".to_string(),
        Value::String(deployment_name.to_string()),
    );
    params.insert("input".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), Value::Bool(true));
    if let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref()) {
        params.insert("prompt_cache_key".to_string(), Value::String(key));
    }
    params.insert("store".to_string(), Value::Bool(false));

    if let Some(max_tokens) = options.max_tokens {
        params.insert(
            "max_output_tokens".to_string(),
            Value::from(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(temperature) = options.temperature {
        params.insert("temperature".to_string(), json!(temperature));
    }

    let tools = context.tools.clone().unwrap_or_default();
    if !tools.is_empty() {
        params.insert(
            "tools".to_string(),
            Value::Array(convert_responses_tools(&tools, false, false)),
        );
    }

    if model.reasoning {
        if options.reasoning_effort.is_some() || options.reasoning_summary.is_some() {
            let effort = match &options.reasoning_effort {
                Some(effort) => {
                    thinking_level_lookup(model, effort).unwrap_or_else(|| effort.clone())
                }
                None => "medium".to_string(),
            };
            let summary = options
                .reasoning_summary
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "auto".to_string());
            params.insert(
                "reasoning".to_string(),
                json!({ "effort": effort, "summary": summary }),
            );
            params.insert(
                "include".to_string(),
                json!(["reasoning.encrypted_content"]),
            );
        } else if !thinking_off_is_null(model) {
            let effort = thinking_off_value(model).unwrap_or_else(|| "none".to_string());
            params.insert("reasoning".to_string(), json!({ "effort": effort }));
        }
    }

    Value::Object(params)
}

fn thinking_level_lookup(model: &OpenAIResponsesModel, effort: &str) -> Option<String> {
    let level = parse_thinking_level(effort)?;
    model
        .thinking_level_map
        .as_ref()?
        .get(&level)
        .cloned()
        .flatten()
}

fn parse_thinking_level(effort: &str) -> Option<ModelThinkingLevel> {
    match effort {
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        "off" => Some(ModelThinkingLevel::Off),
        _ => None,
    }
}

fn thinking_off_is_null(model: &OpenAIResponsesModel) -> bool {
    matches!(
        model
            .thinking_level_map
            .as_ref()
            .map(|m| m.get(&ModelThinkingLevel::Off)),
        Some(Some(None))
    )
}

fn thinking_off_value(model: &OpenAIResponsesModel) -> Option<String> {
    model
        .thinking_level_map
        .as_ref()?
        .get(&ModelThinkingLevel::Off)
        .cloned()
        .flatten()
}

#[cfg(test)]
mod tests;
