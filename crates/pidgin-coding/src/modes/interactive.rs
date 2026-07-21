//! mirrors pi-coding-agent's interactive mode (`packages/coding-agent/src/modes/interactive`).

pub mod app;
pub mod components;
pub mod extension_ui;
pub mod model_search;
pub mod routing;
pub mod theme;
pub mod turn;

pub use app::{InteractiveShell, ShellEvent, ShellNotifySink};
