//! Transcript export, ported from pi's `AgentSession`
//! (`packages/coding-agent/src/core/agent-session.ts:3166-3221`).
//!
//! This slice ports [`AgentSession::export_to_html`] (pi's `exportToHtml`, which
//! delegates to `exportSessionToHtml(sessionManager, state, ...)`) and
//! [`AgentSession::export_to_jsonl`] (pi's `exportToJsonl`).
//!
//! `export_to_jsonl` writes the current session header followed by every entry on
//! the current branch, re-chaining `parentId` so the file is a linear sequence.
//! `export_to_html` renders the transcript to a standalone HTML file via the
//! [`export_html`](crate::core::export_html) renderer.
//!
//! # Deferred renderer collaborators
//!
//! pi's `exportToHtml` resolves a **theme** (via `settingsManager.getTheme()` +
//! `getThemeByName`) and a **custom-tool HTML renderer** (`createToolHtmlRenderer`)
//! before delegating. The concrete tool renderer drives pi-tui `Component`s to
//! ANSI and is **not yet ported** (it depends on `pidgin-tui`, a sibling's crate;
//! see [`export_html::tool_renderer`](crate::core::export_html::tool_renderer)).
//! It only affects **custom** (non-template) tools; sessions whose tools are the
//! template-rendered built-ins (`bash`/`read`/`write`/`edit`/`ls`) export
//! identically without it. This port therefore passes no tool renderer and default
//! theme inputs, matching the existing RPC export path
//! (`modes/rpc/session.rs::export_html`); both collaborators layer on once
//! `pidgin-tui` and the settings→theme-color resolution land.

// straitjacket-allow-file:duplication

use std::path::Path;

use pidgin_agent::types::AgentTool;

use crate::core::export_html;
use crate::core::session_manager::{now_iso, SessionHeader, SessionTag, CURRENT_SESSION_VERSION};
use crate::core::slash_commands::APP_NAME;
use crate::utils::paths::{resolve_path, PathInputOptions};

use super::session::AgentSession;

/// A failure exporting a session to HTML (pi's `exportSessionToHtml` throws,
/// `core/export-html/index.ts:244`). The [`Display`](std::fmt::Display) strings
/// match pi's thrown messages verbatim.
#[derive(Debug)]
pub enum ExportHtmlError {
    /// The session has no backing file (pi: "Cannot export in-memory session to
    /// HTML").
    InMemorySession,
    /// The session file does not exist yet (pi: "Nothing to export yet - start a
    /// conversation first").
    NothingToExport,
    /// The HTML could not be written to disk.
    Io(std::io::Error),
}

impl std::fmt::Display for ExportHtmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportHtmlError::InMemorySession => {
                f.write_str("Cannot export in-memory session to HTML")
            }
            ExportHtmlError::NothingToExport => {
                f.write_str("Nothing to export yet - start a conversation first")
            }
            ExportHtmlError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ExportHtmlError {}

impl AgentSession {
    /// Export the current session branch to a JSONL file (pi's `exportToJsonl`,
    /// `agent-session.ts:3190`).
    ///
    /// Writes the session header followed by all entries on the current branch,
    /// re-chaining each entry's `parentId` to the previous entry's id so the file
    /// is a linear sequence. Returns the resolved output path. When `output_path`
    /// is omitted a timestamped filename is generated in the process working
    /// directory.
    pub fn export_to_jsonl(&self, output_path: Option<&str>) -> std::io::Result<String> {
        let base_dir = std::env::current_dir()
            .map(|dir| dir.to_string_lossy().to_string())
            .unwrap_or_default();
        let default_name = format!("session-{}.jsonl", now_iso().replace([':', '.'], "-"));
        let requested = output_path.unwrap_or(&default_name);
        let file_path = resolve_path(requested, &base_dir, &PathInputOptions::default())
            .unwrap_or_else(|_| requested.to_string());

        if let Some(parent) = Path::new(&file_path).parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let (session_id, cwd, branch) = {
            let manager = self.session_manager();
            (
                manager.get_session_id().to_string(),
                manager.get_cwd().to_string(),
                manager.get_branch(None),
            )
        };

        let header = SessionHeader {
            tag: SessionTag::Session,
            version: Some(CURRENT_SESSION_VERSION),
            id: session_id,
            timestamp: now_iso(),
            cwd,
            parent_session: None,
        };

        let mut lines = vec![serde_json::to_string(&header)?];

        // Re-chain parentIds to form a linear sequence (pi's `{ ...entry, parentId:
        // prevId }`).
        let mut prev_id: Option<String> = None;
        for entry in branch {
            let mut linear = entry.clone();
            linear.set_parent_id(prev_id.clone());
            lines.push(serde_json::to_string(&linear)?);
            prev_id = Some(entry.id().to_string());
        }

        std::fs::write(&file_path, format!("{}\n", lines.join("\n")))?;
        Ok(file_path)
    }

    /// Export the session to a standalone HTML file (pi's `exportToHtml`,
    /// `agent-session.ts:3166`, delegating to `exportSessionToHtml`).
    ///
    /// Fails for an in-memory session or one whose file does not exist yet. When
    /// `output_path` is omitted a default `${APP_NAME}-session-<name>.html`
    /// filename is derived from the session file's basename. Returns the output
    /// path. See the [module docs](self) for the deferred theme / tool-renderer
    /// collaborators.
    pub fn export_to_html(&self, output_path: Option<&str>) -> Result<String, ExportHtmlError> {
        let session_file = self
            .session_file()
            .ok_or(ExportHtmlError::InMemorySession)?;
        if !Path::new(&session_file).exists() {
            return Err(ExportHtmlError::NothingToExport);
        }

        let (header, entries, leaf_id) = {
            let manager = self.session_manager();
            let header: Option<export_html::SessionHeader> =
                manager.get_header().and_then(|header| {
                    serde_json::to_value(header)
                        .ok()
                        .and_then(|value| serde_json::from_value(value).ok())
                });
            (
                header,
                manager.get_entries(),
                manager.get_leaf_id().map(str::to_string),
            )
        };

        // Round-trip storage entries through their JSON wire form into the export
        // renderer's `SessionEntry` union (both mirror pi's shape); entries the
        // export union does not model are skipped, matching the RPC path.
        let export_entries: Vec<export_html::SessionEntry> = entries
            .iter()
            .filter_map(|entry| serde_json::to_value(entry).ok())
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect();

        let tools: Vec<export_html::ToolInfo> = self.agent.tools().iter().map(tool_info).collect();

        let session_data = export_html::assemble_session_data(
            export_html::SessionDataInputs {
                header,
                entries: export_entries,
                leaf_id,
                system_prompt: Some(self.agent.system_prompt()),
                tools: Some(tools),
            },
            // Deferred: the concrete custom-tool HTML renderer (see module docs).
            None,
        );

        let output_path = match output_path {
            Some(path) => normalize_or_passthrough(path),
            None => default_html_path(&session_file),
        };

        let options = export_html::ExportOptions {
            output_path: std::path::PathBuf::from(&output_path),
            // Deferred: settings→theme-color resolution (see module docs).
            theme_inputs: export_html::ThemeInputs {
                resolved_colors: Vec::new(),
                export_colors: export_html::ThemeExportColors::default(),
            },
        };

        let written = export_html::export_session_data_to_html(&session_data, &options)
            .map_err(ExportHtmlError::Io)?;
        Ok(written.to_string_lossy().to_string())
    }
}

/// Map an [`AgentTool`] to the export renderer's [`ToolInfo`](export_html::ToolInfo)
/// (pi's `state.tools.map((t) => ({ name, description, parameters }))`).
fn tool_info(tool: &AgentTool) -> export_html::ToolInfo {
    export_html::ToolInfo {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
    }
}

/// The default HTML output filename derived from the session file's basename (pi's
/// `${APP_NAME}-session-${basename(sessionFile, ".jsonl")}.html`).
fn default_html_path(session_file: &str) -> String {
    let stem = Path::new(session_file)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session");
    let stem = stem.strip_suffix(".jsonl").unwrap_or(stem);
    format!("{APP_NAME}-session-{stem}.html")
}

/// Normalize a caller-supplied output path (pi's `normalizePath(opts.outputPath)`),
/// falling back to the raw value when normalization fails.
fn normalize_or_passthrough(path: &str) -> String {
    crate::utils::paths::normalize_path(path, &PathInputOptions::default())
        .unwrap_or_else(|_| path.to_string())
}

/// Free-function view of [`AgentSession::export_to_jsonl`]'s re-chaining, exposed
/// for unit coverage of the linear-parent rewrite.
#[cfg(test)]
pub(super) fn rechain_parent_ids(
    entries: &[crate::core::session_manager::SessionEntry],
) -> Vec<crate::core::session_manager::SessionEntry> {
    let mut prev_id: Option<String> = None;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut linear = entry.clone();
        linear.set_parent_id(prev_id.clone());
        prev_id = Some(entry.id().to_string());
        out.push(linear);
    }
    out
}

#[cfg(test)]
mod tests;
