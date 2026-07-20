//! Loading one Python extension: run its module, call `extension(pi)`, and drain
//! the collected [`Inventory`] + [`HandlerStore`].
//!
//! The Python analog of the deno engine's `JsPlaneHandle::load_extension_source`.
//! Where deno transpiles and evaluates an ES module on the off-thread V8 plane,
//! this reads the `.py` source, executes it as a module under
//! [`Python::with_gil`], fetches its `extension` factory, and runs it with a
//! freshly constructed [`PyPiApi`]. The collected records are drained out **inside
//! the same GIL block** so no `Py<PyAny>` clone is needed and only owned Rust data
//! (the [`Inventory`] and the `Arc<Py<PyAny>>` handler handles, both `Send`)
//! survives the block.
//!
//! Import scope is stdlib-only: the module runs in a fresh namespace with no
//! external-dependency resolution (no site-packages injection, no bare-specifier
//! bundling) — matching the deno engine's current no-bare-specifier state. An
//! extension may `import` the Python standard library; anything else raises at
//! load and surfaces as a load error.

use std::ffi::CString;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use pyo3::prelude::*;
use pyo3::types::PyModule;

use super::api::{HandlerStore, PyCollect, PyPiApi};
use crate::inventory::Inventory;

/// One loaded Python extension: its source path, the plain-data [`Inventory`] it
/// registered, and the live [`HandlerStore`] backing that inventory.
pub struct LoadedPyExtension {
    /// The extension's source path (its identity; pi's `ext.path`).
    pub path: String,
    /// Everything the extension registered, as plain data.
    pub inventory: Inventory,
    /// The live Python callables backing the inventory's records.
    pub handlers: HandlerStore,
}

/// Read, execute, and drain the extension at `path`.
///
/// Runs the module under the GIL, calls its `extension(pi)` factory, and returns
/// the collected inventory + handlers. Any Python error (syntax, a missing
/// `extension` attribute, a throwing factory, a non-stdlib import) is mapped to an
/// [`anyhow::Error`] attributed to `path`.
pub fn load_python_extension(path: &str) -> Result<LoadedPyExtension> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading python extension '{path}'"))?;

    let code = CString::new(source)
        .map_err(|_| anyhow!("python extension '{path}' source contains an interior NUL byte"))?;
    let file_name = CString::new(path).unwrap_or_else(|_| CString::new("<extension>").unwrap());
    let module_name = CString::new(module_name_for(path)).unwrap();

    Python::with_gil(|py| -> PyResult<LoadedPyExtension> {
        let collect = std::sync::Arc::new(std::sync::Mutex::new(PyCollect::default()));

        let module = PyModule::from_code(
            py,
            code.as_c_str(),
            file_name.as_c_str(),
            module_name.as_c_str(),
        )?;
        let factory = module.getattr("extension")?;
        let api = Py::new(py, PyPiApi::new(std::sync::Arc::clone(&collect)))?;
        factory.call1((api,))?;

        // Drain inside the GIL block: `std::mem::take` moves the inventory and the
        // handler handles out without any `Py<PyAny>` clone. The `api` object may
        // still reference the (now-emptied) collection; that is fine — it is
        // dropped when the block ends.
        let mut guard = collect.lock().unwrap();
        Ok(LoadedPyExtension {
            path: path.to_string(),
            inventory: std::mem::take(&mut guard.inventory),
            handlers: std::mem::take(&mut guard.handlers),
        })
    })
    .map_err(|error| python_load_error(path, error))
}

/// Format a Python exception into an [`anyhow::Error`] attributed to `path`,
/// including the traceback when the GIL is available.
fn python_load_error(path: &str, error: PyErr) -> anyhow::Error {
    let detail = Python::with_gil(|py| {
        let message = error.value(py).str().ok().map(|s| s.to_string());
        match message {
            Some(message) => message,
            None => error.to_string(),
        }
    });
    anyhow!("python extension '{path}' failed to load: {detail}")
}

/// Derive a Python module `__name__` from the entrypoint path: the file stem, or
/// the parent directory name for an `index.py` entrypoint (mirroring the deno
/// loader's id derivation), sanitized to a valid identifier.
fn module_name_for(path: &str) -> String {
    let path = Path::new(path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("extension");
    let base = if stem == "index" {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or(stem)
    } else {
        stem
    };
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if sanitized.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("ext_{sanitized}")
    } else if sanitized.is_empty() {
        "extension".to_string()
    } else {
        sanitized
    }
}
