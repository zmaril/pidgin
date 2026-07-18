//! Deferred port of pi's `utils/open-browser.ts`.
//!
//! Opening a URL launches a platform-specific helper (`open`, `xdg-open`,
//! `rundll32`) as a subprocess. Deferred until the subprocess-launch utility
//! lands.
