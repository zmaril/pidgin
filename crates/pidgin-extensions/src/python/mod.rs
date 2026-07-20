//! The PyO3-backed Python extension engine — the offline sibling of the deno
//! engine.
//!
//! Loads pi-style extensions authored in Python (`def extension(pi):
//! pi.register_command / register_tool / pi.on(...)`) and produces the SAME host
//! records as the deno engine, sharing the engine-neutral [`Inventory`](crate::Inventory)
//! -> `Registry` lowering core. CPython is embedded via PyO3's `auto-initialize`
//! (the `python` Cargo feature), so — unlike deno's V8 — this engine builds and
//! runs offline in every sandbox where libpython is present.
//!
//! * [`api`] — [`PyPiApi`](api::PyPiApi), the `pi` object handed to the factory.
//! * [`convert`] — plain-data marshalling between JSON and native Python objects.
//! * [`engine`] — [`load_python_extension`](engine::load_python_extension): run a
//!   `.py` module and drain its registrations.
//! * [`loader`] — [`PythonExtensionLoader`] / [`PythonExtensionRuntime`], the
//!   `ExtensionLoader` seam.
//! * [`runner`] — [`PythonExtensionRunner`] / [`create_python_extension_runner`],
//!   the `ExtensionRunner` seam (three wired emitters + sanctioned no-op rest).

mod api;
mod convert;
mod engine;
mod loader;
mod runner;

pub use engine::LoadedPyExtension;
pub use loader::{PythonExtensionLoader, PythonExtensionRuntime};
pub use runner::{
    create_python_extension_runner, create_python_extension_runner_from_runtime_ref,
    PythonExtensionRunner,
};
