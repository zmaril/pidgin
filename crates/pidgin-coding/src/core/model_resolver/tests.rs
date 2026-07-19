//! Tests for [`super`] — the model-resolver pure slice.
//!
//! Split into its own file to keep `model_resolver.rs` under the file-size
//! budget, mirroring the `api/anthropic/tests.rs` layout in `pidgin-ai`. Every
//! case here is a translation of `test/model-resolver.test.ts`, plus a few
//! helper-level checks the pi suite exercises only indirectly.

use super::*;
use crate::core::test_model;

// --- fixtures -----------------------------------------------------------

/// Build a test [`Model`] with the fields the resolver reads; the rest take
/// sensible defaults. Mirrors the mock objects in pi's test file.
fn model(id: &str, provider: &str, reasoning: bool) -> Model {
    let mut m = test_model(id, provider);
    m.reasoning = reasoning;
    m
}

/// A named model, for readability where `name` is asserted or matched.
fn named_model(id: &str, name: &str, provider: &str) -> Model {
    let mut m = model(id, provider, false);
    m.name = name.to_string();
    m
}

fn base_models() -> Vec<Model> {
    vec![
        named_model("claude-sonnet-4-5", "Claude Sonnet 4.5", "anthropic"),
        named_model("gpt-4o", "GPT-4o", "openai"),
        named_model(
            "qwen/qwen3-coder:exacto",
            "Qwen3 Coder Exacto",
            "openrouter",
        ),
        named_model("openai/gpt-4o:extended", "GPT-4o Extended", "openrouter"),
    ]
}

/// A configurable fixture standing in for pi's duck-typed runtime objects.
#[derive(Default)]
struct FakeRuntime {
    all: Vec<Model>,
    available: Vec<Model>,
    lookup: Vec<Model>,
    authed_providers: AuthMode,
}

#[derive(Default)]
enum AuthMode {
    #[default]
    None,
    All,
    Only(Vec<String>),
}

impl FakeRuntime {
    /// Runtime whose `getModels()` returns `all` (and `getModel` looks in it).
    fn with_models(all: Vec<Model>) -> Self {
        FakeRuntime {
            lookup: all.clone(),
            all,
            ..Self::default()
        }
    }
    /// Runtime whose `getAvailable()` returns `available`.
    fn with_available(available: Vec<Model>) -> Self {
        FakeRuntime {
            lookup: available.clone(),
            available,
            ..Self::default()
        }
    }
    fn auth(mut self, mode: AuthMode) -> Self {
        self.authed_providers = mode;
        self
    }
    fn lookup(mut self, lookup: Vec<Model>) -> Self {
        self.lookup = lookup;
        self
    }
}

impl ModelRuntimeView for FakeRuntime {
    fn get_models(&self) -> Vec<Model> {
        self.all.clone()
    }
    fn get_available(&self) -> Vec<Model> {
        self.available.clone()
    }
    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.lookup
            .iter()
            .find(|m| m.provider == provider && m.id == model_id)
            .cloned()
    }
    fn has_configured_auth(&self, provider: &str) -> bool {
        match &self.authed_providers {
            AuthMode::All => true,
            AuthMode::None => false,
            AuthMode::Only(ps) => ps.iter().any(|p| p == provider),
        }
    }
}

// --- shared assertion helpers -------------------------------------------

/// Assert a parse produced `expect_id`/`expect_level` with no warning.
fn assert_parse(pattern: &str, expect_id: Option<&str>, expect_level: Option<ModelThinkingLevel>) {
    let r = parse_model_pattern(pattern, &base_models());
    assert_eq!(
        r.model.as_ref().map(|m| m.id.as_str()),
        expect_id,
        "id for {pattern:?}"
    );
    assert_eq!(r.thinking_level, expect_level, "thinking for {pattern:?}");
    assert_eq!(r.warning, None, "warning for {pattern:?}");
}

/// Run [`resolve_cli_model`] against a fixture and return the result.
fn cli(models: Vec<Model>, provider: Option<&str>, model_ref: &str) -> ResolveCliModelResult {
    let rt = FakeRuntime::with_models(models);
    resolve_cli_model(
        ResolveCliModelOptions {
            cli_provider: provider,
            cli_model: Some(model_ref),
            cli_thinking: None,
        },
        &rt,
    )
}

/// Assert a CLI resolution matched `provider`/`id` with no error.
fn assert_cli(result: &ResolveCliModelResult, provider: &str, id: &str) {
    assert_eq!(result.error, None);
    assert_eq!(
        result.model.as_ref().map(|m| m.provider.as_str()),
        Some(provider)
    );
    assert_eq!(result.model.as_ref().map(|m| m.id.as_str()), Some(id));
}

const ALL_LEVELS: [(&str, ModelThinkingLevel); 7] = [
    ("off", ModelThinkingLevel::Off),
    ("minimal", ModelThinkingLevel::Minimal),
    ("low", ModelThinkingLevel::Low),
    ("medium", ModelThinkingLevel::Medium),
    ("high", ModelThinkingLevel::High),
    ("xhigh", ModelThinkingLevel::Xhigh),
    ("max", ModelThinkingLevel::Max),
];

// --- parseModelPattern: simple patterns ---------------------------------

#[test]
fn parse_exact_match() {
    assert_parse("claude-sonnet-4-5", Some("claude-sonnet-4-5"), None);
}

#[test]
fn parse_partial_match() {
    assert_parse("sonnet", Some("claude-sonnet-4-5"), None);
}

#[test]
fn parse_no_match() {
    assert_parse("nonexistent", None, None);
}

// --- parseModelPattern: valid thinking levels ---------------------------

#[test]
fn parse_valid_thinking_levels() {
    assert_parse(
        "sonnet:high",
        Some("claude-sonnet-4-5"),
        Some(ModelThinkingLevel::High),
    );
    assert_parse(
        "gpt-4o:medium",
        Some("gpt-4o"),
        Some(ModelThinkingLevel::Medium),
    );
    for (name, level) in ALL_LEVELS {
        assert_parse(
            &format!("sonnet:{name}"),
            Some("claude-sonnet-4-5"),
            Some(level),
        );
    }
}

// --- parseModelPattern: invalid thinking levels -------------------------

#[test]
fn parse_invalid_thinking_level_warns() {
    let r = parse_model_pattern("sonnet:random", &base_models());
    assert_eq!(r.model.map(|m| m.id), Some("claude-sonnet-4-5".to_string()));
    assert_eq!(r.thinking_level, None);
    let w = r.warning.unwrap();
    assert!(w.contains("Invalid thinking level"));
    assert!(w.contains("random"));

    let r2 = parse_model_pattern("gpt-4o:invalid", &base_models());
    assert_eq!(r2.model.map(|m| m.id), Some("gpt-4o".to_string()));
    assert_eq!(r2.thinking_level, None);
    assert!(r2.warning.unwrap().contains("Invalid thinking level"));
}

// --- parseModelPattern: OpenRouter colon ids ----------------------------

#[test]
fn parse_openrouter_colon_ids() {
    assert_parse(
        "qwen/qwen3-coder:exacto",
        Some("qwen/qwen3-coder:exacto"),
        None,
    );
    assert_parse(
        "qwen/qwen3-coder:exacto:high",
        Some("qwen/qwen3-coder:exacto"),
        Some(ModelThinkingLevel::High),
    );
    assert_parse(
        "openai/gpt-4o:extended",
        Some("openai/gpt-4o:extended"),
        None,
    );
}

#[test]
fn parse_openrouter_with_provider_prefix() {
    let r = parse_model_pattern("openrouter/qwen/qwen3-coder:exacto", &base_models());
    assert_eq!(
        r.model.as_ref().map(|m| m.id.as_str()),
        Some("qwen/qwen3-coder:exacto")
    );
    assert_eq!(
        r.model.as_ref().map(|m| m.provider.as_str()),
        Some("openrouter")
    );
    assert_eq!(r.thinking_level, None);
    assert_eq!(r.warning, None);

    let r2 = parse_model_pattern("openrouter/qwen/qwen3-coder:exacto:high", &base_models());
    assert_eq!(
        r2.model.as_ref().map(|m| m.id.as_str()),
        Some("qwen/qwen3-coder:exacto")
    );
    assert_eq!(
        r2.model.as_ref().map(|m| m.provider.as_str()),
        Some("openrouter")
    );
    assert_eq!(r2.thinking_level, Some(ModelThinkingLevel::High));
}

#[test]
fn parse_openrouter_invalid_suffix_warns() {
    for pattern in [
        "qwen/qwen3-coder:exacto:random",
        "qwen/qwen3-coder:exacto:high:random",
    ] {
        let r = parse_model_pattern(pattern, &base_models());
        assert_eq!(
            r.model.as_ref().map(|m| m.id.as_str()),
            Some("qwen/qwen3-coder:exacto")
        );
        assert_eq!(r.thinking_level, None);
        let w = r.warning.unwrap();
        assert!(w.contains("Invalid thinking level"));
        assert!(w.contains("random"));
    }
}

// --- parseModelPattern: edge cases --------------------------------------

#[test]
fn parse_empty_pattern_matches_via_partial() {
    let r = parse_model_pattern("", &base_models());
    assert!(r.model.is_some());
    assert_eq!(r.thinking_level, None);
}

#[test]
fn parse_trailing_colon_treats_empty_suffix_as_invalid() {
    let r = parse_model_pattern("sonnet:", &base_models());
    assert_eq!(r.model.map(|m| m.id), Some("claude-sonnet-4-5".to_string()));
    assert!(r.warning.unwrap().contains("Invalid thinking level"));
}

// --- resolveModelScopeWithDiagnostics -----------------------------------

#[test]
fn scope_returns_models_and_structured_diagnostics() {
    let rt = FakeRuntime::with_available(base_models());
    let patterns = ["sonnet:high", "gpt-4o:invalid", "missing"]
        .map(String::from)
        .to_vec();
    let result = resolve_model_scope_with_diagnostics(&patterns, &rt);

    let ids: Vec<&str> = result
        .scoped_models
        .iter()
        .map(|s| s.model.id.as_str())
        .collect();
    assert_eq!(ids, ["claude-sonnet-4-5", "gpt-4o"]);
    assert_eq!(
        result.scoped_models[0].thinking_level,
        Some(ModelThinkingLevel::High)
    );
    assert_eq!(result.scoped_models[1].thinking_level, None);
    assert_eq!(
            result.diagnostics,
            vec![
                ModelScopeDiagnostic {
                    kind: DiagnosticKind::Warning,
                    message:
                        "Invalid thinking level \"invalid\" in pattern \"gpt-4o:invalid\". Using default instead."
                            .to_string(),
                    pattern: "gpt-4o:invalid".to_string(),
                },
                ModelScopeDiagnostic {
                    kind: DiagnosticKind::Warning,
                    message: "No models match pattern \"missing\"".to_string(),
                    pattern: "missing".to_string(),
                },
            ]
        );
}

#[test]
fn scope_drops_console_warnings_but_keeps_diagnostics() {
    // pi's resolveModelScope writes to console.warn; the pure port returns
    // only scoped models, with the warning surfaced via the diagnostics API.
    let rt = FakeRuntime::with_available(base_models());
    let patterns = vec!["missing".to_string()];
    assert_eq!(resolve_model_scope(&patterns, &rt), vec![]);
    let diags = resolve_model_scope_with_diagnostics(&patterns, &rt).diagnostics;
    assert_eq!(diags.len(), 1);
    assert!(diags[0]
        .message
        .contains("No models match pattern \"missing\""));
}

#[test]
fn scope_glob_matches_and_dedupes() {
    let rt = FakeRuntime::with_available(base_models());
    // Bare-id glob against `claude-sonnet-4-5`.
    let sonnet = resolve_model_scope(&["*sonnet*".to_string()], &rt);
    assert_eq!(
        sonnet
            .iter()
            .map(|s| s.model.id.as_str())
            .collect::<Vec<_>>(),
        ["claude-sonnet-4-5"]
    );
    // Provider glob with a thinking suffix applies the level to every match.
    let anthropic = resolve_model_scope(&["anthropic/*:high".to_string()], &rt);
    assert_eq!(anthropic.len(), 1);
    assert_eq!(anthropic[0].thinking_level, Some(ModelThinkingLevel::High));
}

// --- resolveCliModel ----------------------------------------------------

#[test]
fn cli_resolves_provider_slash_id_without_provider() {
    assert_cli(
        &cli(base_models(), None, "openai/gpt-4o"),
        "openai",
        "gpt-4o",
    );
}

#[test]
fn cli_resolves_fuzzy_within_explicit_provider() {
    assert_cli(
        &cli(base_models(), Some("openai"), "4o"),
        "openai",
        "gpt-4o",
    );
}

#[test]
fn cli_supports_pattern_thinking_suffix() {
    let r = cli(base_models(), None, "sonnet:high");
    assert_cli(&r, "anthropic", "claude-sonnet-4-5");
    assert_eq!(r.thinking_level, Some(ModelThinkingLevel::High));
}

#[test]
fn cli_prefers_exact_id_over_provider_inference() {
    assert_cli(
        &cli(base_models(), None, "openai/gpt-4o:extended"),
        "openrouter",
        "openai/gpt-4o:extended",
    );
}

#[test]
fn cli_does_not_strip_invalid_suffix_as_thinking() {
    assert_cli(
        &cli(base_models(), Some("openai"), "gpt-4o:extended"),
        "openai",
        "gpt-4o:extended",
    );
}

#[test]
fn cli_allows_custom_ids_without_double_prefixing() {
    assert_cli(
        &cli(
            base_models(),
            Some("openrouter"),
            "openrouter/openai/ghost-model",
        ),
        "openrouter",
        "openai/ghost-model",
    );
}

#[test]
fn cli_errors_when_no_models() {
    let r = cli(vec![], Some("openai"), "gpt-4o");
    assert!(r.model.is_none());
    assert!(r.error.unwrap().contains("No models available"));
}

#[test]
fn cli_prefers_provider_split_over_gateway_id() {
    let mut models = base_models();
    models.push(model("glm-5", "zai", true));
    models.push(model("zai/glm-5", "vercel-ai-gateway", true));
    let rt = FakeRuntime::with_models(models).auth(AuthMode::All);
    let r = resolve_cli_model(
        ResolveCliModelOptions {
            cli_model: Some("zai/glm-5"),
            ..Default::default()
        },
        &rt,
    );
    assert_cli(&r, "zai", "glm-5");
}

#[test]
fn cli_prefers_authed_raw_id_over_unauthed_inferred_provider() {
    let mut models = base_models();
    models.push(model("xiaomi/mimo-v2.5-pro", "commandcode", false));
    models.push(model("mimo-v2.5-pro", "xiaomi", false));
    let rt = FakeRuntime::with_models(models).auth(AuthMode::Only(vec!["commandcode".to_string()]));
    let r = resolve_cli_model(
        ResolveCliModelOptions {
            cli_model: Some("xiaomi/mimo-v2.5-pro"),
            ..Default::default()
        },
        &rt,
    );
    assert_cli(&r, "commandcode", "xiaomi/mimo-v2.5-pro");
}

#[test]
fn cli_resolves_provider_prefixed_fuzzy() {
    assert_cli(
        &cli(base_models(), None, "openrouter/qwen"),
        "openrouter",
        "qwen/qwen3-coder:exacto",
    );
}

// --- resolveCliModel: custom-model fallback with :thinking (#5552) -------

fn models_with_neuralwatt() -> Vec<Model> {
    let mut models = base_models();
    models.push(model("some-base-model", "neuralwatt", false));
    models
}

#[test]
fn cli_fallback_strips_thinking_suffix() {
    let r = cli(
        models_with_neuralwatt(),
        None,
        "neuralwatt/zai-org/GLM-5.1-FP8:high",
    );
    assert_cli(&r, "neuralwatt", "zai-org/GLM-5.1-FP8");
    assert!(r.model.unwrap().reasoning);
    assert_eq!(r.thinking_level, Some(ModelThinkingLevel::High));
}

#[test]
fn cli_fallback_without_thinking_suffix() {
    let r = cli(
        models_with_neuralwatt(),
        None,
        "neuralwatt/zai-org/GLM-5.1-FP8",
    );
    assert_cli(&r, "neuralwatt", "zai-org/GLM-5.1-FP8");
    assert_eq!(r.thinking_level, None);
}

#[test]
fn cli_fallback_all_thinking_levels() {
    for (name, level) in ALL_LEVELS {
        let r = cli(
            models_with_neuralwatt(),
            None,
            &format!("neuralwatt/zai-org/GLM-5.1-FP8:{name}"),
        );
        assert_eq!(r.error, None);
        assert_eq!(
            r.model.as_ref().map(|m| m.id.as_str()),
            Some("zai-org/GLM-5.1-FP8")
        );
        assert_eq!(r.thinking_level, Some(level));
    }
}

#[test]
fn cli_fallback_invalid_suffix_stays_in_id() {
    let r = cli(
        models_with_neuralwatt(),
        None,
        "neuralwatt/zai-org/GLM-5.1-FP8:banana",
    );
    assert_cli(&r, "neuralwatt", "zai-org/GLM-5.1-FP8:banana");
    assert_eq!(r.thinking_level, None);
}

#[test]
fn cli_fallback_explicit_provider_strips_suffix() {
    let r = cli(
        models_with_neuralwatt(),
        Some("neuralwatt"),
        "zai-org/GLM-5.1-FP8:high",
    );
    assert_cli(&r, "neuralwatt", "zai-org/GLM-5.1-FP8");
    assert_eq!(r.thinking_level, Some(ModelThinkingLevel::High));
}

#[test]
fn cli_fallback_explicit_thinking_keeps_suffix_in_id() {
    let rt = FakeRuntime::with_models(models_with_neuralwatt());
    let r = resolve_cli_model(
        ResolveCliModelOptions {
            cli_model: Some("neuralwatt/zai-org/GLM-5.1-FP8:high"),
            cli_thinking: Some(ModelThinkingLevel::Medium),
            ..Default::default()
        },
        &rt,
    );
    assert_cli(&r, "neuralwatt", "zai-org/GLM-5.1-FP8:high");
    assert_eq!(r.thinking_level, None);
}

// --- default model selection --------------------------------------------

#[test]
fn default_model_map_values() {
    let get = |p: &str| default_model_for_provider(p);
    assert_eq!(get("openai"), Some("gpt-5.5"));
    assert_eq!(get("openai-codex"), Some("gpt-5.5"));
    assert_eq!(get("zai"), Some("glm-5.1"));
    assert_eq!(get("minimax"), Some("MiniMax-M2.7"));
    assert_eq!(get("minimax-cn"), Some("MiniMax-M2.7"));
    assert_eq!(get("cerebras"), Some("zai-glm-4.7"));
    assert_eq!(get("ant-ling"), Some("Ring-2.6-1T"));
    assert_eq!(get("vercel-ai-gateway"), Some("zai/glm-5.1"));
}

#[test]
fn find_initial_accepts_explicit_provider_custom_id() {
    let rt = FakeRuntime::with_models(base_models());
    let result = find_initial_model(
        FindInitialModelOptions {
            cli_provider: Some("openrouter"),
            cli_model: Some("openrouter/openai/ghost-model"),
            ..Default::default()
        },
        &rt,
    )
    .unwrap();
    assert_eq!(
        result.model.as_ref().map(|m| m.provider.as_str()),
        Some("openrouter")
    );
    assert_eq!(
        result.model.as_ref().map(|m| m.id.as_str()),
        Some("openai/ghost-model")
    );
}

#[test]
fn find_initial_selects_ai_gateway_default() {
    let ai_gateway = model("anthropic/claude-opus-4-6", "vercel-ai-gateway", true);
    // vercel-ai-gateway's default id is "zai/glm-5.1", not this one, so this
    // exercises the "no known default -> first available" path.
    let rt = FakeRuntime::with_available(vec![ai_gateway]);
    let result = find_initial_model(
        FindInitialModelOptions {
            is_continuing: false,
            ..Default::default()
        },
        &rt,
    )
    .unwrap();
    assert_eq!(
        result.model.as_ref().map(|m| m.provider.as_str()),
        Some("vercel-ai-gateway")
    );
    assert_eq!(
        result.model.as_ref().map(|m| m.id.as_str()),
        Some("anthropic/claude-opus-4-6")
    );
}

#[test]
fn find_initial_ignores_unauthenticated_saved_default() {
    let saved = model("deepseek-v4-flash", "deepseek", true);
    let mut local = saved.clone();
    local.provider = "spark-two".to_string();
    local.base_url = "http://spark-two:8000/v1".to_string();
    let rt = FakeRuntime::with_available(vec![local])
        .lookup(vec![saved])
        .auth(AuthMode::Only(vec!["spark-two".to_string()]));
    let result = find_initial_model(
        FindInitialModelOptions {
            default_provider: Some("deepseek"),
            default_model_id: Some("deepseek-v4-flash"),
            ..Default::default()
        },
        &rt,
    )
    .unwrap();
    assert_eq!(
        result.model.as_ref().map(|m| m.provider.as_str()),
        Some("spark-two")
    );
    assert_eq!(
        result.model.as_ref().map(|m| m.id.as_str()),
        Some("deepseek-v4-flash")
    );
}

// --- helpers not covered by translated pi tests -------------------------

#[test]
fn is_alias_rules() {
    assert!(is_alias("claude-sonnet-4-5"));
    assert!(is_alias("claude-3-5-sonnet-latest"));
    assert!(!is_alias("claude-3-5-sonnet-20241022"));
    assert!(is_alias("model-1234")); // 4 digits is not a date
}

#[test]
fn restore_from_session_falls_back_to_current() {
    let rt = FakeRuntime::default();
    let current = model("gpt-4o", "openai", false);
    let r = restore_model_from_session("deepseek", "gone", Some(current), &rt);
    assert_eq!(r.model.as_ref().map(|m| m.id.as_str()), Some("gpt-4o"));
    assert!(r
        .fallback_message
        .unwrap()
        .contains("model no longer exists"));
}

#[test]
fn restore_from_session_returns_restored_when_authed() {
    let saved = model("gpt-4o", "openai", false);
    let rt = FakeRuntime::with_models(vec![saved]).auth(AuthMode::All);
    let r = restore_model_from_session("openai", "gpt-4o", None, &rt);
    assert_eq!(r.model.as_ref().map(|m| m.id.as_str()), Some("gpt-4o"));
    assert_eq!(r.fallback_message, None);
}
