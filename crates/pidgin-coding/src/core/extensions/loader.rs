//! Extension-loader seam for the resource-loader orchestrator.
//!
//! pi's `core/extensions/loader.ts` (721 lines) is a `jiti` dynamic-TypeScript
//! import host: `loadExtensionsCached` / `createExtensionRuntime` execute users'
//! `.ts` extension modules at runtime, statically bundling pi-agent-core,
//! pi-ai, pi-tui, typebox, and pi-coding-agent so those modules can `import`
//! them. Rust cannot execute users' TypeScript, so — per the seam-bridges
//! boundary rule ("JS owns orchestration and dynamic module loading; Rust owns
//! deterministic transforms") — the extension engine is **not** ported to Rust
//! in the same shape. It is owned by the extension-plane session (building a
//! `deno_core` host + napi core); see the `wi3-orchestrator-blockers-ownership`
//! team memory.
//!
//! What lives here is the **trait seam** the orchestrator ([`super::super::resource_loader_orchestrator::DefaultResourceLoader`])
//! calls, plus a [`StubExtensionLoader`] that returns an empty
//! [`LoadExtensionsResult`]. The real host lands later and implements
//! [`ExtensionLoader`]; its signature mirrors pi's `loadExtensionsCached(paths,
//! cwd, eventBus?, runtime?)` so the engine drops in compatibly.
//!
//! # Locked seam contract (extension-plane integration point)
//!
//! This is the negotiated, locked contract the extension-plane session's
//! integration PR drops into. The load-bearing shape decisions:
//!
//! * [`ExtensionRuntime`] is an **opaque marker trait** (`trait ExtensionRuntime
//!   {}`), threaded as `Option<Box<dyn ExtensionRuntime>>`. The orchestrator's
//!   `reload()` only **moves** the handle in and out — it never inspects, clones,
//!   or compares it — so the real runtime (a mutable action-callback bag with
//!   pending provider registrations) can preserve identity across the two-pass
//!   trust flow. Nothing embedding the runtime derives `Clone` / `PartialEq` /
//!   `Eq`.
//! * [`Extension`] keeps its tool / command / flag **name-string** vectors for
//!   now, but all downstream consumption goes through the [`Extension::tool_names`]
//!   / [`Extension::flag_names`] accessors (never the raw `Vec` fields). The
//!   extension-plane's integration PR swaps the fields to rich records and
//!   repoints those accessors, leaving the orchestrator's conflict pass
//!   unchanged.
//! * The sync `load_extensions_cached(&self, paths, cwd, event_bus, runtime)`
//!   signature, the [`LoadExtensionsResult`] `{extensions, errors, runtime}`
//!   shape, [`ExtensionLoadError`] `{path, error}`, and the `Box<dyn
//!   ExtensionLoader>` holding pattern are faithful and unchanged.

// straitjacket-allow-file:duplication

use crate::core::event_bus::EventBus;
use crate::core::source_info::SourceInfo;

/// A single load failure, mirroring pi's `{ path; error }` shape from
/// `LoadExtensionsResult.errors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionLoadError {
    /// The extension path that failed to load.
    pub path: String,
    /// The human-readable failure message.
    pub error: String,
}

/// Opaque marker trait standing in for pi's `ExtensionRuntime`
/// (`extensions/types.ts:1648`), the mutable action-callback bag whose actions
/// are throwing stubs until `runner.initialize()` binds a live session.
///
/// The orchestrator holds this only as an owned `Option<Box<dyn
/// ExtensionRuntime>>` and **moves** it through the two-pass trust flow — it
/// never inspects, clones, or compares it — so the real runtime (owned by the
/// extension-plane session) can preserve object identity across passes.
///
/// The one exposed method is [`ExtensionRuntime::as_any`], a downcast accessor.
/// The seam stays opaque to the orchestrator (which only moves the handle), but
/// the extension-plane host's `ExtensionRunner` factory — which knows the
/// concrete runtime type — can recover it to **share the loader's already-live
/// plane** (reusing the extensions/inventory the loader loaded) instead of
/// spawning a second plane and re-loading every extension from disk.
pub trait ExtensionRuntime {
    /// Recover the concrete runtime for a checked downcast
    /// (`as_any().downcast_ref::<Concrete>()`). The extension-plane factory uses
    /// this to reach the loader's live plane and reuse its loaded inventory;
    /// marker impls simply return `self`.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// The trivial [`ExtensionRuntime`] the [`StubExtensionLoader`] mints. A unit
/// struct with no state; superseded by the extension-plane host's real runtime.
#[derive(Debug, Default)]
pub struct StubExtensionRuntime;

impl ExtensionRuntime for StubExtensionRuntime {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Port of pi's `createExtensionRuntime()`: seed a fresh (uninitialized)
/// runtime handle. Placeholder until the extension-plane host lands.
pub fn create_extension_runtime() -> Box<dyn ExtensionRuntime> {
    Box::new(StubExtensionRuntime)
}

/// A loaded extension. Minimal placeholder subset of pi's `Extension`
/// (`extensions/types.ts`) carrying only the fields the resource-loader
/// orchestrator reads: identity (`path` / `resolved_path`), the registered
/// tool / command / flag names (for conflict detection), provenance
/// (`source_info`), and the `hidden` flag. The full shape is owned by the
/// extension-plane session.
///
/// The `tools` / `commands` / `flags` fields stay name-string vectors for now;
/// downstream consumers must read them only through [`Extension::tool_names`] /
/// [`Extension::flag_names`] so the extension-plane can later swap the fields to
/// rich records (`Map<name, RegisteredTool>`, ...) and repoint the accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    /// The extension's source path (its identity; may be a `<inline:...>` tag).
    pub path: String,
    /// The resolved on-disk path (equals `path` for inline extensions).
    pub resolved_path: String,
    /// Names of tools this extension registers.
    pub tools: Vec<String>,
    /// Names of commands this extension registers.
    pub commands: Vec<String>,
    /// Names of flags this extension registers (without the `--`).
    pub flags: Vec<String>,
    /// Whether the extension is hidden from the model-visible surface.
    pub hidden: bool,
    /// Provenance, stamped by the orchestrator after load.
    pub source_info: Option<SourceInfo>,
}

impl Extension {
    /// The names of the tools this extension registers. This is the **only**
    /// sanctioned way for the orchestrator's conflict pass to read tool names;
    /// the backing storage (`Vec<String>` today, rich records later) is an
    /// implementation detail the extension-plane will change.
    pub fn tool_names(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(String::as_str)
    }

    /// The names of the flags this extension registers (without the `--`). See
    /// [`Extension::tool_names`] for the accessor rationale.
    pub fn flag_names(&self) -> impl Iterator<Item = &str> {
        self.flags.iter().map(String::as_str)
    }
}

/// Port of pi's `LoadExtensionsResult` (`extensions/types.ts:1666`): the loaded
/// extensions, per-path errors, and the shared runtime handle.
///
/// Deliberately **not** `Clone` / `PartialEq` / `Eq`: the `runtime` handle is an
/// opaque, move-only `Option<Box<dyn ExtensionRuntime>>` (see
/// [`ExtensionRuntime`]).
#[derive(Default)]
pub struct LoadExtensionsResult {
    /// Successfully loaded extensions, in load order.
    pub extensions: Vec<Extension>,
    /// Per-path load failures.
    pub errors: Vec<ExtensionLoadError>,
    /// The shared runtime handle (throwing stubs until `runner.initialize()`),
    /// threaded — never inspected — through the two-pass trust flow.
    pub runtime: Option<Box<dyn ExtensionRuntime>>,
}

/// The seam pi's `loadExtensionsCached` implements. The resource-loader
/// orchestrator holds a `Box<dyn ExtensionLoader>` and calls
/// [`ExtensionLoader::load_extensions_cached`] on every `reload()`.
///
/// Signature mirrors pi's `loadExtensionsCached(paths, cwd, eventBus?,
/// runtime?): Promise<LoadExtensionsResult>` — synchronous here, matching the
/// rest of the pidgin port (pi's `async` covers dynamic import, which becomes
/// the host's concern behind this seam). The `runtime` argument threads a
/// pre-seeded runtime handle through the two-pass trust flow
/// (`loadFinalExtensionSet`); when `None`, the loader mints a fresh one.
pub trait ExtensionLoader {
    /// Load the extensions at `paths`, resolving relative paths against `cwd`.
    /// `event_bus` is handed to extension factories; `runtime`, when supplied,
    /// is reused rather than freshly created (the trust second pass), preserving
    /// its identity in the returned result.
    fn load_extensions_cached(
        &self,
        paths: &[String],
        cwd: &str,
        event_bus: &EventBus,
        runtime: Option<Box<dyn ExtensionRuntime>>,
    ) -> LoadExtensionsResult;
}

/// The no-op [`ExtensionLoader`] used until the extension-plane host lands:
/// returns an empty [`LoadExtensionsResult`] (no extensions, no errors) with a
/// fresh-or-reused runtime handle. The orchestrator's non-extension assertions
/// pass unchanged against it; the ~6 extension integration tests stay deferred
/// behind this stub.
#[derive(Debug, Clone, Default)]
pub struct StubExtensionLoader;

impl ExtensionLoader for StubExtensionLoader {
    fn load_extensions_cached(
        &self,
        _paths: &[String],
        _cwd: &str,
        _event_bus: &EventBus,
        runtime: Option<Box<dyn ExtensionRuntime>>,
    ) -> LoadExtensionsResult {
        LoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            // pi's loader defaults `runtime ?? createExtensionRuntime()`, so the
            // result always carries a runtime; a supplied handle is threaded back
            // out unchanged (identity preserved for the trust second pass).
            runtime: Some(runtime.unwrap_or_else(create_extension_runtime)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_empty_result_with_a_runtime() {
        let loader = StubExtensionLoader;
        let bus = EventBus::new();
        let result = loader.load_extensions_cached(&["/some/ext".to_string()], "/cwd", &bus, None);
        assert!(result.extensions.is_empty());
        assert!(result.errors.is_empty());
        // A fresh runtime handle is minted when none is supplied.
        assert!(result.runtime.is_some());
    }

    #[test]
    fn stub_threads_supplied_runtime_back_out() {
        let loader = StubExtensionLoader;
        let bus = EventBus::new();
        let runtime = create_extension_runtime();
        let result = loader.load_extensions_cached(&[], "/cwd", &bus, Some(runtime));
        // The supplied handle is returned (moved through), not dropped.
        assert!(result.runtime.is_some());
        assert!(result.extensions.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn stub_ignores_paths() {
        let loader = StubExtensionLoader;
        let bus = EventBus::new();
        let result = loader.load_extensions_cached(
            &["/a".to_string(), "/b".to_string()],
            "/cwd",
            &bus,
            None,
        );
        assert!(result.extensions.is_empty());
    }

    #[test]
    fn extension_name_accessors_read_the_backing_vecs() {
        let ext = Extension {
            path: "/ext".to_string(),
            resolved_path: "/ext".to_string(),
            tools: vec!["a".to_string(), "b".to_string()],
            commands: vec!["c".to_string()],
            flags: vec!["verbose".to_string()],
            hidden: false,
            source_info: None,
        };
        assert_eq!(ext.tool_names().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(ext.flag_names().collect::<Vec<_>>(), vec!["verbose"]);
    }
}
