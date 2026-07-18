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
//! The concrete types here ([`Extension`], [`ExtensionRuntime`]) are the
//! minimal placeholder subset the orchestrator reads (`path`, `resolvedPath`,
//! tool/command/flag names, `sourceInfo`, `hidden`); the full `Extension` /
//! `ExtensionRuntime` shape from pi `extensions/types.ts` (1682 lines) is owned
//! by the extension-plane session and supersedes these when it lands.

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

/// Placeholder for pi's `ExtensionRuntime` (`extensions/types.ts:1648`), the
/// mutable action-callback bag whose actions are throwing stubs until
/// `runner.initialize()` binds a live session. The real runtime is owned by the
/// extension-plane session; this empty marker exists only so the orchestrator
/// can hold and thread `LoadExtensionsResult.runtime` through the seam.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionRuntime {}

/// Port of pi's `createExtensionRuntime()`: seed a fresh (uninitialized)
/// runtime. Placeholder until the extension-plane host lands.
pub fn create_extension_runtime() -> ExtensionRuntime {
    ExtensionRuntime::default()
}

/// A loaded extension. Minimal placeholder subset of pi's `Extension`
/// (`extensions/types.ts`) carrying only the fields the resource-loader
/// orchestrator reads: identity (`path` / `resolved_path`), the registered
/// tool / command / flag names (for conflict detection), provenance
/// (`source_info`), and the `hidden` flag. The full shape is owned by the
/// extension-plane session.
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

/// Port of pi's `LoadExtensionsResult` (`extensions/types.ts:1666`): the loaded
/// extensions, per-path errors, and the shared runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadExtensionsResult {
    /// Successfully loaded extensions, in load order.
    pub extensions: Vec<Extension>,
    /// Per-path load failures.
    pub errors: Vec<ExtensionLoadError>,
    /// The shared runtime (throwing stubs until `runner.initialize()`).
    pub runtime: ExtensionRuntime,
}

/// The seam pi's `loadExtensionsCached` implements. The resource-loader
/// orchestrator holds a `Box<dyn ExtensionLoader>` and calls
/// [`ExtensionLoader::load_extensions_cached`] on every `reload()`.
///
/// Signature mirrors pi's `loadExtensionsCached(paths, cwd, eventBus?,
/// runtime?): Promise<LoadExtensionsResult>` — synchronous here, matching the
/// rest of the atilla port (pi's `async` covers dynamic import, which becomes
/// the host's concern behind this seam). The `runtime` argument threads a
/// pre-seeded runtime through the two-pass trust flow (`loadFinalExtensionSet`).
pub trait ExtensionLoader {
    /// Load the extensions at `paths`, resolving relative paths against `cwd`.
    /// `event_bus` is handed to extension factories; `runtime`, when supplied,
    /// is reused rather than freshly created (the trust second pass).
    fn load_extensions_cached(
        &self,
        paths: &[String],
        cwd: &str,
        event_bus: &EventBus,
        runtime: Option<ExtensionRuntime>,
    ) -> LoadExtensionsResult;
}

/// The no-op [`ExtensionLoader`] used until the extension-plane host lands:
/// returns an empty [`LoadExtensionsResult`] (no extensions, no errors) with a
/// fresh-or-reused runtime. The orchestrator's non-extension assertions pass
/// unchanged against it; the ~6 extension integration tests stay deferred
/// behind this stub.
#[derive(Debug, Clone, Default)]
pub struct StubExtensionLoader;

impl ExtensionLoader for StubExtensionLoader {
    fn load_extensions_cached(
        &self,
        _paths: &[String],
        _cwd: &str,
        _event_bus: &EventBus,
        runtime: Option<ExtensionRuntime>,
    ) -> LoadExtensionsResult {
        LoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            runtime: runtime.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_returns_empty_result_with_fresh_runtime() {
        let loader = StubExtensionLoader;
        let bus = EventBus::new();
        let result = loader.load_extensions_cached(&["/some/ext".to_string()], "/cwd", &bus, None);
        assert!(result.extensions.is_empty());
        assert!(result.errors.is_empty());
        assert_eq!(result.runtime, ExtensionRuntime::default());
    }

    #[test]
    fn stub_reuses_supplied_runtime() {
        let loader = StubExtensionLoader;
        let bus = EventBus::new();
        let runtime = create_extension_runtime();
        let result = loader.load_extensions_cached(&[], "/cwd", &bus, Some(runtime.clone()));
        assert_eq!(result.runtime, runtime);
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
}
