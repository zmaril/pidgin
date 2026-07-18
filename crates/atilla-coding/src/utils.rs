//! Rust mirror of pi-coding-agent's `utils` module
//! (`packages/coding-agent/src/utils`).
//!
//! This subtree ports the pure, dependency-light utilities from pi. Modules
//! that require an async runtime, native addons, subprocess execution, HTTP,
//! or WASM image processing are represented by documented placeholder modules
//! and will be ported in later PRs.
//!
//! Ported modules: [`ansi`], [`html`], [`json`], [`pi_user_agent`], [`mime`],
//! [`paths`], [`changelog`], [`version_check`], [`git_url`], [`frontmatter`],
//! [`deprecation`], [`exif`].
//!
//! Deferred modules: [`sleep`], [`clipboard`], [`clipboard_native`],
//! [`clipboard_image`], [`child_process`], [`open_browser`], [`shell`],
//! [`fs_watch`], [`tools_manager`], [`windows_self_update`], [`image`],
//! [`syntax_highlight`].

// Ported modules.
pub mod ansi;
pub mod changelog;
pub mod deprecation;
pub mod exif;
pub mod frontmatter;
pub mod git_url;
pub mod html;
pub mod json;
pub mod mime;
pub mod paths;
pub mod pi_user_agent;
pub mod version_check;

// Deferred modules (documented placeholders).
pub mod child_process;
pub mod clipboard;
pub mod clipboard_image;
pub mod clipboard_native;
pub mod fs_watch;
pub mod image;
pub mod open_browser;
pub mod shell;
pub mod sleep;
pub mod syntax_highlight;
pub mod tools_manager;
pub mod windows_self_update;
