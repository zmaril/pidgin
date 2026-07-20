//! Deferred port of pi's `utils/clipboard.ts`.
//!
//! Reading and writing the system clipboard shells out to platform tools
//! (pbcopy, wl-copy, xclip, PowerShell) and falls back to OSC52 terminal
//! escapes plus a native addon. Those subprocess and terminal dependencies are
//! deferred until the process-execution layer is ported.
