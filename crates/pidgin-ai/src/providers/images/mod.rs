//! Built-in image-generation api-provider registration, mirroring pi-ai's
//! `providers/images/` (`packages/ai/src/providers/images`).
//!
//! Currently just [`register_builtins`], the image analog of the chat
//! [`register_builtin_api_providers`](crate::compat::register_builtin_api_providers).

pub mod register_builtins;
