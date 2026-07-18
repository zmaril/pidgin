//! Mirror of pi-coding-agent's `core` module (`packages/coding-agent/src/core`).
//!
//! The `tools` subtree and the config/settings cluster are ported so far;
//! remaining sibling submodules land later.

pub mod command_flow;
pub mod defaults;
pub mod experimental;
pub mod export_html;
pub mod package_manager;
pub mod project_trust;
pub mod radius;
pub mod resolve_config_value;
pub mod tools;
pub mod trust_manager;

#[cfg(test)]
mod test_support;
