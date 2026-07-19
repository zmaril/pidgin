//! Model resolution, scoping, and initial selection.
//!
//! Ported from pi's `core/model-resolver.ts`. This is the pure slice: pattern
//! parsing, exact/fuzzy/glob matching, alias-vs-dated preference, colon
//! precedence for thinking-level suffixes, and initial-model selection. The
//! only collaborator is a small [`ModelRuntimeView`] read seam standing in for
//! pi's stateful `ModelRuntime` (a type-only import upstream); tests supply a
//! fixture implementation exactly as pi's tests pass duck-typed `{ getModels }`
//! objects.
//!
//! The model type is standardized on `pidgin_ai::types::Model<serde_json::Value>`
//! (the crate's default compat view). pi's `ThinkingLevel` (`off | minimal |
//! low | medium | high | xhigh | max`) maps onto
//! [`pidgin_ai::types::ModelThinkingLevel`], which carries the same seven
//! variants including `off`.

use pidgin_ai::types::{Model, ModelThinkingLevel};

/// Read-only view of the model runtime the resolver consults.
///
/// This abstracts pi's `ModelRuntime` (imported type-only in the source) behind
/// the four methods the pure resolver actually calls. Sibling threads own the
/// stateful runtime/registry/composer; this seam lets the pure logic land now.
///
// NOTE: pi's `ModelRuntime.getAvailable()` is async. The pure resolver has no
// I/O, so this seam is synchronous; a future async runtime can wrap this or the
// callers can bridge at the edge.
pub trait ModelRuntimeView {
    /// All configured models, regardless of auth. Mirrors `getModels()`.
    fn get_models(&self) -> Vec<Model>;
    /// Models whose provider currently has usable auth. Mirrors `getAvailable()`.
    fn get_available(&self) -> Vec<Model>;
    /// Look up a model by exact provider + id. Mirrors `getModel()`.
    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model>;
    /// Whether the given provider has auth configured. Mirrors `hasConfiguredAuth()`.
    fn has_configured_auth(&self, provider: &str) -> bool;
}

/// Default model IDs for each known provider (`model-resolver.ts:14`).
///
/// Copied verbatim from pi; the resolver tests assert individual values. Kept as
/// an ordered slice because [`find_initial_model`] iterates it in declaration
/// order when picking a default among available models.
pub const DEFAULT_MODEL_PER_PROVIDER: &[(&str, &str)] = &[
    ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
    ("ant-ling", "Ring-2.6-1T"),
    ("anthropic", "claude-opus-4-8"),
    ("openai", "gpt-5.5"),
    ("azure-openai-responses", "gpt-5.4"),
    ("openai-codex", "gpt-5.5"),
    ("radius", "auto"),
    ("nvidia", "nvidia/nemotron-3-super-120b-a12b"),
    ("deepseek", "deepseek-v4-pro"),
    ("google", "gemini-3.1-pro-preview"),
    ("google-vertex", "gemini-3.1-pro-preview"),
    ("github-copilot", "gpt-5.4"),
    ("openrouter", "moonshotai/kimi-k2.6"),
    ("vercel-ai-gateway", "zai/glm-5.1"),
    ("xai", "grok-4.5"),
    ("groq", "openai/gpt-oss-120b"),
    ("cerebras", "zai-glm-4.7"),
    ("zai", "glm-5.1"),
    ("zai-coding-cn", "glm-5.1"),
    ("mistral", "devstral-medium-latest"),
    ("minimax", "MiniMax-M2.7"),
    ("minimax-cn", "MiniMax-M2.7"),
    ("moonshotai", "kimi-k2.6"),
    ("moonshotai-cn", "kimi-k2.6"),
    ("huggingface", "moonshotai/Kimi-K2.6"),
    ("fireworks", "accounts/fireworks/models/kimi-k2p6"),
    ("together", "moonshotai/Kimi-K2.6"),
    ("opencode", "kimi-k2.6"),
    ("opencode-go", "kimi-k2.6"),
    ("kimi-coding", "kimi-for-coding"),
    ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
    (
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    ),
    ("xiaomi", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-cn", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-ams", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-sgp", "mimo-v2.5-pro"),
];

/// The default thinking level when none is specified (`defaults.ts:3`).
pub const DEFAULT_THINKING_LEVEL: ModelThinkingLevel = ModelThinkingLevel::Medium;

/// Look up the default model id for a provider in [`DEFAULT_MODEL_PER_PROVIDER`].
fn default_model_for_provider(provider: &str) -> Option<&'static str> {
    DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, id)| *id)
}

/// Parse a thinking-level string, mirroring pi's `isValidThinkingLevel`.
///
/// Delegates to `ModelThinkingLevel`'s `serde(rename_all = "lowercase")`
/// deserialization so the accepted spellings (`off | minimal | low | medium |
/// high | xhigh | max`) stay in lockstep with the enum, rather than
/// transcribing the same table that `pidgin-ai`'s provider `builtins.rs`
/// already owns in a sibling crate.
fn parse_thinking_level(level: &str) -> Option<ModelThinkingLevel> {
    serde_json::from_value(serde_json::Value::String(level.to_owned())).ok()
}

/// Whether two models refer to the same catalog entry (`models.ts:699`).
fn models_are_equal(a: &Model, b: &Model) -> bool {
    a.id == b.id && a.provider == b.provider
}

/// Find a model whose bare id or canonical `provider/id` equals `cli_model`
/// (case-insensitive). Shared by the two raw-id fallback paths in
/// [`resolve_cli_model`].
fn find_raw_exact_match(available_models: &[Model], cli_model: &str) -> Option<Model> {
    let lower = cli_model.to_lowercase();
    available_models
        .iter()
        .find(|m| {
            m.id.to_lowercase() == lower
                || format!("{}/{}", m.provider, m.id).to_lowercase() == lower
        })
        .cloned()
}

/// A [`ResolveCliModelResult`] carrying only a resolved model.
fn cli_model_ok(model: Model) -> ResolveCliModelResult {
    ResolveCliModelResult {
        model: Some(model),
        thinking_level: None,
        warning: None,
        error: None,
    }
}

/// A model paired with an optional explicit thinking level (`model-resolver.ts:53`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedModel {
    /// The resolved model.
    pub model: Model,
    /// Thinking level if explicitly specified in the pattern (e.g. `model:high`).
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// Whether a model id looks like an alias rather than a dated version
/// (`model-resolver.ts:63`).
///
/// An id is an alias if it ends with `-latest`, or does not end with a
/// `-YYYYMMDD` date suffix.
fn is_alias(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }
    // Dated ids end with `-` followed by exactly 8 ASCII digits.
    let dated = id.rfind('-').is_some_and(|dash| {
        let digits = &id[dash + 1..];
        digits.len() == 8 && digits.bytes().all(|b| b.is_ascii_digit())
    });
    !dated
}

/// Find an exact model reference match (`model-resolver.ts:77`).
///
/// Accepts a bare model id or a canonical `provider/modelId` reference. Bare-id
/// matches that are ambiguous across providers are rejected (return `None`).
pub fn find_exact_model_reference_match(
    model_reference: &str,
    available_models: &[Model],
) -> Option<Model> {
    let trimmed = model_reference.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_lowercase();

    let canonical: Vec<&Model> = available_models
        .iter()
        .filter(|m| format!("{}/{}", m.provider, m.id).to_lowercase() == normalized)
        .collect();
    match canonical.len() {
        1 => return Some(canonical[0].clone()),
        n if n > 1 => return None,
        _ => {}
    }

    if let Some(slash) = trimmed.find('/') {
        let provider = trimmed[..slash].trim();
        let model_id = trimmed[slash + 1..].trim();
        if !provider.is_empty() && !model_id.is_empty() {
            let provider_lc = provider.to_lowercase();
            let model_id_lc = model_id.to_lowercase();
            let provider_matches: Vec<&Model> = available_models
                .iter()
                .filter(|m| {
                    m.provider.to_lowercase() == provider_lc && m.id.to_lowercase() == model_id_lc
                })
                .collect();
            match provider_matches.len() {
                1 => return Some(provider_matches[0].clone()),
                n if n > 1 => return None,
                _ => {}
            }
        }
    }

    let id_matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.id.to_lowercase() == normalized)
        .collect();
    if id_matches.len() == 1 {
        Some(id_matches[0].clone())
    } else {
        None
    }
}

/// Try to match a pattern to a model (`model-resolver.ts:125`).
fn try_match_model(pattern: &str, available_models: &[Model]) -> Option<Model> {
    if let Some(exact) = find_exact_model_reference_match(pattern, available_models) {
        return Some(exact);
    }

    let needle = pattern.to_lowercase();
    let matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| {
            m.id.to_lowercase().contains(&needle) || m.name.to_lowercase().contains(&needle)
        })
        .collect();
    if matches.is_empty() {
        return None;
    }

    // Prefer an alias (highest-sorting id); otherwise the latest dated version.
    let aliases: Vec<&Model> = matches
        .iter()
        .copied()
        .filter(|m| is_alias(&m.id))
        .collect();
    let pool = if aliases.is_empty() {
        &matches
    } else {
        &aliases
    };
    pool.iter()
        .max_by(|a, b| a.id.cmp(&b.id))
        .map(|m| (*m).clone())
}

/// Result of parsing a model pattern (`model-resolver.ts:157`).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedModelResult {
    /// The matched model, if any.
    pub model: Option<Model>,
    /// Thinking level if explicitly specified in the pattern.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// A diagnostic warning (e.g. invalid thinking-level suffix).
    pub warning: Option<String>,
}

/// Build a fallback custom model for an explicit provider (`model-resolver.ts:164`).
fn build_fallback_model(
    provider: &str,
    model_id: &str,
    available_models: &[Model],
) -> Option<Model> {
    let provider_models: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.provider == provider)
        .collect();
    let base = provider_models.first()?;

    let base_model = default_model_for_provider(provider)
        .and_then(|default_id| provider_models.iter().find(|m| m.id == default_id).copied())
        .unwrap_or(base);

    let mut model = (*base_model).clone();
    model.id = model_id.to_string();
    model.name = model_id.to_string();
    Some(model)
}

/// Parse a pattern into a model and optional thinking level (`model-resolver.ts:193`).
///
/// Tries the full pattern first, then splits on the last colon: a valid
/// thinking-level suffix recurses on the prefix, an invalid suffix warns (scope
/// mode) or fails (strict CLI mode).
pub fn parse_model_pattern(pattern: &str, available_models: &[Model]) -> ParsedModelResult {
    parse_model_pattern_with(pattern, available_models, true)
}

fn parse_model_pattern_with(
    pattern: &str,
    available_models: &[Model],
    allow_invalid_thinking_level_fallback: bool,
) -> ParsedModelResult {
    if let Some(model) = try_match_model(pattern, available_models) {
        return ParsedModelResult {
            model: Some(model),
            thinking_level: None,
            warning: None,
        };
    }

    let Some(last_colon) = pattern.rfind(':') else {
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    };

    let prefix = &pattern[..last_colon];
    let suffix = &pattern[last_colon + 1..];

    if let Some(level) = parse_thinking_level(suffix) {
        let result = parse_model_pattern_with(
            prefix,
            available_models,
            allow_invalid_thinking_level_fallback,
        );
        if result.model.is_some() {
            let thinking_level = if result.warning.is_some() {
                None
            } else {
                Some(level)
            };
            return ParsedModelResult {
                model: result.model,
                thinking_level,
                warning: result.warning,
            };
        }
        return result;
    }

    if !allow_invalid_thinking_level_fallback {
        // Strict mode (CLI `--model`): treat the suffix as part of the id and fail.
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    }

    let result = parse_model_pattern_with(
        prefix,
        available_models,
        allow_invalid_thinking_level_fallback,
    );
    if result.model.is_some() {
        return ParsedModelResult {
            model: result.model,
            thinking_level: None,
            warning: Some(format!(
                "Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."
            )),
        };
    }
    result
}

/// The kind of a scope diagnostic. pi only ever emits warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// A non-fatal warning.
    Warning,
}

/// A structured diagnostic from scope resolution (`model-resolver.ts:259`).
///
/// pi writes these to `console.warn`; the pure port returns them so callers (and
/// tests) can inspect them instead of spying on stderr.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelScopeDiagnostic {
    /// Diagnostic severity (always [`DiagnosticKind::Warning`]).
    pub kind: DiagnosticKind,
    /// Human-readable message.
    pub message: String,
    /// The pattern that produced this diagnostic.
    pub pattern: String,
}

/// The result of scope resolution with diagnostics (`model-resolver.ts:265`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveModelScopeResult {
    /// Resolved, de-duplicated scoped models in match order.
    pub scoped_models: Vec<ScopedModel>,
    /// Structured diagnostics (unmatched patterns, invalid suffixes).
    pub diagnostics: Vec<ModelScopeDiagnostic>,
}

fn matches_glob(pattern: &str, text: &str) -> bool {
    globset::GlobBuilder::new(pattern)
        .case_insensitive(true)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher().is_match(text))
        .unwrap_or(false)
}

/// Resolve model patterns to scoped models with structured diagnostics
/// (`model-resolver.ts:270`).
pub fn resolve_model_scope_with_diagnostics<R: ModelRuntimeView>(
    patterns: &[String],
    model_runtime: &R,
) -> ResolveModelScopeResult {
    let available_models = model_runtime.get_available();
    let mut scoped_models: Vec<ScopedModel> = Vec::new();
    let mut diagnostics: Vec<ModelScopeDiagnostic> = Vec::new();

    let push_scoped =
        |scoped: &mut Vec<ScopedModel>, model: &Model, level: Option<ModelThinkingLevel>| {
            if !scoped.iter().any(|sm| models_are_equal(&sm.model, model)) {
                scoped.push(ScopedModel {
                    model: model.clone(),
                    thinking_level: level,
                });
            }
        };

    for pattern in patterns {
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            // Extract an optional thinking-level suffix (e.g. `provider/*:high`).
            let mut glob_pattern = pattern.as_str();
            let mut thinking_level: Option<ModelThinkingLevel> = None;
            if let Some(colon) = pattern.rfind(':') {
                if let Some(level) = parse_thinking_level(&pattern[colon + 1..]) {
                    thinking_level = Some(level);
                    glob_pattern = &pattern[..colon];
                }
            }

            let matching: Vec<Model> = available_models
                .iter()
                .filter(|m| {
                    let full_id = format!("{}/{}", m.provider, m.id);
                    matches_glob(glob_pattern, &full_id) || matches_glob(glob_pattern, &m.id)
                })
                .cloned()
                .collect();

            if matching.is_empty() {
                diagnostics.push(ModelScopeDiagnostic {
                    kind: DiagnosticKind::Warning,
                    message: format!("No models match pattern \"{pattern}\""),
                    pattern: pattern.clone(),
                });
                continue;
            }

            for model in &matching {
                push_scoped(&mut scoped_models, model, thinking_level);
            }
            continue;
        }

        let parsed = parse_model_pattern(pattern, &available_models);

        if let Some(warning) = parsed.warning {
            diagnostics.push(ModelScopeDiagnostic {
                kind: DiagnosticKind::Warning,
                message: warning,
                pattern: pattern.clone(),
            });
        }

        let Some(model) = parsed.model else {
            diagnostics.push(ModelScopeDiagnostic {
                kind: DiagnosticKind::Warning,
                message: format!("No models match pattern \"{pattern}\""),
                pattern: pattern.clone(),
            });
            continue;
        };

        push_scoped(&mut scoped_models, &model, parsed.thinking_level);
    }

    ResolveModelScopeResult {
        scoped_models,
        diagnostics,
    }
}

/// Resolve model patterns to scoped models (`model-resolver.ts:334`).
///
/// pi additionally writes each diagnostic to `console.warn`; the pure port drops
/// that side effect. Callers wanting the warnings should use
/// [`resolve_model_scope_with_diagnostics`].
pub fn resolve_model_scope<R: ModelRuntimeView>(
    patterns: &[String],
    model_runtime: &R,
) -> Vec<ScopedModel> {
    resolve_model_scope_with_diagnostics(patterns, model_runtime).scoped_models
}

/// Options for [`resolve_cli_model`], mirroring pi's inline object.
#[derive(Debug, Default, Clone)]
pub struct ResolveCliModelOptions<'a> {
    /// `--provider`.
    pub cli_provider: Option<&'a str>,
    /// `--model`.
    pub cli_model: Option<&'a str>,
    /// `--thinking`.
    pub cli_thinking: Option<ModelThinkingLevel>,
}

/// The result of resolving a single CLI model (`model-resolver.ts:342`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveCliModelResult {
    /// The resolved model, if any (mutually exclusive with `error`).
    pub model: Option<Model>,
    /// A thinking level parsed from the pattern, if present.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// A non-fatal warning.
    pub warning: Option<String>,
    /// A CLI-facing error message; when set, `model` is `None`.
    pub error: Option<String>,
}

/// Resolve a single model from CLI flags (`model-resolver.ts:364`).
pub fn resolve_cli_model<R: ModelRuntimeView>(
    options: ResolveCliModelOptions,
    model_runtime: &R,
) -> ResolveCliModelResult {
    let ResolveCliModelOptions {
        cli_provider,
        cli_model,
        cli_thinking,
    } = options;

    let Some(cli_model) = cli_model else {
        return ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: None,
        };
    };

    // Important: use *all* models here, not just models with configured auth.
    let available_models = model_runtime.get_models();
    if available_models.is_empty() {
        return ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: Some(
                "No models available. Check your installation or add models to models.json."
                    .to_string(),
            ),
        };
    }

    // Canonical provider lookup (case-insensitive). Later entries win, matching
    // JS `Map.set` iteration/overwrite order.
    let canonical_provider = |name: &str| -> Option<String> {
        let lc = name.to_lowercase();
        available_models
            .iter()
            .rfind(|m| m.provider.to_lowercase() == lc)
            .map(|m| m.provider.clone())
    };

    let mut provider = cli_provider.and_then(&canonical_provider);
    if let Some(cli_provider) = cli_provider {
        if provider.is_none() {
            return ResolveCliModelResult {
                model: None,
                thinking_level: None,
                warning: None,
                error: Some(format!(
                    "Unknown provider \"{cli_provider}\". Use --list-models to see available providers/models."
                )),
            };
        }
    }

    // Without an explicit `--provider`, try to interpret `provider/model`.
    let mut pattern = cli_model.to_string();
    let mut inferred_provider = false;

    if provider.is_none() {
        if let Some(slash) = cli_model.find('/') {
            if let Some(canonical) = canonical_provider(&cli_model[..slash]) {
                provider = Some(canonical);
                pattern = cli_model[slash + 1..].to_string();
                inferred_provider = true;
            }
        }
    }

    // No provider inferred from a slash: try an exact match across all models,
    // which handles ids that naturally contain slashes.
    if provider.is_none() {
        if let Some(exact) = find_raw_exact_match(&available_models, cli_model) {
            return cli_model_ok(exact);
        }
    }

    if cli_provider.is_some() {
        if let Some(prov) = provider.as_deref() {
            // Tolerate `--model <provider>/<pattern>` by stripping the provider prefix.
            let prefix = format!("{prov}/");
            if cli_model.to_lowercase().starts_with(&prefix.to_lowercase()) {
                pattern = cli_model[prefix.len()..].to_string();
            }
        }
    }

    let candidates: Vec<Model> = match provider.as_deref() {
        Some(prov) => available_models
            .iter()
            .filter(|m| m.provider == prov)
            .cloned()
            .collect(),
        None => available_models.clone(),
    };
    let parsed = parse_model_pattern_with(&pattern, &candidates, false);

    if let Some(model) = parsed.model {
        // Prefer an authenticated exact raw model-id match over an unauthenticated
        // inferred provider/model pair.
        if inferred_provider {
            let cli_lower = cli_model.to_lowercase();
            let raw_exact: Vec<Model> = available_models
                .iter()
                .filter(|m| m.id.to_lowercase() == cli_lower && !models_are_equal(m, &model))
                .cloned()
                .collect();
            if !raw_exact.is_empty() && !model_runtime.has_configured_auth(&model.provider) {
                let authed: Vec<Model> = raw_exact
                    .into_iter()
                    .filter(|m| model_runtime.has_configured_auth(&m.provider))
                    .collect();
                if authed.len() == 1 {
                    return cli_model_ok(authed.into_iter().next().unwrap());
                }
            }
        }
        return ResolveCliModelResult {
            model: Some(model),
            thinking_level: parsed.thinking_level,
            warning: parsed.warning,
            error: None,
        };
    }

    // Inferred a provider from the slash but found no match within it: fall back
    // to matching the full input as a raw id across all models.
    if inferred_provider {
        if let Some(exact) = find_raw_exact_match(&available_models, cli_model) {
            return cli_model_ok(exact);
        }
        let fallback = parse_model_pattern_with(cli_model, &available_models, false);
        if fallback.model.is_some() {
            return ResolveCliModelResult {
                model: fallback.model,
                thinking_level: fallback.thinking_level,
                warning: fallback.warning,
                error: None,
            };
        }
    }

    if let Some(prov) = provider.as_deref() {
        // Parse a thinking-level suffix before building the fallback model, but
        // only when `--thinking` is not explicitly provided.
        let mut fallback_pattern = pattern.as_str();
        let mut fallback_thinking: Option<ModelThinkingLevel> = None;
        if cli_thinking.is_none() {
            if let Some(colon) = pattern.rfind(':') {
                if let Some(level) = parse_thinking_level(&pattern[colon + 1..]) {
                    fallback_pattern = &pattern[..colon];
                    fallback_thinking = Some(level);
                }
            }
        }

        if let Some(mut fallback_model) =
            build_fallback_model(prov, fallback_pattern, &available_models)
        {
            let requested_thinking = cli_thinking.or(fallback_thinking);
            if matches!(requested_thinking, Some(l) if l != ModelThinkingLevel::Off) {
                fallback_model.reasoning = true;
            }
            let base = format!(
                "Model \"{fallback_pattern}\" not found for provider \"{prov}\". Using custom model id."
            );
            let fallback_warning = match &parsed.warning {
                Some(w) => format!("{w} {base}"),
                None => base,
            };
            return ResolveCliModelResult {
                model: Some(fallback_model),
                thinking_level: fallback_thinking,
                warning: Some(fallback_warning),
                error: None,
            };
        }
    }

    let display = match provider.as_deref() {
        Some(prov) => format!("{prov}/{pattern}"),
        None => cli_model.to_string(),
    };
    ResolveCliModelResult {
        model: None,
        thinking_level: None,
        warning: parsed.warning,
        error: Some(format!(
            "Model \"{display}\" not found. Use --list-models to see available models."
        )),
    }
}

/// The result of initial-model selection (`model-resolver.ts:537`).
#[derive(Debug, Clone, PartialEq)]
pub struct InitialModelResult {
    /// The chosen model, if any.
    pub model: Option<Model>,
    /// The thinking level to start with.
    pub thinking_level: ModelThinkingLevel,
    /// An optional fallback message (unused on the success paths here).
    pub fallback_message: Option<String>,
}

/// Options for [`find_initial_model`], mirroring pi's inline object.
#[derive(Debug, Default, Clone)]
pub struct FindInitialModelOptions<'a> {
    /// `--provider`.
    pub cli_provider: Option<&'a str>,
    /// `--model`.
    pub cli_model: Option<&'a str>,
    /// Models from `--scope`, in order.
    pub scoped_models: Vec<ScopedModel>,
    /// Whether the session is continuing/resuming.
    pub is_continuing: bool,
    /// Saved default provider from settings.
    pub default_provider: Option<&'a str>,
    /// Saved default model id from settings.
    pub default_model_id: Option<&'a str>,
    /// Saved default thinking level from settings.
    pub default_thinking_level: Option<ModelThinkingLevel>,
}

/// Find the initial model to use, by priority (`model-resolver.ts:551`).
///
/// pi calls `process.exit(1)` on a CLI resolution error; the pure port returns
/// `Err(message)` instead so the caller decides how to surface it.
pub fn find_initial_model<R: ModelRuntimeView>(
    options: FindInitialModelOptions,
    model_runtime: &R,
) -> Result<InitialModelResult, String> {
    // 1. CLI args take priority.
    if let (Some(cli_provider), Some(cli_model)) = (options.cli_provider, options.cli_model) {
        let resolved = resolve_cli_model(
            ResolveCliModelOptions {
                cli_provider: Some(cli_provider),
                cli_model: Some(cli_model),
                cli_thinking: None,
            },
            model_runtime,
        );
        if let Some(error) = resolved.error {
            return Err(error);
        }
        if let Some(model) = resolved.model {
            return Ok(InitialModelResult {
                model: Some(model),
                thinking_level: DEFAULT_THINKING_LEVEL,
                fallback_message: None,
            });
        }
    }

    // 2. First scoped model, unless continuing/resuming.
    if !options.scoped_models.is_empty() && !options.is_continuing {
        let first = &options.scoped_models[0];
        return Ok(InitialModelResult {
            model: Some(first.model.clone()),
            thinking_level: first
                .thinking_level
                .or(options.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            fallback_message: None,
        });
    }

    // 3. Saved default from settings, if auth is configured.
    if let (Some(default_provider), Some(default_model_id)) =
        (options.default_provider, options.default_model_id)
    {
        if let Some(found) = model_runtime.get_model(default_provider, default_model_id) {
            if model_runtime.has_configured_auth(&found.provider) {
                return Ok(InitialModelResult {
                    model: Some(found),
                    thinking_level: options
                        .default_thinking_level
                        .unwrap_or(DEFAULT_THINKING_LEVEL),
                    fallback_message: None,
                });
            }
        }
    }

    // 4. First available model with valid auth, preferring known defaults.
    let available_models = model_runtime.get_available();
    if !available_models.is_empty() {
        if let Some(model) = find_known_default(&available_models) {
            return Ok(InitialModelResult {
                model: Some(model),
                thinking_level: DEFAULT_THINKING_LEVEL,
                fallback_message: None,
            });
        }
        return Ok(InitialModelResult {
            model: Some(available_models[0].clone()),
            thinking_level: DEFAULT_THINKING_LEVEL,
            fallback_message: None,
        });
    }

    // 5. No model found.
    Ok(InitialModelResult {
        model: None,
        thinking_level: DEFAULT_THINKING_LEVEL,
        fallback_message: None,
    })
}

/// Find the first available model that matches a known-provider default, in
/// [`DEFAULT_MODEL_PER_PROVIDER`] order.
fn find_known_default(available_models: &[Model]) -> Option<Model> {
    for (provider, default_id) in DEFAULT_MODEL_PER_PROVIDER {
        if let Some(m) = available_models
            .iter()
            .find(|m| m.provider == *provider && m.id == *default_id)
        {
            return Some(m.clone());
        }
    }
    None
}

/// The result of restoring a model from a saved session (`model-resolver.ts:642`).
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreModelResult {
    /// The restored or fallback model, if any.
    pub model: Option<Model>,
    /// A message describing a fallback, when one occurred.
    pub fallback_message: Option<String>,
}

/// Restore a model from a saved session, falling back if needed
/// (`model-resolver.ts:636`).
///
/// pi's `shouldPrintMessages` only gated `console.log`/`console.error` side
/// effects; the pure port drops it and returns the fallback message directly.
pub fn restore_model_from_session<R: ModelRuntimeView>(
    saved_provider: &str,
    saved_model_id: &str,
    current_model: Option<Model>,
    model_runtime: &R,
) -> RestoreModelResult {
    let restored = model_runtime.get_model(saved_provider, saved_model_id);
    let has_auth = restored
        .as_ref()
        .is_some_and(|m| model_runtime.has_configured_auth(&m.provider));

    if let Some(model) = &restored {
        if has_auth {
            return RestoreModelResult {
                model: Some(model.clone()),
                fallback_message: None,
            };
        }
    }

    let reason = if restored.is_none() {
        "model no longer exists"
    } else {
        "no auth configured"
    };

    if let Some(current) = current_model {
        return RestoreModelResult {
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                current.provider, current.id
            )),
            model: Some(current),
        };
    }

    let available_models = model_runtime.get_available();
    if !available_models.is_empty() {
        let fallback =
            find_known_default(&available_models).unwrap_or_else(|| available_models[0].clone());
        return RestoreModelResult {
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                fallback.provider, fallback.id
            )),
            model: Some(fallback),
        };
    }

    RestoreModelResult {
        model: None,
        fallback_message: None,
    }
}

#[cfg(test)]
mod tests;
