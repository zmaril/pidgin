//! Deferred port of pi's `utils/clipboard-image.ts`.
//!
//! Grabbing an image from the clipboard combines Wayland/X11/PowerShell
//! subprocess calls with Photon-based format conversion. Both the process and
//! WASM image dependencies are deferred to a later PR.
