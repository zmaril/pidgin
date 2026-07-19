//! Integration tests for OAuth phase (A): the shared one-shot invoke-stored
//! primitive, provider capture, the retrofitted tool-`execute` / command-`handler`
//! placeholders, and the concrete [`DenoExtensionOAuthLogin`] seam impl.
//!
//! These load a small inline pi-style extension onto the real embedded
//! `deno_core` runtime and assert:
//!
//!   * `JsPlaneHandle::invoke_stored` invokes a tool's `execute` / a command's
//!     `handler` and returns the expected JSON;
//!   * `pi.registerProvider(config)` captures a `ProviderRecord` into the
//!     Inventory (with the `oauth` closure-presence flags);
//!   * the retrofitted `DenoExtensionRunner` tool `execute` / command `handler`
//!     dispatch through the primitive to the live JS closures;
//!   * `DenoExtensionOAuthLogin::get_api_key` / `refresh_token` invoke the stored
//!     provider `oauth` closures and map their results;
//!   * `DenoExtensionOAuthLogin::login` returns the pending-primitive
//!     [`AuthFlowError`] error-stub (never a silent no-op).
//!
//! The whole file is gated on the `deno` feature — it compiles and runs ONLY in
//! the dedicated `deno runtime (V8)` CI job, since building `deno_core` needs the
//! V8 blob that 403s in-sandbox.
#![cfg(feature = "deno")]

use std::sync::Arc;

use serde_json::{json, Map};

use pidgin_ai::auth::error::AuthFlowError;
use pidgin_ai::auth::oauth::extension::{
    ExtensionOAuthLogin, OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt,
    OAuthSelectPrompt,
};
use pidgin_ai::auth::types::OAuthCredential;

use pidgin_coding::core::extensions::runner::ExtensionRunner as RunnerTrait;

use pidgin_extensions::{
    DenoExtensionOAuthLogin, DenoExtensionRunner, JsPlaneHandle, MinimalExtensionContext,
    SourceLanguage,
};

/// A pi-style extension registering a tool (`echo`) with an `execute`, a command
/// (`greet`) with a `handler`, and a provider (`acme`) with `oauth.getApiKey` /
/// `refreshToken` / `login`.
const FIXTURE: &str = r#"
export default (pi) => {
  pi.registerTool({
    name: "echo",
    description: "echo the args back in details",
    parameters: { type: "object" },
    execute: (id, args) => ({ content: [], details: { id, echoed: args } }),
  });

  pi.registerCommand("greet", {
    description: "greet the args",
    handler: (args) => { globalThis.__lastGreet = args; },
  });

  pi.registerProvider({
    name: "acme",
    baseUrl: "https://acme.test",
    authHeader: true,
    oauth: {
      name: "acme-oauth",
      getApiKey: (cred) => "key-for-" + cred.access,
      refreshToken: async (cred) => ({
        refresh: cred.refresh,
        access: cred.access + "-refreshed",
        expires: cred.expires + 1000,
      }),
      login: async (_callbacks) => ({ refresh: "r", access: "a", expires: 0 }),
    },
  });
};
"#;

/// Spawn a plane (as a shared `Arc`) and load the fixture onto it, returning the
/// plane and the loaded inventory.
async fn load_fixture() -> (Arc<JsPlaneHandle>, pidgin_extensions::Inventory) {
    let plane = Arc::new(JsPlaneHandle::spawn());
    let inventory = plane
        .load_extension_source("fixture", FIXTURE, SourceLanguage::TypeScript)
        .await
        .expect("fixture loads");
    (plane, inventory)
}

/// A no-op [`OAuthLoginCallbacks`] — `login` returns its error-stub before ever
/// touching the callbacks, so nothing here is exercised.
struct NoopCallbacks;
impl OAuthLoginCallbacks for NoopCallbacks {
    fn on_auth(&self, _info: OAuthAuthInfo) {}
    fn on_device_code(&self, _info: OAuthDeviceCodeInfo) {}
    fn on_prompt(&self, _prompt: OAuthPrompt) -> Result<String, AuthFlowError> {
        Ok(String::new())
    }
    fn on_progress(&self, _message: String) {}
    fn on_manual_code_input(&self) -> Result<String, AuthFlowError> {
        Ok(String::new())
    }
    fn on_select(&self, _prompt: OAuthSelectPrompt) -> Result<Option<String>, AuthFlowError> {
        Ok(None)
    }
}

/// Build a sample OAuth credential.
fn sample_credential() -> OAuthCredential {
    OAuthCredential {
        refresh: "refresh-token".into(),
        access: "access-token".into(),
        expires: 1_000,
        extra: Map::new(),
    }
}

// -------------------------------------------------------------------------
// One-shot invoke-stored primitive
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invoke_stored_runs_a_tool_execute() {
    let (plane, _inv) = load_fixture().await;

    let invocation = plane
        .invoke_stored("tool", "echo", &json!(["call-1", { "x": 42 }]))
        .await
        .expect("invoke echo");

    assert!(invocation.ok, "expected ok, got {:?}", invocation.error);
    assert_eq!(invocation.result["details"]["id"], json!("call-1"));
    assert_eq!(invocation.result["details"]["echoed"], json!({ "x": 42 }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invoke_stored_runs_a_command_handler() {
    let (plane, _inv) = load_fixture().await;

    let invocation = plane
        .invoke_stored("command", "greet", &json!(["world"]))
        .await
        .expect("invoke greet");
    assert!(invocation.ok, "expected ok, got {:?}", invocation.error);

    // The handler ran its side effect on the plane.
    let last = plane
        .eval("globalThis.__lastGreet")
        .await
        .expect("read effect");
    assert_eq!(last, json!("world"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invoke_stored_missing_key_is_isolated() {
    let (plane, _inv) = load_fixture().await;

    let invocation = plane
        .invoke_stored("tool", "nonexistent", &json!([]))
        .await
        .expect("invoke returns an envelope, not an error");
    assert!(!invocation.ok);
    assert!(invocation
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("nonexistent"));
}

// -------------------------------------------------------------------------
// Provider capture
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_provider_captures_a_record() {
    let (_plane, inventory) = load_fixture().await;

    assert_eq!(inventory.providers.len(), 1);
    let provider = &inventory.providers[0];
    assert_eq!(provider.name, "acme");
    assert_eq!(provider.base_url.as_deref(), Some("https://acme.test"));
    assert_eq!(provider.auth_header, Some(true));
    assert!(provider.has_oauth);
    assert!(provider.has_get_api_key);
    assert!(provider.has_refresh_token);
    assert!(provider.has_login);
    assert_eq!(provider.oauth_name.as_deref(), Some("acme-oauth"));
}

// -------------------------------------------------------------------------
// Retrofitted ExtensionRunner tool / command dispatch
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_tool_execute_dispatches_through_primitive() {
    let (plane, inventory) = load_fixture().await;
    let runner = DenoExtensionRunner::from_loaded(plane, vec![("fixture".into(), inventory)], "/p");

    let tools = runner.get_all_registered_tools();
    let echo = tools
        .iter()
        .find(|t| t.tool.name == "echo")
        .expect("echo tool registered");

    let ctx = MinimalExtensionContext::new("/p");
    let result = (echo.tool.execute)("call-9", &json!({ "hello": "there" }), None, None, &ctx);

    assert_eq!(result.details["id"], json!("call-9"));
    assert_eq!(result.details["echoed"], json!({ "hello": "there" }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runner_command_handler_dispatches_through_primitive() {
    let (plane, inventory) = load_fixture().await;
    let runner = DenoExtensionRunner::from_loaded(
        Arc::clone(&plane),
        vec![("fixture".into(), inventory)],
        "/p",
    );

    let command = runner.get_command("greet").expect("greet command resolved");
    let ctx = MinimalExtensionContext::new("/p");
    (command.command.handler)("hello-args", &ctx).expect("handler runs ok");

    let last = plane
        .eval("globalThis.__lastGreet")
        .await
        .expect("read effect");
    assert_eq!(last, json!("hello-args"));
}

// -------------------------------------------------------------------------
// DenoExtensionOAuthLogin
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oauth_get_api_key_invokes_stored_closure() {
    let (plane, _inv) = load_fixture().await;
    let login = DenoExtensionOAuthLogin::new(plane, "acme");

    let key = login
        .get_api_key(&sample_credential())
        .expect("get_api_key succeeds");
    assert_eq!(key, "key-for-access-token");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oauth_refresh_token_invokes_stored_closure() {
    let (plane, _inv) = load_fixture().await;
    let login = DenoExtensionOAuthLogin::new(plane, "acme");

    let refreshed = login
        .refresh_token(&sample_credential())
        .expect("refresh_token succeeds");
    assert_eq!(refreshed.access, "access-token-refreshed");
    assert_eq!(refreshed.refresh, "refresh-token");
    assert_eq!(refreshed.expires, 2_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oauth_login_is_pending_primitive_error_stub() {
    let (plane, _inv) = load_fixture().await;
    let login = DenoExtensionOAuthLogin::new(plane, "acme");

    let error = login
        .login(&NoopCallbacks)
        .expect_err("login is a documented error-stub");
    assert!(
        error.message.contains("pending reentrant primitive"),
        "unexpected login error message: {}",
        error.message
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oauth_get_api_key_maps_unknown_provider_to_error() {
    let (plane, _inv) = load_fixture().await;
    let login = DenoExtensionOAuthLogin::new(plane, "ghost");

    let error = login
        .get_api_key(&sample_credential())
        .expect_err("unknown provider fails");
    assert!(
        error.message.contains("ghost"),
        "unexpected error message: {}",
        error.message
    );
}
