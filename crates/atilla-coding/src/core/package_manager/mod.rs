//! Port of pi-coding-agent's `core/package-manager.ts`, command boundary.
//!
//! pi's `DefaultPackageManager` mixes two concerns: pure filesystem resolution
//! (settings parsing, resource discovery, pattern filtering) and external
//! command execution (npm install/uninstall/view, git fetch/reset/clean/clone,
//! `npm root -g`, `pnpm list -g`). This module ports the *command* concern — the
//! 43-site command-mock cohort of `package-manager.test.ts` — as
//! [`CommandFlowMachine`]s whose planned [`CommandRequest`]s are byte-exact with
//! pi's `runCommand` / `runCommandCapture` / `runCommandSync` argv (and, where
//! pi asserts them, `cwd` / `timeoutMs` / `env`).
//!
//! # Config injection
//!
//! pi reads the package-manager command from `settingsManager.getNpmCommand()`
//! (an argv array such as `["mise", "exec", "node@20", "--", "npm"]`) and pins
//! install roots to `cwd` / `agentDir`. Here [`PackageManagerConfig`] captures
//! the same three inputs (`cwd`, `agent_dir`, `npm_command`) so the planned argv
//! — including any `mise` wrapper prefix and the `--prefix <root>` path — matches
//! pi exactly. Everything the argv depends on is derived from that config the
//! same way pi derives it (`getNpmCommand`, `getPackageManagerName`,
//! `getNpmInstallRoot`, `getNpmInstallArgs`, `getGitDependencyInstallArgs`).
//!
//! # Scope of this port
//!
//! The machines take already-parsed inputs (npm specs/names, install roots, git
//! refs and target dirs, and filesystem facts such as "package.json exists")
//! rather than re-porting pi's `parseSource` / `parseGitUrl` / path-resolution
//! machinery, which is a separate (non-command) concern. Filesystem facts that
//! pi checks inline (`existsSync(packageJson)`) are passed in by the host shim,
//! which owns the filesystem seam; the machines remain pure command planners.

mod config;
mod git;
mod git_update;
mod npm;

pub use config::*;
pub use git::*;
pub use git_update::*;
pub use npm::*;

#[cfg(test)]
mod test_support;
