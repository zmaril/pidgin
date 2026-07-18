//! Mirror of pi-coding-agent's `core/tools` directory
//! (`packages/coding-agent/src/core/tools`).
//!
//! This module hosts the pure / mechanical tool algorithms ported from pi:
//! output truncation, path expansion, render helpers, streaming output
//! accounting, edit-diff computation, and the pure formatting/search layers of
//! the read, edit, grep, and find tools. Modules whose behavior is entirely
//! filesystem-, subprocess-, or TUI-bound are present as documented
//! placeholders until the surrounding agent runtime is ported.

pub mod bash;
pub mod definitions;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod find;
pub mod grep;
pub mod index;
pub mod ls;
pub mod output_accumulator;
pub mod path_utils;
pub mod read;
pub mod render_utils;
#[cfg(test)]
mod test_support;
pub mod tool_definition_wrapper;
pub mod truncate;
pub mod write;
