//! Coding-agent auth storage, ported from pi's `packages/coding-agent/src/core`
//! at pinned commit `3da591ab`.
//!
//! Three files land here:
//!
//! - [`auth_storage`] — the file-backed [`CredentialStore`] over `auth.json`
//!   ([`auth_storage::AuthStorage`]), its locked storage backends, and the
//!   one-off [`auth_storage::read_stored_credential`] reader.
//! - [`runtime_credentials`] — a non-persistent override overlay
//!   ([`runtime_credentials::RuntimeCredentials`]) wrapping another store.
//! - [`auth_guidance`] — pure login-help message formatters.
//!
//! [`CredentialStore`]: atilla_ai::auth::CredentialStore

pub mod auth_guidance;
pub mod auth_storage;
pub mod runtime_credentials;

// Flat re-exports of the auth-storage surface, so cross-crate consumers (e.g.
// `atilla-orchestrator`) can reach the store, its backends, the backend trait,
// the one-off credential reader, and the default `auth.json` path resolution
// without spelling out the `auth_storage` submodule.
pub use auth_storage::{
    default_auth_path, read_stored_credential, AuthStorage, AuthStorageBackend,
    FileAuthStorageBackend, InMemoryAuthStorageBackend,
};
