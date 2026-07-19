//! mirrors pi-coding-agent's interactive mode (`packages/coding-agent/src/modes/interactive`).

pub mod app;
pub mod components;
pub mod routing;
pub mod theme;
pub mod turn;

pub use app::{InteractiveShell, ShellEvent};
