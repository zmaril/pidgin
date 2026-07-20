//! Transcript-export tests.
//!
//! `export_to_html`'s only pi coverage is the RPC smoke test
//! `test/rpc.test.ts` ("should export to HTML": the returned path ends in `.html`
//! and the file exists after a turn); its two guard branches come from
//! `exportSessionToHtml`'s thrown messages (`core/export-html/index.ts:244`).
//! `export_to_jsonl` has no dedicated pi test, so its assertions are derived
//! structurally from pi's `exportToJsonl` body (`agent-session.ts:3190`): a
//! `type: "session"` header line followed by the branch entries with `parentId`
//! re-chained into a linear sequence.

// This suite stands up a session directly (persisted and in-memory) from the same
// public constructors the scaffold / offline-echo builders use, so its wiring
// necessarily mirrors theirs.
// straitjacket-allow-file:duplication

use serde_json::{json, Value};

use pidgin_agent::agent::{Agent, AgentOptions};

use crate::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPath};
use crate::core::resource_loader_orchestrator::{
    DefaultResourceLoader, DefaultResourceLoaderOptions,
};
use crate::core::session_manager::SessionManager;
use crate::core::settings_manager::SettingsManager;

use super::super::session::{AgentSession, AgentSessionConfig};
use super::ExportHtmlError;

/// A built session plus the temp dir backing its cwd (kept alive for the test).
struct Fixture {
    session: AgentSession,
    _temp_dir: tempfile::TempDir,
    cwd: String,
}

/// Build a session over a fresh temp cwd, wiring the offline model runtime /
/// resource loader / settings (mirrors the scaffold's `build_session`). When
/// `persisted`, the session manager is file-backed (its file is written on the
/// first assistant append); otherwise it is in-memory.
fn build_fixture(persisted: bool) -> Fixture {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cwd = temp_dir.path().to_string_lossy().to_string();
    let agent_dir = temp_dir.path().join(".agent").to_string_lossy().to_string();

    let session_manager = if persisted {
        let session_dir = temp_dir
            .path()
            .join("sessions")
            .to_string_lossy()
            .to_string();
        SessionManager::create(&cwd, Some(&session_dir), None)
    } else {
        SessionManager::in_memory(&cwd)
    };

    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: ModelsPath::Disabled,
        ..Default::default()
    });
    let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        ..Default::default()
    });

    let session = AgentSession::new(AgentSessionConfig {
        agent: Agent::new(AgentOptions::default()),
        session_manager,
        settings_manager: SettingsManager::create(&cwd, &agent_dir),
        cwd: cwd.clone(),
        scoped_models: Vec::new(),
        resource_loader,
        custom_tools: Vec::new(),
        model_runtime,
        initial_active_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner: None,
        session_start_event: None,
        summarization_models: None,
    });

    Fixture {
        session,
        _temp_dir: temp_dir,
        cwd,
    }
}

/// An in-memory session (no backing file).
fn in_memory_fixture() -> Fixture {
    build_fixture(false)
}

/// A `{ role: "user", content: text }` message value.
fn user_message(text: &str) -> Value {
    json!({ "role": "user", "content": text, "timestamp": 0 })
}

/// A minimal assistant message value (appending one persists the session file).
fn assistant_message(text: &str) -> Value {
    json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "api": "openai-completions",
        "provider": "faux",
        "model": "faux-1",
        "usage": {
            "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0,
            "totalTokens": 2,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
        },
        "stopReason": "stop",
        "timestamp": 0,
    })
}

#[test]
fn export_to_jsonl_writes_header_and_rechained_entries() {
    let fixture = in_memory_fixture();
    fixture
        .session
        .session_manager()
        .append_message(user_message("hello"));
    fixture
        .session
        .session_manager()
        .append_message(assistant_message("hi"));

    let out = format!("{}/export.jsonl", fixture.cwd);
    let path = fixture
        .session
        .export_to_jsonl(Some(&out))
        .expect("jsonl export succeeds");

    let contents = std::fs::read_to_string(&path).expect("read export");
    let lines: Vec<&str> = contents.trim_end().split('\n').collect();
    assert_eq!(lines.len(), 3, "header + two entries");

    // Line 0 is the session header.
    let header: Value = serde_json::from_str(lines[0]).expect("header parses");
    assert_eq!(header["type"], json!("session"));
    assert_eq!(header["version"], json!(3));
    assert_eq!(header["id"], json!(fixture.session.session_id()));
    assert_eq!(header["cwd"], json!(fixture.cwd));

    // The entries are re-chained into a linear parent sequence.
    let first: Value = serde_json::from_str(lines[1]).expect("entry parses");
    let second: Value = serde_json::from_str(lines[2]).expect("entry parses");
    assert_eq!(first["parentId"], Value::Null);
    assert_eq!(second["parentId"], first["id"]);
}

#[test]
fn export_to_jsonl_defaults_to_a_timestamped_filename() {
    let fixture = in_memory_fixture();
    fixture
        .session
        .session_manager()
        .append_message(user_message("hello"));

    let path = fixture
        .session
        .export_to_jsonl(None)
        .expect("jsonl export succeeds");
    assert!(path.ends_with(".jsonl"), "generated a .jsonl path: {path}");
    assert!(std::path::Path::new(&path).exists());
    // Clean up the file written into the process cwd.
    let _ = std::fs::remove_file(&path);
}

#[test]
fn export_to_html_rejects_in_memory_session() {
    let fixture = in_memory_fixture();
    let result = fixture.session.export_to_html(None);
    assert!(matches!(result, Err(ExportHtmlError::InMemorySession)));
}

#[test]
fn export_to_html_rejects_a_session_with_no_written_file() {
    // A persisted session's file is not written until the first assistant message,
    // so exporting before then reports "nothing to export yet".
    let fixture = build_fixture(true);

    // Only a user message: the file is still absent.
    fixture
        .session
        .session_manager()
        .append_message(user_message("hello"));

    let result = fixture.session.export_to_html(None);
    assert!(matches!(result, Err(ExportHtmlError::NothingToExport)));
}

#[test]
fn export_to_html_writes_a_file_for_a_persisted_session() {
    let fixture = build_fixture(true);

    // Appending an assistant message writes the session file to disk.
    fixture
        .session
        .session_manager()
        .append_message(user_message("hello"));
    fixture
        .session
        .session_manager()
        .append_message(assistant_message("hi"));

    let out_str = format!("{}/transcript.html", fixture.cwd);
    let path = fixture
        .session
        .export_to_html(Some(&out_str))
        .expect("html export succeeds");

    assert!(path.ends_with(".html"), "returned an .html path: {path}");
    assert!(std::path::Path::new(&path).exists());
    let html = std::fs::read_to_string(&path).expect("read html");
    assert!(html.contains("<script id=\"session-data\""));
}

#[test]
fn rechain_parent_ids_forms_a_linear_chain() {
    let fixture = in_memory_fixture();
    fixture
        .session
        .session_manager()
        .append_message(user_message("a"));
    fixture
        .session
        .session_manager()
        .append_message(assistant_message("b"));
    fixture
        .session
        .session_manager()
        .append_message(user_message("c"));

    let branch = fixture.session.session_manager().get_branch(None);
    let rechained = super::rechain_parent_ids(&branch);

    assert_eq!(rechained[0].parent_id(), None);
    assert_eq!(rechained[1].parent_id(), Some(branch[0].id()));
    assert_eq!(rechained[2].parent_id(), Some(branch[1].id()));
}
