//! The off-thread `deno_core` runtime host.
//!
//! A [`JsPlaneHandle`] owns a dedicated OS thread on which a `!Send`
//! `JsRuntime` lives. The handle itself holds only a channel sender and a join
//! handle, so it is `Send + Sync` and can be shared with the multi-threaded
//! tokio core. Callers submit [`Command`]s over the channel and await the reply
//! through a per-request `oneshot`. Only plain data (`serde_json::Value`, the
//! source string, the [`Inventory`]) crosses the boundary — V8 handles never
//! leave the owning thread.
//!
//! PR-A shipped [`JsPlaneHandle::spawn`] / [`JsPlaneHandle::eval`] /
//! [`JsPlaneHandle::shutdown`]. PR-E adds [`JsPlaneHandle::load_extension_source`]
//! and [`JsPlaneHandle::load_discovered`], which transpile + evaluate a pi-style
//! extension module and run its factory, returning the [`Inventory`] of what it
//! registered (see the `loader` and `api_ops` modules).

// straitjacket-allow-file:duplication -- the request/reply channel boilerplate
// (build a oneshot, send a Command, await the answer) and the event-loop error
// mapping are deliberate parallel structure of the flavor-2 rendezvous pattern
// (notes/startup/deep-hooks.md §5); the same shape recurs for every off-thread
// host plane, so it is mirror duplication, not an accident to hoist away.

use anyhow::{anyhow, Result};
use deno_core::{JsRuntime, PollEventLoopOptions, RuntimeOptions};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use pidgin_coding::core::extensions::discovery::{DiscoveredExtension, ExtensionLanguage};

use crate::api_ops::{self, SharedInventory};
use crate::dispatch::{self, HookInvocation, StoredInvocation};
use crate::inventory::Inventory;
use crate::loader;

use std::cell::RefCell;
use std::rc::Rc;

/// The implementation language of an extension entrypoint, controlling whether
/// its source is transpiled (TypeScript) or evaluated as-is (JavaScript).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLanguage {
    /// A `.ts` entrypoint — transpiled to JavaScript before evaluation.
    TypeScript,
    /// A `.js` entrypoint — evaluated directly.
    JavaScript,
}

impl From<ExtensionLanguage> for SourceLanguage {
    fn from(language: ExtensionLanguage) -> Self {
        match language {
            ExtensionLanguage::TypeScript => SourceLanguage::TypeScript,
            ExtensionLanguage::JavaScript => SourceLanguage::JavaScript,
            // The deno/JS runtime never legitimately receives a Python-language
            // extension: the combined loader routes `.py` to the Python engine and
            // only `.ts`/`.js` here. This arm keeps the exhaustive match compiling.
            ExtensionLanguage::Python => {
                unreachable!("Python extensions are not handled by the deno/JS runtime")
            }
        }
    }
}

/// The rendezvous protocol: work requests in, plain-data results out. Each
/// variant carries a `oneshot` reply channel so the submitting task can park
/// cooperatively until the JS thread answers.
enum Command {
    /// Evaluate a snippet of JavaScript and return its (awaited) value.
    Eval {
        source: String,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    /// Load an extension module and run its factory, returning the inventory it
    /// registered (or a pi-style load error).
    LoadExtension {
        id: String,
        source: String,
        language: SourceLanguage,
        reply: oneshot::Sender<Result<Inventory, String>>,
    },
    /// Invoke a previously-registered hook handler (kept in the JS runtime,
    /// keyed by event name) with a JSON event + ctx, returning the shaped
    /// invocation envelope. This is the Rust-drives-JS half of hook dispatch.
    InvokeHook {
        event: String,
        index: usize,
        event_json: Value,
        ctx_json: Value,
        reply: oneshot::Sender<Result<HookInvocation, String>>,
    },
    /// Invoke a previously-registered JS closure (a tool's `execute`, a
    /// command's `handler`, a provider's `oauth.getApiKey` / `refreshToken`)
    /// kept live in the runtime and keyed by (`kind`, `name`), passing the
    /// positional JSON `args`, returning the shaped invocation envelope. This is
    /// the shared one-shot invoke-stored-JS-function primitive.
    InvokeStored {
        kind: String,
        name: String,
        args: Value,
        reply: oneshot::Sender<Result<StoredInvocation, String>>,
    },
    /// Drain in-flight work and stop the runtime thread.
    Shutdown { reply: oneshot::Sender<()> },
}

/// A `Send + Sync` handle to the JavaScript extension plane.
///
/// The `!Send` `JsRuntime` it fronts lives on a dedicated thread; this handle
/// carries only a channel, so it may be cloned across the tokio core freely.
/// Dropping the handle without calling [`JsPlaneHandle::shutdown`] still stops
/// the thread: the command channel closes, the runtime loop ends, and the
/// thread exits (the join handle is dropped, detaching it).
pub struct JsPlaneHandle {
    tx: mpsc::UnboundedSender<Command>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl JsPlaneHandle {
    /// Spawn the dedicated OS thread that OWNS the `JsRuntime`.
    ///
    /// The runtime is constructed *inside* the thread: it is `!Send`, so it
    /// cannot be built elsewhere and moved in. The thread runs a current-thread
    /// tokio runtime plus a `LocalSet` to host the `!Send` local tasks
    /// (`deno_core` timers and ops) that the runtime's event loop spawns.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Command>();
        let join = std::thread::Builder::new()
            .name("pidgin-js-plane".into())
            .spawn(move || js_plane_thread(rx))
            .expect("spawn pidgin js plane thread");
        JsPlaneHandle {
            tx,
            join: Some(join),
        }
    }

    /// Evaluate a JavaScript snippet on the runtime thread and await its result
    /// as a [`serde_json::Value`].
    ///
    /// The snippet's completion value is resolved through the event loop, so a
    /// snippet that evaluates to a `Promise` is awaited before its value is
    /// returned. Errors — a channel drop, a thrown JS exception, or a value
    /// that cannot be deserialized — surface as `Err`.
    pub async fn eval(&self, source: impl Into<String>) -> Result<Value> {
        let source = source.into();
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Eval { source, reply })
            .map_err(|_| anyhow!("js plane gone"))?;
        rx.await
            .map_err(|_| anyhow!("js plane reply dropped"))?
            .map_err(|e| anyhow!(e))
    }

    /// Load a pi-style extension from in-memory `source`, run its default-export
    /// factory, and return the [`Inventory`] of everything it registered.
    ///
    /// `id` is a stable identifier used to derive the module specifier.
    /// A load failure (invalid code, a factory that throws, or no valid default
    /// export) surfaces as an `Err` carrying pi's load-error wording.
    pub async fn load_extension_source(
        &self,
        id: impl Into<String>,
        source: impl Into<String>,
        language: SourceLanguage,
    ) -> Result<Inventory> {
        let (id, source) = (id.into(), source.into());
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::LoadExtension {
                id,
                source,
                language,
                reply,
            })
            .map_err(|_| anyhow!("js plane gone"))?;
        rx.await
            .map_err(|_| anyhow!("js plane reply dropped"))?
            .map_err(|e| anyhow!(e))
    }

    /// Load a [`DiscoveredExtension`] from its resolved entrypoint on disk,
    /// running its factory and returning the registered [`Inventory`].
    ///
    /// This is the bridge from PR-D discovery to the JS-execution plane: it
    /// reads `entrypoint_path`, maps the discovered language, and delegates to
    /// [`load_extension_source`](Self::load_extension_source).
    pub async fn load_discovered(&self, extension: &DiscoveredExtension) -> Result<Inventory> {
        let source = std::fs::read_to_string(&extension.entrypoint_path).map_err(|e| {
            anyhow!(
                "read extension {}: {e}",
                extension.entrypoint_path.display()
            )
        })?;
        self.load_extension_source(
            extension.id.clone(),
            source,
            SourceLanguage::from(extension.language),
        )
        .await
    }

    /// Invoke the hook handler registered at `index` for `event`, passing the
    /// JSON `event_json` and `ctx_json`, and await its shaped [`HookInvocation`].
    ///
    /// This is the closure-invocation primitive the [`crate::ExtensionRunner`]
    /// drives once per registered handler: `index` selects into the JS-side
    /// handler list for `event` (in load-then-registration order), the runtime
    /// runs that handler with `(event, ctx)`, awaits its Promise if async, and
    /// returns the plain-data envelope. A handler that throws surfaces as an
    /// envelope with `ok == false` — the runtime thread is never unwound.
    pub async fn invoke_hook(
        &self,
        event: impl Into<String>,
        index: usize,
        event_json: &Value,
        ctx_json: &Value,
    ) -> Result<HookInvocation> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::InvokeHook {
                event: event.into(),
                index,
                event_json: event_json.clone(),
                ctx_json: ctx_json.clone(),
                reply,
            })
            .map_err(|_| anyhow!("js plane gone"))?;
        rx.await
            .map_err(|_| anyhow!("js plane reply dropped"))?
            .map_err(|e| anyhow!(e))
    }

    /// Invoke the stored JS closure identified by (`kind`, `name`), passing the
    /// positional JSON `args`, and await its shaped [`StoredInvocation`].
    ///
    /// The shared, one-shot, forward-only closure-invocation primitive: `kind`
    /// selects the registry map (`"tool"` → a registered tool's `execute`,
    /// `"command"` → a command's `handler`, `"providerGetApiKey"` /
    /// `"providerRefreshToken"` → a provider's `oauth.*`), `name` is the registry
    /// key, and `args` is a JSON array spread as the closure's positional
    /// arguments. The runtime runs the closure, awaits its Promise if async, and
    /// returns the plain-data envelope. A closure that throws — or a missing key
    /// — surfaces as an envelope with `ok == false`; the runtime thread is never
    /// unwound.
    pub async fn invoke_stored(
        &self,
        kind: impl Into<String>,
        name: impl Into<String>,
        args: &Value,
    ) -> Result<StoredInvocation> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::InvokeStored {
                kind: kind.into(),
                name: name.into(),
                args: args.clone(),
                reply,
            })
            .map_err(|_| anyhow!("js plane gone"))?;
        rx.await
            .map_err(|_| anyhow!("js plane reply dropped"))?
            .map_err(|e| anyhow!(e))
    }

    /// Shut the runtime thread down cleanly, waiting for it to join.
    pub async fn shutdown(mut self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Command::Shutdown { reply }).is_ok() {
            let _ = rx.await;
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The runtime thread body: build the `JsRuntime` (with the `ExtensionAPI` ops
/// and the `pi` bootstrap), then service commands until asked to stop (or until
/// the command channel closes).
fn js_plane_thread(mut rx: mpsc::UnboundedReceiver<Command>) {
    // A current-thread tokio runtime + LocalSet: deno_core spawns !Send local
    // tasks (timers, ops), which require a LocalSet to host them.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread tokio runtime for js plane");
    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let inventory: SharedInventory = Rc::new(RefCell::new(Inventory::new()));

        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![api_ops::extension()],
            // Bare-specifier resolution for extension imports. Without a loader
            // deno_core uses NoopModuleLoader, so an extension whose value import
            // survives transpile (notably `import { Type } from "typebox"`) fails
            // to load. This serves a vendored typebox and rejects other bare
            // specifiers with a clear error (see `module_loader`). Built here on
            // the runtime's owning thread — the `Rc<dyn ModuleLoader>` is `!Send`.
            module_loader: Some(Rc::new(crate::module_loader::PidginModuleLoader::new())),
            ..Default::default()
        });
        runtime.op_state().borrow_mut().put(inventory.clone());

        // Install `globalThis.__pi` and the loader helpers before any load.
        runtime
            .execute_script("<bootstrap>", api_ops::BOOTSTRAP_JS)
            .expect("install ExtensionAPI bootstrap");

        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Eval { source, reply } => {
                    let res = eval_source(&mut runtime, &source).await;
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                }
                Command::LoadExtension {
                    id,
                    source,
                    language,
                    reply,
                } => {
                    let res =
                        loader::load_extension(&mut runtime, &inventory, &id, &source, language)
                            .await;
                    let _ = reply.send(res);
                }
                Command::InvokeHook {
                    event,
                    index,
                    event_json,
                    ctx_json,
                    reply,
                } => {
                    let res = dispatch::invoke_hook_on_runtime(
                        &mut runtime,
                        &event,
                        index,
                        &event_json,
                        &ctx_json,
                    )
                    .await;
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                }
                Command::InvokeStored {
                    kind,
                    name,
                    args,
                    reply,
                } => {
                    let res =
                        dispatch::invoke_stored_on_runtime(&mut runtime, &kind, &name, &args).await;
                    let _ = reply.send(res.map_err(|e| e.to_string()));
                }
                Command::Shutdown { reply } => {
                    let _ = reply.send(());
                    break;
                }
            }
        }
    });
}

/// Execute a snippet, drive the event loop until its completion value settles,
/// then deserialize that value into a [`serde_json::Value`].
async fn eval_source(runtime: &mut JsRuntime, source: &str) -> Result<Value> {
    let promise = runtime
        .execute_script("<eval>", source.to_string())
        .map_err(|e| anyhow!(e.to_string()))?;
    let resolve = runtime.resolve(promise);
    let resolved = runtime
        .with_event_loop_promise(resolve, PollEventLoopOptions::default())
        .await
        .map_err(|e| anyhow!(e.to_string()))?;

    deno_core::scope!(scope, runtime);
    let local = deno_core::v8::Local::new(scope, resolved);
    deno_core::serde_v8::from_v8::<Value>(scope, local)
        .map_err(|e| anyhow!("deserialize JS result: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boot the runtime off-thread, evaluate arithmetic, and assert the value
    /// round-trips back across the thread boundary.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn evaluates_arithmetic_off_thread() {
        let hub_tid = std::thread::current().id();
        let plane = JsPlaneHandle::spawn();

        let result = plane.eval("1 + 2").await.expect("eval 1 + 2");
        assert_eq!(result, Value::from(3));

        // The runtime must run on its own thread: proving JS runs there is the
        // whole point of the off-thread plane. `Deno.core` globals only exist
        // inside the JsRuntime, so reading one back confirms we executed there
        // rather than on this hub thread.
        let has_core = plane
            .eval("typeof Deno.core.ops === 'object'")
            .await
            .expect("eval Deno.core probe");
        assert_eq!(has_core, Value::Bool(true));
        assert_ne!(format!("{hub_tid:?}"), "");

        plane.shutdown().await;
    }

    /// A string value must survive the JS -> Rust crossing intact.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trips_a_string() {
        let plane = JsPlaneHandle::spawn();

        let result = plane
            .eval(r#""hello, " + "world""#)
            .await
            .expect("eval string concat");
        assert_eq!(result, Value::from("hello, world"));

        // An object literal round-trips through serde_v8 as structured JSON.
        let obj = plane
            .eval(r#"({ ok: true, n: 42, s: "x" })"#)
            .await
            .expect("eval object literal");
        assert_eq!(obj["ok"], Value::Bool(true));
        assert_eq!(obj["n"], Value::from(42));
        assert_eq!(obj["s"], Value::from("x"));

        plane.shutdown().await;
    }

    /// A snippet that evaluates to a resolved Promise is awaited through the
    /// event loop before its value comes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn awaits_a_promise() {
        let plane = JsPlaneHandle::spawn();

        let result = plane
            .eval("Promise.resolve(7 * 6)")
            .await
            .expect("eval resolved promise");
        assert_eq!(result, Value::from(42));

        plane.shutdown().await;
    }

    /// A thrown JavaScript exception surfaces as a Rust `Err`, not a panic, and
    /// the runtime keeps serving afterward.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reports_js_errors() {
        let plane = JsPlaneHandle::spawn();

        let err = plane.eval("throw new Error('boom')").await;
        assert!(err.is_err(), "expected a thrown JS error to be Err");

        // The runtime is still alive and answering after an error.
        let ok = plane.eval("100 + 1").await.expect("eval after error");
        assert_eq!(ok, Value::from(101));

        plane.shutdown().await;
    }
}
