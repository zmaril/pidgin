//! The live hook-dispatch engine: `ExtensionRunner`, atilla's port of pi's
//! `ExtensionRunner` (`runner.ts`).
//!
//! PR-E loaded pi's `(pi) => {}` extensions into the off-thread `deno_core`
//! plane and collected each one's registrations into a Rust [`Inventory`]. This
//! module is the runtime successor to that: given the loaded extensions and
//! their registered handlers (which stay live inside the `JsRuntime`, keyed by
//! event name), the [`ExtensionRunner`] dispatches a hook event by calling each
//! registered handler in order — over the [`JsPlaneHandle::invoke_hook`]
//! rendezvous — and applying pi's per-hook result semantics.
//!
//! # Faithful split
//!
//! The dispatch is a faithful port of pi's `emitXxx` methods, factored in two:
//!
//!   * the **shaping** (chain / merge / short-circuit / replace + error
//!     isolation) is the pure `atilla_coding::core::extensions::dispatch` folds,
//!     unit-tested in the default V8-free build;
//!   * the **invocation** (running each handler and awaiting its Promise) is the
//!     [`JsPlaneHandle::invoke_hook`] primitive over the `Affinity::OwnRuntime`
//!     rendezvous.
//!
//! The per-emit driver loops in [`emit`](self), building the JSON event for each
//! handler from the current fold state, invoking the handler, isolating a throw
//! into an [`ExtensionError`] routed to `onError`, and feeding the result to the
//! fold. See [`emit`](emit) for the emitter methods.
//!
//! # Implemented emitters
//!
//! The five the acceptance suite (`extensions-runner.test.ts`,
//! `extensions-input-event.test.ts`) asserts:
//! [`emit_input`](ExtensionRunner::emit_input),
//! [`emit_before_agent_start`](ExtensionRunner::emit_before_agent_start),
//! [`emit_tool_result`](ExtensionRunner::emit_tool_result),
//! [`emit_before_provider_headers`](ExtensionRunner::emit_before_provider_headers),
//! and [`emit_context`](ExtensionRunner::emit_context). The plumbing generalizes
//! to the other hooks (the JS `invokeHook` surface is event-agnostic); their
//! dedicated emitters + shaping are deferred until an acceptance test needs them.

mod context;
mod emit;

pub use context::ContextConfig;

use std::sync::Mutex;

use atilla_coding::core::extensions::dispatch::ExtensionError;
use atilla_coding::core::extensions::hook::HookEvent;

use crate::dispatch::HookInvocation;
use crate::inventory::Inventory;
use crate::runtime::JsPlaneHandle;

/// A loaded extension as the runner sees it: its entrypoint path (provenance for
/// error records) and the hook-event names it registered, in registration order.
///
/// The order matters: the JS-side handler list for an event (`__atilla.registry
/// .hooks.get(event)`) is filled in load-then-registration order, so the runner's
/// per-event handler index lines up with the flattened order of these across all
/// loaded extensions.
pub struct LoadedExtension {
    /// The extension's entrypoint path / id (pi's `ext.path`).
    pub path: String,
    /// The hook-event names this extension registered, in registration order.
    pub hook_events: Vec<String>,
}

impl LoadedExtension {
    /// Build from an entrypoint path and the [`Inventory`] the extension's
    /// factory produced (PR-E's `load_extension_source` return value).
    pub fn new(path: impl Into<String>, inventory: &Inventory) -> Self {
        Self {
            path: path.into(),
            hook_events: inventory.hooks.iter().map(|h| h.event.clone()).collect(),
        }
    }
}

/// A registered `onError` listener (pi's `runner.onError(cb)`).
type ErrorListener = Box<dyn Fn(&ExtensionError) + Send>;

/// The live hook-dispatch engine (pi's `ExtensionRunner`, `runner.ts`).
///
/// Owns the JS plane the handlers live in, the loaded extensions' handler
/// inventory, the minimal ctx configuration, and the `onError` machinery.
pub struct ExtensionRunner {
    plane: JsPlaneHandle,
    extensions: Vec<LoadedExtension>,
    context: ContextConfig,
    errors: Mutex<Vec<ExtensionError>>,
    listeners: Mutex<Vec<ErrorListener>>,
}

impl ExtensionRunner {
    /// Build a runner over `plane` (holding the loaded handlers) and the
    /// `extensions` inventory that describes which handlers are registered.
    pub fn new(plane: JsPlaneHandle, extensions: Vec<LoadedExtension>) -> Self {
        Self {
            plane,
            extensions,
            context: ContextConfig::default(),
            errors: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
        }
    }

    /// Set the minimal ctx configuration threaded into handlers.
    pub fn with_context(mut self, context: ContextConfig) -> Self {
        self.context = context;
        self
    }

    /// The JS plane the handlers live in (for driving the runtime directly, e.g.
    /// reading a `globalThis` effect an `input` handler wrote).
    pub fn plane(&self) -> &JsPlaneHandle {
        &self.plane
    }

    /// Shut the underlying plane down cleanly.
    pub async fn shutdown(self) {
        self.plane.shutdown().await;
    }

    /// Register an `onError` listener (pi's `runner.onError`). Every isolated
    /// handler error is delivered to each listener as it happens.
    pub fn on_error(&self, listener: impl Fn(&ExtensionError) + Send + 'static) {
        self.listeners.lock().unwrap().push(Box::new(listener));
    }

    /// A snapshot of every [`ExtensionError`] isolated so far, in order.
    pub fn errors(&self) -> Vec<ExtensionError> {
        self.errors.lock().unwrap().clone()
    }

    /// Whether any loaded extension registered a handler for `event` (pi's
    /// `runner.hasHandlers(event)`).
    pub fn has_handlers(&self, event: HookEvent) -> bool {
        let name = event.as_str();
        self.extensions
            .iter()
            .any(|ext| ext.hook_events.iter().any(|e| e == name))
    }

    /// The extension path owning each registered handler for `event`, flattened
    /// across all loaded extensions in load-then-registration order. The index
    /// into this vec is exactly the JS-side handler index for `event`.
    fn sites(&self, event: HookEvent) -> Vec<&str> {
        let name = event.as_str();
        let mut sites = Vec::new();
        for ext in &self.extensions {
            for hook_event in &ext.hook_events {
                if hook_event == name {
                    sites.push(ext.path.as_str());
                }
            }
        }
        sites
    }

    /// Isolate a thrown handler into an [`ExtensionError`], deliver it to the
    /// `onError` listeners, and record it — mirroring pi's `emitError`.
    fn record_error(&self, event: &str, extension_path: &str, invocation: HookInvocation) {
        let error = ExtensionError {
            extension_path: extension_path.to_string(),
            event: event.to_string(),
            error: invocation
                .error
                .unwrap_or_else(|| "unknown extension error".to_string()),
            stack: invocation.stack,
        };
        for listener in self.listeners.lock().unwrap().iter() {
            listener(&error);
        }
        self.errors.lock().unwrap().push(error);
    }
}
