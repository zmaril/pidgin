//! The Python engine's [`ExtensionLoader`] + [`ExtensionRuntime`] — the offline
//! parallel of the deno `RealExtensionLoader` / `RealExtensionRuntime`.
//!
//! pidgin-coding's `resource_loader_orchestrator` holds a `Box<dyn
//! ExtensionLoader>` and calls `load_extensions_cached(paths, cwd, event_bus,
//! runtime)` on every `reload()`. This module supplies the Python loader: for each
//! already-resolved `.py` path it runs the extension's `extension(pi)` factory
//! under the GIL (see [`load_python_extension`]), collects the resulting
//! [`Inventory`] + live handler store, and maps the tool / command / flag **names**
//! into pidgin-coding's [`Extension`].
//!
//! # `resolved_path` and the trust two-pass
//!
//! Like the deno loader, `resolved_path` is computed the SAME way the
//! orchestrator's `resolveExtensionLoadPath` does — a **logical** `resolve_path(
//! path, cwd, { normalize_unicode_spaces: true })`, NOT a realpath — so the
//! two-pass dedup keys match (see [`logical_resolved_path`]). Paths repeated in one
//! call are deduped by `resolved_path` via a `seen` vec.
//!
//! # Runtime threading
//!
//! The opaque `runtime: Option<Box<dyn ExtensionRuntime>>` handle is threaded
//! through the two passes: a supplied handle is reused identity-preserving and
//! returned unchanged; when `None`, a fresh [`PythonExtensionRuntime`] is minted
//! carrying the just-loaded extensions so the runner factory can share them
//! without re-importing. `event_bus` is accepted-and-ignored (the deno loader does
//! the same — pi's event bus is not part of the offline load path).

// straitjacket-allow-file:duplication -- the per-path load loop, the logical
// `resolved_path` recipe, and the `inventory_to_extension` name-vector mapping are
// transcribed from the deno `resource_loader_impl` on purpose: both engines
// implement the SAME orchestrator seam the SAME way (dedupe by resolved_path, map
// the inventory's names into an Extension, thread the opaque runtime handle), so
// the parallel structure is faithful to the shared seam, not incidental.

use std::any::Any;
use std::sync::Arc;

use pidgin_coding::core::event_bus::EventBus;
use pidgin_coding::core::extensions::loader::{
    Extension, ExtensionLoadError, ExtensionLoader, ExtensionRuntime, LoadExtensionsResult,
};
use pidgin_coding::utils::paths::{resolve_path, PathInputOptions};

use super::engine::{load_python_extension, LoadedPyExtension};
use crate::inventory::Inventory;

/// The Python engine's [`ExtensionRuntime`]: the `(path, Inventory, handlers)`
/// list the loader collected, shared via `Arc` so the runner factory can recover
/// it (through [`ExtensionRuntime::as_any`]) and reuse the already-imported
/// handlers rather than re-running every `extension(pi)` factory.
///
/// Opaque per the seam contract *to the orchestrator*: it only ever *moves* this
/// through the two-pass trust flow. `Send` because `Arc<LoadedPyExtension>` (whose
/// handler store holds `Arc<Py<PyAny>>`, itself `Send + Sync`) is `Send`.
pub struct PythonExtensionRuntime {
    loaded: Vec<Arc<LoadedPyExtension>>,
}

impl PythonExtensionRuntime {
    /// The per-extension loaded state (inventories + live handlers) the loader
    /// captured, for the runner factory to reuse.
    pub(crate) fn loaded(&self) -> &[Arc<LoadedPyExtension>] {
        &self.loaded
    }
}

impl ExtensionRuntime for PythonExtensionRuntime {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The Python engine's `impl ExtensionLoader`. Stateless: each load runs on the
/// embedded (auto-initialized) CPython interpreter under the GIL, so there is no
/// off-thread plane to own (unlike deno's `!Send` `JsRuntime`).
#[derive(Debug, Default, Clone)]
pub struct PythonExtensionLoader;

impl PythonExtensionLoader {
    /// A fresh Python loader.
    pub fn new() -> Self {
        PythonExtensionLoader
    }

    /// Construct the Python loader as a boxed trait object, the Python engine's
    /// own constructor (the analog of the deno `RealExtensionLoader::spawn`).
    pub fn spawn() -> Box<dyn ExtensionLoader> {
        Box::new(PythonExtensionLoader)
    }
}

impl ExtensionLoader for PythonExtensionLoader {
    fn load_extensions_cached(
        &self,
        paths: &[String],
        cwd: &str,
        _event_bus: &EventBus,
        runtime: Option<Box<dyn ExtensionRuntime>>,
    ) -> LoadExtensionsResult {
        let mut extensions: Vec<Extension> = Vec::new();
        let mut errors: Vec<ExtensionLoadError> = Vec::new();
        let mut loaded: Vec<Arc<LoadedPyExtension>> = Vec::new();
        // Dedup within a single call by the logical resolved path.
        let mut seen: Vec<String> = Vec::new();

        for path in paths {
            let resolved_path = logical_resolved_path(path, cwd);
            if seen.contains(&resolved_path) {
                continue;
            }
            seen.push(resolved_path.clone());

            match load_python_extension(path) {
                Ok(extension) => {
                    extensions.push(inventory_to_extension(
                        path.clone(),
                        resolved_path,
                        &extension.inventory,
                    ));
                    loaded.push(Arc::new(extension));
                }
                Err(error) => {
                    errors.push(ExtensionLoadError {
                        path: path.clone(),
                        error: error.to_string(),
                    });
                }
            }
        }

        LoadExtensionsResult {
            extensions,
            errors,
            // Reuse a supplied handle identity-preserving (the trust second pass),
            // else mint a fresh one carrying this call's loaded extensions for the
            // runner factory to share.
            runtime: Some(runtime.unwrap_or_else(|| Box::new(PythonExtensionRuntime { loaded }))),
        }
    }
}

/// Compute an [`Extension`]'s `resolved_path` the SAME way the orchestrator does —
/// a logical `resolve_path(path, cwd, { normalize_unicode_spaces: true })`, NOT a
/// realpath — so the two-pass dedup keys match. Falls back to the input path.
fn logical_resolved_path(path: &str, cwd: &str) -> String {
    let options = PathInputOptions {
        normalize_unicode_spaces: true,
        ..PathInputOptions::default()
    };
    resolve_path(path, cwd, &options).unwrap_or_else(|_| path.to_string())
}

/// Map a loaded [`Inventory`] into pidgin-coding's [`Extension`], keeping the
/// tool / command / flag **names** (via the sanctioned name accessors on the
/// consuming side). `hidden` / `source_info` are left at their defaults; the
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
