//! Module loading + factory execution: the JS-execution half of pi's
//! `loader.ts`.
//!
//! This is the port of pi's `loadExtension` execution path (`loader.ts:443`):
//! given an extension's source, evaluate it and run its default-export factory
//! with the `pi` object. pi does this with jiti (`jiti.import(path, { default:
//! true })` then `await factory(api)`); atilla does it on the embedded
//! `deno_core` runtime:
//!
//!   1. **Transpile.** pi's extensions are TypeScript; V8 only runs JavaScript.
//!      [`transpile_ts`] strips the types with `deno_ast` — the same thing jiti
//!      does under the hood. JavaScript entrypoints skip this step.
//!   2. **Evaluate as an ES module.** The transpiled source is loaded with
//!      `load_main_es_module_from_code` and evaluated, so `export default` (and
//!      any top-level `await`) work with real ES-module semantics.
//!   3. **Extract the default export.** The module namespace's `default` key is
//!      the factory function; a missing / non-function default is pi's
//!      "does not export a valid factory function" error.
//!   4. **Run the factory.** It is called with `globalThis.__pi` (built by the
//!      `api_ops` bootstrap); its `pi.register*` / `pi.on` calls populate the
//!      Rust [`Inventory`] through the ops.
//!
//! # TypeScript transpile
//!
//! `deno_ast` handles the type-stripping this PR needs (it is exactly the spike's
//! `transpile_ts`). What it does **not** do is module *resolution*: an extension
//! that `import`s another module (`@sinclair/typebox`, a sibling file, its own
//! `node_modules`) needs a `deno_core` `ModuleLoader` reproducing jiti's
//! resolution — the highest-risk jiti-parity surface (recon blocker #6). PR-E
//! deliberately scopes to import-free entrypoints (pi's `(pi) => {}` factory
//! modules); wiring the resolver is a follow-up.
//!
//! # Error semantics (mirroring pi)
//!
//! - a syntax error / a module that throws at evaluation -> `Failed to load
//!   extension: <message>` (pi's catch-all wrap, `loader.ts:476`);
//! - a missing / non-function default export -> `Extension does not export a
//!   valid factory function: <id>` (pi's `loader.ts:465`);
//! - a factory that throws -> `Failed to load extension: <message>`.

use deno_core::v8;
use deno_core::{JsRuntime, ModuleSpecifier, PollEventLoopOptions};

use std::sync::atomic::{AtomicU64, Ordering};

use crate::api_ops::SharedInventory;
use crate::inventory::Inventory;
use crate::runtime::SourceLanguage;

/// The synthetic module-specifier scheme extensions load under.
const SPECIFIER_PREFIX: &str = "file:///atilla-extension/";

/// Monotonic per-load counter making every extension's module specifier unique.
///
/// deno_core keys modules by specifier and rejects loading a second module under
/// an existing one. Two extensions can legitimately share an id (or the same one
/// can be reloaded), so the specifier must be unique per load regardless of id.
static LOAD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build a unique `file://` module specifier from an extension id, sanitizing it
/// to a URL-safe stem and suffixing a monotonic counter for uniqueness.
fn make_specifier(id: &str) -> Result<ModuleSpecifier, String> {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem: &str = if sanitized.is_empty() {
        "extension"
    } else {
        sanitized.as_str()
    };
    let n = LOAD_COUNTER.fetch_add(1, Ordering::Relaxed);
    let url = format!("{SPECIFIER_PREFIX}{stem}-{n}.ts");
    ModuleSpecifier::parse(&url)
        .map_err(|e| format!("Failed to load extension: bad specifier: {e}"))
}

/// Strip TypeScript types, producing runnable JavaScript (the spike's
/// `transpile_ts`). Parse/transpile failures map to pi's load-error wording.
pub fn transpile_ts(specifier: &ModuleSpecifier, source: &str) -> Result<String, String> {
    use deno_ast::{
        parse_module, EmitOptions, MediaType, ParseParams, SourceMapOption, TranspileModuleOptions,
        TranspileOptions,
    };

    let parsed = parse_module(ParseParams {
        specifier: specifier.clone(),
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|e| format!("Failed to load extension: {e}"))?;

    let transpiled = parsed
        .transpile(
            &TranspileOptions::default(),
            &TranspileModuleOptions { module_kind: None },
            &EmitOptions {
                source_map: SourceMapOption::None,
                ..Default::default()
            },
        )
        .map_err(|e| format!("Failed to load extension: {e}"))?;

    Ok(transpiled.into_source().text)
}

/// Load one extension's `source` and run its factory, returning the [`Inventory`]
/// it registered (or a pi-style load error). The `inventory` is reset first, so
/// the returned value carries exactly this extension's registrations.
pub async fn load_extension(
    runtime: &mut JsRuntime,
    inventory: &SharedInventory,
    id: &str,
    source: &str,
    language: SourceLanguage,
) -> Result<Inventory, String> {
    *inventory.borrow_mut() = Inventory::new();

    let specifier = make_specifier(id)?;
    let code = match language {
        SourceLanguage::TypeScript => transpile_ts(&specifier, source)?,
        SourceLanguage::JavaScript => source.to_string(),
    };

    // 1. Load + evaluate as an ES module. A *side* module (not "main"): a
    //    JsRuntime allows only one main module for its whole life, but a plane
    //    loads many extensions, so each is a side module.
    let mod_id = runtime
        .load_side_es_module_from_code(&specifier, code)
        .await
        .map_err(|e| format!("Failed to load extension: {e}"))?;
    let eval = runtime.mod_evaluate(mod_id);
    runtime
        .run_event_loop(PollEventLoopOptions::default())
        .await
        .map_err(|e| format!("Failed to load extension: {e}"))?;
    eval.await
        .map_err(|e| format!("Failed to load extension: {e}"))?;

    // 2. Extract the default export (the factory).
    let namespace = runtime
        .get_module_namespace(mod_id)
        .map_err(|e| format!("Failed to load extension: {e}"))?;
    let Some(factory) = extract_default_function(runtime, &namespace) else {
        return Err(format!(
            "Extension does not export a valid factory function: {id}"
        ));
    };

    // 3. Run the factory with the `pi` object; its register* calls fire the ops.
    let pi = pi_global(runtime);
    let call = runtime.call_with_args(&factory, &[pi]);
    runtime
        .with_event_loop_promise(call, PollEventLoopOptions::default())
        .await
        .map_err(|e| format!("Failed to load extension: {e}"))?;

    Ok(inventory.borrow().clone())
}

/// Read the `default` export from a module namespace, returning it only if it is
/// a function (pi's `typeof factory !== "function"` check, `loader.ts:419`).
fn extract_default_function(
    runtime: &mut JsRuntime,
    namespace: &v8::Global<v8::Object>,
) -> Option<v8::Global<v8::Function>> {
    deno_core::scope!(scope, runtime);
    let namespace = v8::Local::new(scope, namespace);
    let key = v8::String::new(scope, "default")?;
    let value = namespace.get(scope, key.into())?;
    if !value.is_function() {
        return None;
    }
    let function: v8::Local<v8::Function> = value.try_into().ok()?;
    Some(v8::Global::new(scope, function))
}

/// Read `globalThis.__pi` (installed by the `api_ops` bootstrap) as a value to
/// pass to the factory.
fn pi_global(runtime: &mut JsRuntime) -> v8::Global<v8::Value> {
    deno_core::scope!(scope, runtime);
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = v8::String::new(scope, "__pi").expect("intern __pi");
    let value = global
        .get(scope, key.into())
        .expect("__pi is installed by the bootstrap script");
    v8::Global::new(scope, value)
}
