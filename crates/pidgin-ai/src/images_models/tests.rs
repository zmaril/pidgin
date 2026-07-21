// straitjacket-allow-file[:duplication] — these tests transcribe pi's
// `test/images-models.test.ts` fixtures: the `ImagesModel` and `AssistantImages`
// literals are near-identical by design and the clone detector reads them as
// duplicates; they are distinct, load-bearing fixtures kept faithful to pi.
//! Unit tests for the image-model collection, mirroring pi's
//! `packages/ai/test/images-models.test.ts`.

use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::auth::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthFlowError, AuthResult, ModelAuth, ProviderAuth,
};
use crate::providers::builtins::builtin_images_models;
use crate::seams::provider::AbortSignal;
use crate::types::{ImagesInputContent, ImagesStopReason, Modality, ModelCost};

// ---------------------------------------------------------------------------
// Test doubles (pi's `fakeAuthContext`, `testProvider`, and recording `api`)
// ---------------------------------------------------------------------------

/// pi's `fakeAuthContext(env)`.
struct FakeAuthContext {
    env: BTreeMap<String, String>,
}

impl AuthContext for FakeAuthContext {
    fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }
    fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn fake_auth_context(pairs: &[(&str, &str)]) -> Arc<dyn AuthContext + Send + Sync> {
    Arc::new(FakeAuthContext {
        env: pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    })
}

/// The custom api-key handler pi builds inline in `testProvider`.
struct TestApiKeyAuth {
    env_var: Option<String>,
}

impl ApiKeyAuth for TestApiKeyAuth {
    fn name(&self) -> &str {
        "Test key"
    }

    fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, AuthFlowError> {
        let Some(env_var) = &self.env_var else {
            // pi: `if (!input.envVar) return { auth: {} }`.
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: None,
            }));
        };
        // pi: `const key = credential?.key ?? (await ctx.env(input.envVar))`.
        let key = credential
            .and_then(|c| c.key.clone())
            .or_else(|| ctx.env(env_var));
        Ok(key.map(|key| AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                ..ModelAuth::default()
            },
            env: None,
            // pi: `source: credential ? "stored" : input.envVar`.
            source: Some(if credential.is_some() {
                "stored".to_string()
            } else {
                env_var.clone()
            }),
        }))
    }
}

#[derive(Clone)]
struct GenerateCall {
    #[allow(dead_code)]
    model: ImagesModel,
    options: Option<ImagesOptions>,
}

type Calls = Arc<Mutex<Vec<GenerateCall>>>;

/// A recording `ProviderImages` (pi's inline `api: { generateImages }`).
struct RecordingApi {
    calls: Calls,
}

impl ProviderImages for RecordingApi {
    fn generate_images(
        &self,
        model: &ImagesModel,
        _context: &ImagesContext,
        options: Option<&ImagesOptions>,
        _signal: Option<&AbortSignal>,
    ) -> AssistantImages {
        self.calls.lock().unwrap().push(GenerateCall {
            model: model.clone(),
            options: options.cloned(),
        });
        ok_result(model)
    }
}

fn test_image_model(provider: &str, id: &str) -> ImagesModel {
    ImagesModel {
        id: id.into(),
        name: id.into(),
        api: "test-images".into(),
        provider: provider.into(),
        base_url: "https://example.test/v1".into(),
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: None,
        output: vec![Modality::Image],
    }
}

fn ok_result(model: &ImagesModel) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: vec![ImagesInputContent::Image {
            data: "aGk=".into(),
            mime_type: "image/png".into(),
        }],
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: 1,
    }
}

fn test_provider(
    id: &str,
    models: Vec<ImagesModel>,
    env_var: Option<&str>,
    calls: Calls,
) -> Arc<dyn ImagesProvider> {
    create_images_provider(CreateImagesProviderOptions {
        id: id.into(),
        name: None,
        auth: ProviderAuth {
            api_key: Some(Box::new(TestApiKeyAuth {
                env_var: env_var.map(str::to_string),
            })),
            oauth: None,
        },
        models,
        refresh_models: None,
        api: Arc::new(RecordingApi { calls }),
    })
}

fn context() -> ImagesContext {
    ImagesContext {
        input: vec![ImagesInputContent::Text {
            text: "a red circle".into(),
            text_signature: None,
        }],
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn registers_providers_and_reads_models_synchronously() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut models = create_images_models(CreateImagesModelsOptions::default());
    models.set_provider(test_provider(
        "p1",
        vec![test_image_model("p1", "m1"), test_image_model("p1", "m2")],
        None,
        calls.clone(),
    ));
    models.set_provider(test_provider(
        "p2",
        vec![test_image_model("p2", "m3")],
        None,
        calls.clone(),
    ));

    assert_eq!(
        models
            .get_providers()
            .iter()
            .map(|p| p.id())
            .collect::<Vec<_>>(),
        vec!["p1", "p2"]
    );
    assert_eq!(
        models
            .get_models(None)
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["m1", "m2", "m3"]
    );
    assert_eq!(
        models
            .get_models(Some("p1"))
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["m1", "m2"]
    );
    assert_eq!(
        models.get_model("p2", "m3").map(|m| m.id),
        Some("m3".to_string())
    );
    assert!(models.get_model("p2", "missing").is_none());

    models.delete_provider("p1");
    assert!(models.get_provider("p1").is_none());
}

#[test]
fn resolves_auth_and_merges_into_requests_explicit_options_win() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        auth_context: Some(fake_auth_context(&[("TEST_KEY", "env-key")])),
        ..CreateImagesModelsOptions::default()
    });
    models.set_provider(test_provider(
        "p1",
        vec![test_image_model("p1", "model-a")],
        Some("TEST_KEY"),
        calls.clone(),
    ));
    let model = models.get_model("p1", "model-a").unwrap();

    assert_eq!(
        models
            .get_auth_for_model(&model, None)
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("env-key".to_string())
    );
    assert_eq!(
        models
            .get_auth_for_provider(&model.provider, None)
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("env-key".to_string())
    );
    let overrides = AuthResolutionOverrides {
        api_key: Some("explicit-key".into()),
        env: None,
    };
    assert_eq!(
        models
            .get_auth_for_model(&model, Some(&overrides))
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("explicit-key".to_string())
    );

    let result = models.generate_images(&model, &context(), None, None);
    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    assert_eq!(
        calls.lock().unwrap()[0]
            .options
            .as_ref()
            .unwrap()
            .api_key
            .as_deref(),
        Some("env-key")
    );

    let explicit = ImagesOptions {
        api_key: Some("explicit".into()),
        ..ImagesOptions::default()
    };
    models.generate_images(&model, &context(), Some(&explicit), None);
    assert_eq!(
        calls.lock().unwrap()[1]
            .options
            .as_ref()
            .unwrap()
            .api_key
            .as_deref(),
        Some("explicit")
    );
}

#[test]
fn merges_provider_resolved_env_into_image_options() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut models = create_images_models(CreateImagesModelsOptions::default());
    models.set_provider(create_images_provider(CreateImagesProviderOptions {
        id: "p1".into(),
        name: None,
        auth: ProviderAuth {
            api_key: Some(Box::new(ProviderKeyWithEnv)),
            oauth: None,
        },
        models: vec![test_image_model("p1", "model-a")],
        refresh_models: None,
        api: Arc::new(RecordingApi {
            calls: calls.clone(),
        }),
    }));
    let model = models.get_model("p1", "model-a").unwrap();

    let options = ImagesOptions {
        api_key: Some("request-key".into()),
        env: Some(BTreeMap::from([
            ("REQUEST_ONLY".to_string(), "request".to_string()),
            ("SHARED".to_string(), "request".to_string()),
        ])),
        ..ImagesOptions::default()
    };
    models.generate_images(&model, &context(), Some(&options), None);

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded[0].options.as_ref().unwrap().api_key.as_deref(),
        Some("request-key")
    );
    assert_eq!(
        recorded[0].options.as_ref().unwrap().env,
        Some(BTreeMap::from([
            ("PROVIDER_ONLY".to_string(), "provider".to_string()),
            ("REQUEST_ONLY".to_string(), "request".to_string()),
            ("SHARED".to_string(), "request".to_string()),
        ]))
    );
}

/// The inline provider whose resolve returns provider-scoped env (pi's second
/// inline provider in the env-merge test).
struct ProviderKeyWithEnv;

impl ApiKeyAuth for ProviderKeyWithEnv {
    fn name(&self) -> &str {
        "Test key"
    }
    fn resolve(
        &self,
        _ctx: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, AuthFlowError> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("provider-key".into()),
                ..ModelAuth::default()
            },
            env: Some(BTreeMap::from([
                ("PROVIDER_ONLY".to_string(), "provider".to_string()),
                ("SHARED".to_string(), "provider".to_string()),
            ])),
            source: None,
        }))
    }
}

#[test]
fn returns_error_for_unknown_providers_and_dispatches_unconfigured() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut models = create_images_models(CreateImagesModelsOptions {
        auth_context: Some(fake_auth_context(&[])),
        ..CreateImagesModelsOptions::default()
    });

    let ghost = models.generate_images(&test_image_model("ghost", "m"), &context(), None, None);
    assert_eq!(ghost.stop_reason, ImagesStopReason::Error);
    assert!(ghost
        .error_message
        .unwrap()
        .contains("Unknown provider: ghost"));

    // Unconfigured (resolve -> None) still dispatches; the provider decides.
    models.set_provider(test_provider(
        "p1",
        vec![test_image_model("p1", "model-a")],
        Some("MISSING"),
        calls.clone(),
    ));
    let model = models.get_model("p1", "model-a").unwrap();
    assert!(models.get_auth_for_model(&model, None).unwrap().is_none());
    models.generate_images(&model, &context(), None, None);
    assert!(calls.lock().unwrap()[0]
        .options
        .as_ref()
        .and_then(|o| o.api_key.as_ref())
        .is_none());
}

#[test]
fn supports_dynamic_providers_via_refresh() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let fetches_hook = fetches.clone();
    let provider = create_images_provider(CreateImagesProviderOptions {
        id: "dyn".into(),
        name: None,
        auth: ProviderAuth {
            api_key: Some(Box::new(TestApiKeyAuth { env_var: None })),
            oauth: None,
        },
        models: vec![],
        refresh_models: Some(Box::new(move || {
            fetches_hook.fetch_add(1, Ordering::SeqCst);
            Ok(vec![test_image_model("dyn", "listed")])
        })),
        api: Arc::new(RecordingApi {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
    });
    let mut models = create_images_models(CreateImagesModelsOptions::default());
    models.set_provider(provider);

    assert!(models.get_models(Some("dyn")).is_empty());
    // The JS concurrent in-flight dedupe has no sync analog; one refresh fetches once.
    models.refresh(Some("dyn")).unwrap();
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    assert!(models.get_model("dyn", "listed").is_some());

    // A single-provider fetch failure rejects with ModelsError model_source.
    models.set_provider(create_images_provider(CreateImagesProviderOptions {
        id: "flaky".into(),
        name: None,
        auth: ProviderAuth {
            api_key: Some(Box::new(TestApiKeyAuth { env_var: None })),
            oauth: None,
        },
        models: vec![],
        refresh_models: Some(Box::new(|| Err(ProviderRefreshError::new("fetch failed")))),
        api: Arc::new(RecordingApi {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
    }));
    let err = models.refresh(Some("flaky")).unwrap_err();
    assert_eq!(err.code, crate::auth::ModelsErrorCode::ModelSource);
    // Refreshing all providers best-effort never rejects.
    assert!(models.refresh(None).is_ok());
}

#[test]
fn builtin_images_models_registers_openrouter_with_catalog() {
    let models = builtin_images_models(CreateImagesModelsOptions {
        auth_context: Some(fake_auth_context(&[("OPENROUTER_API_KEY", "or-key")])),
        ..CreateImagesModelsOptions::default()
    });
    assert_eq!(
        models
            .get_providers()
            .iter()
            .map(|p| p.id())
            .collect::<Vec<_>>(),
        vec!["openrouter"]
    );

    let list = models.get_models(Some("openrouter"));
    assert!(!list.is_empty());
    assert!(list.iter().all(|m| m.api == "openrouter-images"));

    assert_eq!(
        models
            .get_auth_for_model(&list[0], None)
            .unwrap()
            .and_then(|r| r.auth.api_key),
        Some("or-key".to_string())
    );
}
