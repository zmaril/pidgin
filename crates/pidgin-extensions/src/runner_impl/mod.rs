//! The concrete [`ExtensionRunner`] seam implementation — [`DenoExtensionRunner`].
//!
//! pidgin-coding defines the `ExtensionRunner` trait (PR0) and ships a no-op
//! `StubExtensionRunner`; this module supplies the *real* deno-backed impl. It
//! wraps the live hook-dispatch engine ([`crate::runner::ExtensionRunner`], which
//! owns the off-thread [`JsPlaneHandle`]) plus the per-extension registration
//! [`Inventory`], and presents the trait's **sync** surface by bridging to the
//! engine's async emitters over a `block_on` driven off any ambient runtime.
//!
//! # Dependency inversion
//!
//! `pidgin-extensions` depends on `pidgin-coding`, never the reverse, so the real
//! `impl ExtensionRunner` lives here and is injected as a `Box<dyn
//! ExtensionRunner>` by the CLI (via [`create_deno_extension_runner`]). This is
//! the same inversion as the `ExtensionLoader` / `ExtensionHost` seams.
//!
//! # Sync-over-async bridge (off any ambient runtime)
//!
//! The trait is synchronous; the JS plane is async and off-thread. Each emit
//! bridges via [`block_on_off_ambient`]: when invoked from inside an ambient
//! tokio runtime (AgentSession is reached through the ambient-tokio RPC/CLI turn
//! commands), the future is driven on a dedicated scoped thread with its own
//! current-thread runtime, so the `block_on` never nests inside the ambient
//! runtime and cannot panic with "cannot start a runtime from within a runtime".
//! This is the `exec-tools-async-vs-sync-agenttool` pattern, the same one
//! `RealExtensionLoader` uses.

// straitjacket-allow-file:duplication -- the sync trait methods each wrap one
// async engine emitter with the same bridge shape (adapt borrows to owned,
// block_on off-ambient, isolate errors to the default); the parallel structure
// mirrors the ported `runner.ts` façade, not incidental repetition.

mod context;
mod queries;

pub use queries::hook_event_from_str;

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pidgin_coding::core::extensions::command::{CommandContext, ResolvedCommand};
use pidgin_coding::core::extensions::discovery::{
    DiscoveredExtension, DiscoveryOrigin, ExtensionLanguage,
};
use pidgin_coding::core::extensions::dispatch::{BeforeAgentStartCombinedResult, ExtensionError};
use pidgin_coding::core::extensions::events::common::{
    AgentMessage, BuildSystemPromptOptions, ImageContent,
};
use pidgin_coding::core::extensions::events::selection::{
    InputEventResult, InputSource, StreamingBehavior,
};
use pidgin_coding::core::extensions::events::session::{
    ResourcesDiscoverReason, ResourcesDiscoverResult, SessionShutdownEvent,
};
use pidgin_coding::core::extensions::events::tool::{
    ToolCallEvent, ToolCallEventResult, ToolResultEvent, ToolResultEventResult,
};
use pidgin_coding::core::extensions::events::turn::MessageEndEvent;
use pidgin_coding::core::extensions::loader::{Extension, ExtensionRuntime};
use pidgin_coding::core::extensions::runner::{
    ExtensionCommandContextHost, ExtensionDispatchEvent, ExtensionEmitOutcome,
    ExtensionErrorListener, ExtensionMode, ExtensionRunner as ExtensionRunnerTrait,
    ExtensionUIContext, FlagValue, ProviderRegistrationHost, RegisteredTool, SessionContextHost,
    SessionControlHost, UnsubscribeFn,
};
use pidgin_coding::core::model_registry::ModelRegistry;
use pidgin_coding::core::session_manager::SessionManager;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::inventory::Inventory;
use crate::resource_loader_impl::RealExtensionRuntime;
use crate::runner::{ContextConfig, ExtensionRunner as InnerRunner, LoadedExtension};
use crate::runtime::JsPlaneHandle;

/// One loaded extension's registration [`Inventory`], keyed by its `path`
/// (provenance for `sourceInfo` and error attribution).
pub(crate) struct ExtensionInventory {
    /// The extension's source path / id (pi's `ext.path`).
    pub path: String,
    /// Everything the extension registered.
    pub inventory: Inventory,
}

/// The registered `onError` listeners, shared between the trait's
/// [`on_error`](ExtensionRunnerTrait::on_error) surface and a forwarding listener
/// installed on the inner engine so isolated dispatch errors reach them too.
#[derive(Default)]
pub(crate) struct ListenerRegistry {
    listeners: Mutex<Vec<(u64, ExtensionErrorListener)>>,
    next_id: AtomicU64,
}

impl ListenerRegistry {
    /// Register a listener, returning its removal id.
    fn add(&self, listener: ExtensionErrorListener) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().unwrap().push((id, listener));
        id
    }

    /// Remove the listener with `id` (its unsubscribe).
    fn remove(&self, id: u64) {
        self.listeners.lock().unwrap().retain(|(lid, _)| *lid != id);
    }

    /// Deliver `error` to every registered listener.
    pub(crate) fn dispatch(&self, error: &ExtensionError) {
        for (_, listener) in self.listeners.lock().unwrap().iter() {
            listener(error);
        }
    }
}

/// Mutable binding state set through `bind_core` / `set_ui_context` /
/// `bind_command_context` (pi threads these into the runner-managed context).
#[derive(Default)]
pub(crate) struct BindingState {
    pub control_host: Option<Arc<dyn SessionControlHost>>,
    pub context_host: Option<Arc<dyn SessionContextHost>>,
    pub provider_host: Option<Arc<dyn ProviderRegistrationHost>>,
    pub command_context_host: Option<Arc<dyn ExtensionCommandContextHost>>,
    pub ui_context: Option<ExtensionUIContext>,
    pub mode: ExtensionMode,
}

/// The deno-backed [`ExtensionRunner`](ExtensionRunnerTrait) implementation.
pub struct DenoExtensionRunner {
    /// The live async hook-dispatch engine (owns the [`JsPlaneHandle`]).
    inner: InnerRunner,
    /// Per-extension registration inventory, for the sync queries.
    inventories: Vec<ExtensionInventory>,
    /// The shared `onError` listener registry.
    listeners: Arc<ListenerRegistry>,
    /// The bindings set through `bind_core` / `set_ui_context` /
    /// `bind_command_context`.
    pub(crate) bindings: Mutex<BindingState>,
    /// The stale message set once by `invalidate` (pi's `staleMessage`).
    stale: Mutex<Option<String>>,
    /// The session manager, when constructed through the host factory.
    #[allow(dead_code)] // Held for future provider-registration wiring (bindCore).
    session_manager: Option<Arc<SessionManager>>,
    /// The model registry, when constructed through the host factory.
    #[allow(dead_code)] // Held for future provider-registration wiring (bindCore).
    model_registry: Option<Arc<ModelRegistry>>,
}

impl DenoExtensionRunner {
    /// Build a runner over a `plane` that already holds the loaded handlers and
    /// the `loaded` per-extension inventories. Used by tests and by
    /// [`create_deno_extension_runner`]. `plane` may be an owned handle or an
    /// `Arc<JsPlaneHandle>` shared with the loader.
    pub fn from_loaded(
        plane: impl Into<Arc<JsPlaneHandle>>,
        loaded: Vec<(String, Inventory)>,
        cwd: impl Into<String>,
    ) -> Self {
        Self::assemble(plane, loaded, cwd.into(), None, None)
    }

    /// The shared assembly path: wire the inner engine, install the forwarding
    /// error listener, and record the inventories. `plane` may be owned (the
    /// self-spawn path) or an `Arc<JsPlaneHandle>` shared with the loader (the
    /// plane-sharing path).
    fn assemble(
        plane: impl Into<Arc<JsPlaneHandle>>,
        loaded: Vec<(String, Inventory)>,
        cwd: String,
        session_manager: Option<Arc<SessionManager>>,
        model_registry: Option<Arc<ModelRegistry>>,
    ) -> Self {
        let extensions: Vec<LoadedExtension> = loaded
            .iter()
            .map(|(path, inventory)| LoadedExtension::new(path.clone(), inventory))
            .collect();
        let inventories: Vec<ExtensionInventory> = loaded
            .into_iter()
            .map(|(path, inventory)| ExtensionInventory { path, inventory })
            .collect();

        let inner = InnerRunner::new(plane, extensions).with_context(ContextConfig::new(cwd));

        let listeners: Arc<ListenerRegistry> = Arc::new(ListenerRegistry::default());
        {
            // Forward every engine-isolated dispatch error to the shared registry.
            let forward = Arc::clone(&listeners);
            inner.on_error(move |error| forward.dispatch(error));
        }

        DenoExtensionRunner {
            inner,
            inventories,
            listeners,
            bindings: Mutex::new(BindingState::default()),
            stale: Mutex::new(None),
            session_manager,
            model_registry,
        }
    }

    /// The per-extension inventories (for the sync-query helpers).
    pub(crate) fn inventories(&self) -> &[ExtensionInventory] {
        &self.inventories
    }
}

/// Drive `future` to completion, **off any ambient tokio runtime**.
///
/// When called from inside an ambient runtime, the future is run on a dedicated
/// scoped thread with its own current-thread runtime so the blocking drive never
/// nests inside the ambient runtime (which would panic). Otherwise it runs on a
/// fresh current-thread runtime directly.
pub(crate) fn block_on_off_ambient<F>(future: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(|| current_thread_block_on(future))
                .join()
                .expect("extension-runner bridge thread panicked")
        })
    } else {
        current_thread_block_on(future)
    }
}

/// Build a throwaway current-thread runtime and block on `future`.
fn current_thread_block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime for extension-runner bridge")
        .block_on(future)
}

impl ExtensionRunnerTrait for DenoExtensionRunner {
    // ---- lifecycle -------------------------------------------------------
    fn emit_session_shutdown(&self, event: SessionShutdownEvent) {
        block_on_off_ambient(self.inner.emit_session_shutdown(&event));
    }

    // ---- generic dispatch ------------------------------------------------
    fn emit(&self, event: &ExtensionDispatchEvent) -> ExtensionEmitOutcome {
        block_on_off_ambient(self.inner.emit_dispatch(event))
    }

    // ---- dedicated emitters ----------------------------------------------
    fn emit_message_end(&self, event: &MessageEndEvent) -> Option<AgentMessage> {
        block_on_off_ambient(self.inner.emit_message_end(event))
    }

    fn emit_input(
        &self,
        text: &str,
        images: Option<&[ImageContent]>,
        source: InputSource,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> InputEventResult {
        let images = images.map(<[ImageContent]>::to_vec);
        block_on_off_ambient(
            self.inner
                .emit_input(text, images, source, streaming_behavior),
        )
        .unwrap_or(InputEventResult::Continue)
    }

    fn emit_before_agent_start(
        &self,
        prompt: &str,
        images: Option<&[ImageContent]>,
        system_prompt: &str,
        system_prompt_options: &BuildSystemPromptOptions,
    ) -> Option<BeforeAgentStartCombinedResult> {
        let images = images.map(<[ImageContent]>::to_vec);
        block_on_off_ambient(self.inner.emit_before_agent_start(
            prompt,
            images,
            system_prompt,
            system_prompt_options.clone(),
        ))
        .unwrap_or(None)
    }

    fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: ResourcesDiscoverReason,
    ) -> ResourcesDiscoverResult {
        block_on_off_ambient(self.inner.emit_resources_discover(cwd, reason))
    }

    fn emit_tool_call(&self, event: &ToolCallEvent) -> Option<ToolCallEventResult> {
        block_on_off_ambient(self.inner.emit_tool_call(event))
    }

    fn emit_tool_result(&self, event: &ToolResultEvent) -> Option<ToolResultEventResult> {
        block_on_off_ambient(self.inner.emit_tool_result(event.clone())).unwrap_or(None)
    }

    // ---- sync queries ----------------------------------------------------
    fn has_handlers(&self, event_type: &str) -> bool {
        self.query_has_handlers(event_type)
    }

    fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        self.query_get_command(name)
    }

    fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        self.query_registered_commands()
    }

    fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        self.query_all_registered_tools()
    }

    fn get_flag_values(&self) -> BTreeMap<String, FlagValue> {
        self.query_flag_values()
    }

    fn create_command_context(&self) -> Box<dyn CommandContext> {
        self.make_command_context()
    }

    // ---- binding / mutation ----------------------------------------------
    fn bind_core(
        &self,
        actions: Arc<dyn SessionControlHost>,
        context_actions: Arc<dyn SessionContextHost>,
        provider_actions: Option<Arc<dyn ProviderRegistrationHost>>,
    ) {
        let mut bindings = self.bindings.lock().unwrap();
        bindings.control_host = Some(actions);
        bindings.context_host = Some(context_actions);
        bindings.provider_host = provider_actions;
    }

    fn set_ui_context(&self, ui_context: Option<ExtensionUIContext>, mode: ExtensionMode) {
        let mut bindings = self.bindings.lock().unwrap();
        bindings.ui_context = ui_context;
        bindings.mode = mode;
    }

    fn bind_command_context(&self, actions: Option<Arc<dyn ExtensionCommandContextHost>>) {
        self.bindings.lock().unwrap().command_context_host = actions;
    }

    fn on_error(&self, listener: ExtensionErrorListener) -> UnsubscribeFn {
        let id = self.listeners.add(listener);
        let listeners = Arc::clone(&self.listeners);
        Box::new(move || listeners.remove(id))
    }

    fn emit_error(&self, error: ExtensionError) {
        self.listeners.dispatch(&error);
    }

    fn invalidate(&self, message: &str) {
        let mut stale = self.stale.lock().unwrap();
        if stale.is_none() {
            *stale = Some(message.to_string());
            // TODO(unit5): forward to the ExtensionRuntime's `invalidate` once the
            // seam exposes a runtime-invalidate primitive (pi's
            // `this.runtime.invalidate(message)`).
        }
    }
}

/// The host-provided factory, mirroring pi's `createExtensionRuntime` shape and
/// the seam contract: build a `Box<dyn ExtensionRunner>` from the already-loaded
/// extensions and the session/model registries.
///
/// The passed `runtime` is the loader's opaque `ExtensionRuntime`. When it is the
/// real deno-backed [`RealExtensionRuntime`] (the production path), this factory
/// recovers it via [`ExtensionRuntime::as_any`] and builds the runner over the
/// loader's **already-live plane**, reusing the inventory the loader collected —
/// so extensions are NOT re-executed on a second plane. When the downcast fails
/// (e.g. a `StubExtensionRuntime` in tests), it falls back to spawning its own
/// [`JsPlaneHandle`] and re-loading each extension from its resolved path.
pub fn create_deno_extension_runner(
    extensions: Vec<Extension>,
    runtime: Box<dyn ExtensionRuntime>,
    cwd: impl Into<String>,
    session_manager: Arc<SessionManager>,
    model_registry: Arc<ModelRegistry>,
) -> Box<dyn ExtensionRunnerTrait> {
    let cwd = cwd.into();

    // Preferred (production): the loader handed us its real runtime. Share its
    // live plane (Arc) and reuse the inventory it already loaded, so no extension
    // factory runs a second time and only one V8 plane exists for the session.
    if let Some(real) = runtime.as_any().downcast_ref::<RealExtensionRuntime>() {
        return Box::new(DenoExtensionRunner::assemble(
            real.shared_plane(),
            real.loaded().to_vec(),
            cwd,
            Some(session_manager),
            Some(model_registry),
        ));
    }

    // Fallback (e.g. a StubExtensionRuntime in tests): spawn our own plane and
    // re-load each extension from its resolved path.
    let plane = JsPlaneHandle::spawn();
    let loaded: Vec<(String, Inventory)> = block_on_off_ambient(async {
        let mut loaded = Vec::new();
        for extension in &extensions {
            let discovered = discovered_from_extension(extension);
            if let Ok(inventory) = plane.load_discovered(&discovered).await {
                loaded.push((extension.path.clone(), inventory));
            }
        }
        loaded
    });

    Box::new(DenoExtensionRunner::assemble(
        plane,
        loaded,
        cwd,
        Some(session_manager),
        Some(model_registry),
    ))
}

/// Synthesize a [`DiscoveredExtension`] for an already-resolved [`Extension`],
/// deriving `id` / `language` / `root` from its `resolved_path` (pi's discovery
/// convention), so the plane can re-run its factory.
fn discovered_from_extension(extension: &Extension) -> DiscoveredExtension {
    let path = if extension.resolved_path.is_empty() {
        &extension.path
    } else {
        &extension.resolved_path
    };
    let entrypoint_path = PathBuf::from(path);
    let language = if entrypoint_path.extension().and_then(|e| e.to_str()) == Some("ts") {
        ExtensionLanguage::TypeScript
    } else {
        ExtensionLanguage::JavaScript
    };
    let root = entrypoint_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let stem = entrypoint_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("extension");
    let id = if stem == "index" {
        entrypoint_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or(stem)
            .to_string()
    } else {
        stem.to_string()
    };
    DiscoveredExtension {
        id,
        root,
        language,
        entrypoint_path,
        origin: DiscoveryOrigin::Configured,
    }
}
