//! The real [`ExtensionLoader`] implementation — the capstone that wires the
//! `deno_core` extension plane into pi's `DefaultResourceLoader.reload()`.
//!
//! atilla-coding's `resource_loader_orchestrator` holds a `Box<dyn
//! ExtensionLoader>` (defaulting to the no-op `StubExtensionLoader`) and calls
//! `load_extensions_cached(paths, cwd, event_bus, runtime)` on every `reload()`.
//! This module supplies the *real* loader: a [`RealExtensionLoader`] that owns a
//! persistent [`JsPlaneHandle`] (the off-thread `deno_core` runtime) and, for
//! each already-resolved extension path the orchestrator hands it, runs the
//! extension's default-export factory on the plane, collects the resulting
//! [`Inventory`], and maps the registered tool / command / flag **names** into
//! atilla-coding's [`Extension`].
//!
//! # Dependency inversion (why this lives in atilla-extensions)
//!
//! `atilla-extensions` depends on `atilla-coding` (for the seam types, the
//! discovery records, and the `ExtensionHost` registration surface); atilla-coding
//! must **never** depend on atilla-extensions. So the real `impl ExtensionLoader`
//! cannot live in atilla-coding — it lives here and is injected into `reload()`
//! through the existing `DefaultResourceLoaderOptions.extension_loader` seam (by a
//! test today, by the CLI in a future deno-gated wiring PR). This is the same
//! inversion as `ExtensionHost`.
//!
//! # Sync-over-async
//!
//! The `ExtensionLoader` trait is **synchronous**, but the plane is async and
//! lives off-thread. So the loader owns a Tokio runtime and `block_on`s the
//! async load calls — the `block_on` sync↔async bridge the seam contract
//! anticipates. The actual JS execution happens on the plane's own dedicated
//! thread; this side only sends work over a channel and awaits the reply.
//!
//! # `resolved_path` and the trust two-pass
//!
//! The orchestrator's `loadFinalExtensionSet` two-pass dedup ("load each
//! extension exactly once per reload") keys on `Extension.resolved_path`, which
//! it recomputes from each input path via `resolveExtensionLoadPath` =
//! `resolve_path(path, cwd, { normalize_unicode_spaces: true })` — a **logical**
//! resolve, NOT a realpath/canonicalize. So this loader sets `resolved_path` the
//! **same** way (see [`logical_resolved_path`]); if it used a realpath the pass-2
//! reuse dedup would mismatch and extensions would double-load.
//!
//! The opaque `runtime: Option<Box<dyn ExtensionRuntime>>` handle is threaded
//! through the two passes: when the pre-trust pass supplies one, it is **reused
//! identity-preserving** and returned unchanged; when `None`, a fresh handle
//! backed by the persistent plane is minted. The loader additionally dedups the
//! paths within a single call by `resolved_path`, so a path repeated in one call
//! loads only once.

// straitjacket-allow-file:duplication -- the per-path load loop mirrors pi's
// `loadExtensionsInternal` (iterate resolved paths, run each factory, collect an
// Extension or an error) and the runtime-threading mirrors the seam's stub; the
// parallel structure is faithful to the ported source, not incidental.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use atilla_coding::core::event_bus::EventBus;
use atilla_coding::core::extensions::discovery::{
    DiscoveredExtension, DiscoveryOrigin, ExtensionLanguage,
};
use atilla_coding::core::extensions::loader::{
    Extension, ExtensionLoadError, ExtensionLoader, ExtensionRuntime, LoadExtensionsResult,
};
use atilla_coding::utils::paths::{resolve_path, PathInputOptions};

use crate::inventory::Inventory;
use crate::runtime::JsPlaneHandle;

/// The real [`ExtensionRuntime`], backed by the persistent [`JsPlaneHandle`].
///
/// Opaque per the seam contract *to the orchestrator*: it only ever *moves* this
/// through the two-pass trust flow, never inspecting, cloning, or comparing it.
/// It shares the loader's plane (`Arc<JsPlaneHandle>`) so the returned handle
/// fronts the same live runtime the extensions loaded into, and it carries the
/// per-extension [`Inventory`] the loader collected while loading them.
///
/// The extension-plane host's `ExtensionRunner` factory recovers this concrete
/// via [`ExtensionRuntime::as_any`] to build the runner over the **same** plane
/// and reuse this inventory — so extensions are not re-executed on a second
/// plane.
pub struct RealExtensionRuntime {
    plane: Arc<JsPlaneHandle>,
    /// The `(path, Inventory)` pairs the loader collected while loading each
    /// extension onto `plane`. Captured at mint so the runner factory can reuse
    /// them without re-running the extension factories.
    loaded: Vec<(String, Inventory)>,
}

impl ExtensionRuntime for RealExtensionRuntime {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl RealExtensionRuntime {
    /// A cloned handle to the shared live plane the extensions loaded into.
    pub(crate) fn shared_plane(&self) -> Arc<JsPlaneHandle> {
        Arc::clone(&self.plane)
    }

    /// The per-extension inventory the loader captured for this plane.
    pub(crate) fn loaded(&self) -> &[(String, Inventory)] {
        &self.loaded
    }
}

/// The real `impl ExtensionLoader`: a persistent `deno_core` plane plus a Tokio
/// runtime to `block_on` the async loads behind the sync trait.
pub struct RealExtensionLoader {
    plane: Arc<JsPlaneHandle>,
    rt: tokio::runtime::Runtime,
}

impl RealExtensionLoader {
    /// Spawn the off-thread `deno_core` plane and the Tokio runtime that drives
    /// it. The plane's `!Send` `JsRuntime` is constructed on its own dedicated
    /// thread; this runtime only submits work over the channel and awaits.
    pub fn spawn() -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime for extension loader");
        RealExtensionLoader {
            plane: Arc::new(JsPlaneHandle::spawn()),
            rt,
        }
    }

    /// Mint a fresh runtime handle backed by the persistent plane — pi's
    /// `createExtensionRuntime()` equivalent — carrying the inventory just
    /// loaded onto that plane so the runner factory can share both.
    fn make_runtime(&self, loaded: Vec<(String, Inventory)>) -> Box<dyn ExtensionRuntime> {
        Box::new(RealExtensionRuntime {
            plane: Arc::clone(&self.plane),
            loaded,
        })
    }
}

impl ExtensionLoader for RealExtensionLoader {
    fn load_extensions_cached(
        &self,
        paths: &[String],
        cwd: &str,
        _event_bus: &EventBus,
        runtime: Option<Box<dyn ExtensionRuntime>>,
    ) -> LoadExtensionsResult {
        let mut extensions: Vec<Extension> = Vec::new();
        let mut errors: Vec<ExtensionLoadError> = Vec::new();
        // The per-extension inventory loaded onto the plane in this call, handed
        // to a freshly minted runtime so the runner factory can reuse it.
        let mut loaded: Vec<(String, Inventory)> = Vec::new();
        // Dedup within a single call by the logical resolved path, so a path
        // repeated in one call loads its factory exactly once.
        let mut seen: Vec<String> = Vec::new();

        self.rt.block_on(async {
            for path in paths {
                let resolved_path = logical_resolved_path(path, cwd);
                if seen.contains(&resolved_path) {
                    continue;
                }
                seen.push(resolved_path.clone());

                let discovered = discovered_from_path(path);
                match self.plane.load_discovered(&discovered).await {
                    Ok(inventory) => {
                        extensions.push(inventory_to_extension(
                            path.clone(),
                            resolved_path,
                            &inventory,
                        ));
                        loaded.push((path.clone(), inventory));
                    }
                    Err(error) => {
                        errors.push(ExtensionLoadError {
                            path: path.clone(),
                            error: error.to_string(),
                        });
                    }
                }
            }
        });

        LoadExtensionsResult {
            extensions,
            errors,
            // pi's loader defaults `runtime ?? createExtensionRuntime()`: reuse a
            // supplied handle identity-preserving (the trust second pass), else
            // mint a fresh one backed by the persistent plane, carrying this
            // call's loaded inventory for the runner factory to share.
            runtime: Some(runtime.unwrap_or_else(|| self.make_runtime(loaded))),
        }
    }
}

/// Compute an [`Extension`]'s `resolved_path` the SAME way the orchestrator's
/// `resolveExtensionLoadPath` does — a logical `resolve_path(path, cwd,
/// { normalize_unicode_spaces: true })`, NOT a realpath — so the two-pass dedup
/// keys match. Falls back to the input path if resolution fails.
fn logical_resolved_path(path: &str, cwd: &str) -> String {
    let options = PathInputOptions {
        normalize_unicode_spaces: true,
        ..PathInputOptions::default()
    };
    resolve_path(path, cwd, &options).unwrap_or_else(|_| path.to_string())
}

/// Synthesize a [`DiscoveredExtension`] for an already-resolved entrypoint path,
/// deriving `id` / `language` / `root` from the path (pi's discovery convention):
/// the language from the suffix, the id from the file stem — or, for an
/// `index.{ts,js}` entrypoint, the containing directory's name (so subdirectory
/// extensions get a distinct module identity rather than the generic `index`).
fn discovered_from_path(path: &str) -> DiscoveredExtension {
    let entrypoint_path = PathBuf::from(path);
    let language = if path_has_ts_suffix(&entrypoint_path) {
        ExtensionLanguage::TypeScript
    } else {
        ExtensionLanguage::JavaScript
    };
    let root = entrypoint_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let id = derive_id(&entrypoint_path);
    DiscoveredExtension {
        id,
        root,
        language,
        entrypoint_path,
        origin: DiscoveryOrigin::Configured,
    }
}

/// Whether the entrypoint is a `.ts` file (else it is treated as JavaScript,
/// mirroring pi's suffix inference).
fn path_has_ts_suffix(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("ts")
}

/// Derive the stable module id: the file stem, or the parent directory name for
/// an `index.{ts,js}` entrypoint.
fn derive_id(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("extension");
    if stem == "index" {
        if let Some(dir) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
        {
            return dir.to_string();
        }
    }
    stem.to_string()
}

/// Map a loaded [`Inventory`] into atilla-coding's [`Extension`], keeping the
/// tool / command / flag **names** (record-enrichment is out of scope for the
/// integration bar). `hidden` / `source_info` are left at their defaults; the
/// orchestrator stamps `source_info` after load.
fn inventory_to_extension(path: String, resolved_path: String, inventory: &Inventory) -> Extension {
    Extension {
        path,
        resolved_path,
        tools: inventory.tools.iter().map(|t| t.name.clone()).collect(),
        commands: inventory.commands.iter().map(|c| c.name.clone()).collect(),
        flags: inventory.flags.iter().map(|f| f.name.clone()).collect(),
        hidden: false,
        source_info: None,
    }
}
