// straitjacket-allow-file[:duplication] — each black-box case is a self-contained
// spawn-assert scenario that deliberately repeats the small temp-dir/agent/project
// setup and the run_cli(...) invocation shape, mirroring pi's per-file vitest cases;
// collapsing that scaffolding would obscure what each case actually exercises.
//! Black-box CLI tests, mirroring pi's four spawn-the-binary test files
//! (`stdout-cleanliness`, `session-id-readonly`, `startup-session-name`,
//! `session-file-invalid`). Each test spawns the built `atilla` binary and
//! asserts the exact stdout / stderr / exit-code / on-disk behavior that pi's
//! vitest suites assert, so the same 15 cases pass against `atilla`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Env var the tests set for the config dir (pi's `ENV_AGENT_DIR`).
const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";

fn atilla_bin() -> &'static str {
    env!("CARGO_BIN_EXE_atilla")
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("atilla-cli-{tag}-{pid}-{n}-{nanos}"));
    fs::create_dir_all(&dir).unwrap();
    // Resolve symlinks (macOS/Linux /tmp): pi's session cwd filtering compares
    // paths textually against the spawned process's cwd.
    fs::canonicalize(&dir).unwrap()
}

struct Output {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

/// Spawn the atilla binary. `env` is a set of extra environment overrides.
fn run_cli(args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(atilla_bin());
    cmd.args(args).current_dir(cwd);
    // Isolate from any ambient pi config so tests are hermetic.
    cmd.env_remove("PI_CODING_AGENT_DIR");
    cmd.env_remove("PI_CODING_AGENT_SESSION_DIR");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to spawn atilla binary");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
    }
}

/// Recursively look for a `.jsonl` session file whose header id matches.
/// Mirrors the test helper `hasSessionWithId`.
fn has_session_with_id(root: &Path, session_id: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if has_session_with_id(&path, session_id) {
                return true;
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            if let Some(first) = content.split('\n').next() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(first) {
                    if v.get("type").and_then(|t| t.as_str()) == Some("session")
                        && v.get("id").and_then(|i| i.as_str()) == Some(session_id)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn iso_now() -> String {
    // Any valid-ish timestamp works; the header only needs type/id/cwd for these tests.
    "2026-01-01T00:00:00.000Z".to_string()
}

fn write_session(session_dir: &Path, cwd: &Path, id: &str) {
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": id,
        "timestamp": iso_now(), "cwd": cwd.to_str().unwrap(),
    });
    fs::write(
        session_dir.join(format!("{id}.jsonl")),
        format!("{}\n", serde_json::to_string(&header).unwrap()),
    )
    .unwrap();
}

fn node_path() -> Option<String> {
    let out = Command::new("sh")
        .args(["-c", "command -v node"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

// =============================================================================
// stdout-cleanliness.test.ts  (5 cases)
// =============================================================================

/// Set up a project with a fake npm command wired into `.pi/settings.json`.
fn setup_stdout_clean(tag: &str) -> (PathBuf, PathBuf) {
    let root = unique_temp_dir(tag);
    let agent = root.join("agent");
    let project = root.join("project");
    let project_cfg = project.join(".pi");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(&project_cfg).unwrap();

    if let Some(node) = node_path() {
        let fake_npm = root.join("fake-npm.mjs");
        fs::write(
            &fake_npm,
            "console.log(\"changed 1 package in 471ms\");\nconsole.log(\"found 0 vulnerabilities\");\nprocess.exit(0);\n",
        )
        .unwrap();
        let settings = serde_json::json!({
            "packages": ["npm:fake-package"],
            "npmCommand": [node, fake_npm.to_str().unwrap()],
        });
        fs::write(
            project_cfg.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();
    }
    (agent, project)
}

#[test]
fn version_goes_to_stdout_with_empty_stderr() {
    let (agent, project) = setup_stdout_clean("version");
    let r = run_cli(
        &["--version"],
        &project,
        &[(ENV_AGENT_DIR, agent.to_str().unwrap())],
    );
    assert_eq!(r.code, Some(0));
    let trimmed = r.stdout.trim();
    // /^\d+\.\d+\.\d+/
    let mut parts = trimmed.split('.');
    let ok = parts
        .next()
        .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty())
        && parts
            .next()
            .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty())
        && parts
            .next()
            .is_some_and(|s| s.chars().take_while(|c| c.is_ascii_digit()).count() >= 1);
    assert!(ok, "version {trimmed:?} must match /^\\d+\\.\\d+\\.\\d+/");
    assert_eq!(r.stderr, "");
}

#[test]
fn plain_help_goes_to_stdout() {
    let (agent, project) = setup_stdout_clean("plain-help");
    let r = run_cli(
        &["--help"],
        &project,
        &[(ENV_AGENT_DIR, agent.to_str().unwrap())],
    );
    assert_eq!(r.code, Some(0));
    assert!(r.stdout.contains("Usage:"), "stdout must contain Usage:");
    assert!(
        !r.stderr.contains("Usage:"),
        "stderr must not contain Usage:"
    );
}

#[test]
fn json_help_keeps_stdout_clean_and_routes_chatter_to_stderr() {
    if node_path().is_none() {
        eprintln!("skipping: node not available for fake npm");
        return;
    }
    let (agent, project) = setup_stdout_clean("json-help");
    let r = run_cli(
        &["--mode", "json", "--help", "--approve"],
        &project,
        &[(ENV_AGENT_DIR, agent.to_str().unwrap())],
    );
    assert_eq!(r.code, Some(0));
    assert_eq!(r.stdout, "", "stdout must be empty in json mode");
    assert!(r.stderr.contains("changed 1 package in 471ms"));
    assert!(r.stderr.contains("found 0 vulnerabilities"));
    assert!(r.stderr.contains("Usage:"));
}

#[test]
fn print_help_keeps_stdout_clean_and_routes_chatter_to_stderr() {
    if node_path().is_none() {
        eprintln!("skipping: node not available for fake npm");
        return;
    }
    let (agent, project) = setup_stdout_clean("print-help");
    let r = run_cli(
        &["-p", "--help", "--approve"],
        &project,
        &[(ENV_AGENT_DIR, agent.to_str().unwrap())],
    );
    assert_eq!(r.code, Some(0));
    assert_eq!(r.stdout, "");
    assert!(r.stderr.contains("changed 1 package in 471ms"));
    assert!(r.stderr.contains("found 0 vulnerabilities"));
    assert!(r.stderr.contains("Usage:"));
}

#[test]
fn untrusted_project_package_installs_are_ignored_for_help() {
    let (agent, project) = setup_stdout_clean("untrusted-help");
    let r = run_cli(
        &["-p", "--help"],
        &project,
        &[(ENV_AGENT_DIR, agent.to_str().unwrap())],
    );
    assert_eq!(r.code, Some(0));
    assert_eq!(r.stdout, "");
    assert!(!r.stderr.contains("changed 1 package in 471ms"));
    assert!(!r.stderr.contains("found 0 vulnerabilities"));
    assert!(r.stderr.contains("Usage:"));
}

// =============================================================================
// session-id-readonly.test.ts  (7 cases)
// =============================================================================

struct SessionDirs {
    agent: PathBuf,
    project: PathBuf,
    sessions: PathBuf,
}

fn setup_session_dirs(tag: &str) -> SessionDirs {
    let root = unique_temp_dir(tag);
    let agent = root.join("agent");
    let project = root.join("project");
    let sessions = root.join("sessions");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(&project).unwrap();
    SessionDirs {
        agent,
        project,
        sessions,
    }
}

fn readonly_env(agent: &Path) -> Vec<(&'static str, String)> {
    vec![
        (ENV_AGENT_DIR, agent.to_str().unwrap().to_string()),
        ("PI_OFFLINE", "1".to_string()),
    ]
}

fn run_readonly(dirs: &SessionDirs, args: &[&str]) -> Output {
    let env = readonly_env(&dirs.agent);
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    run_cli(args, &dirs.project, &env_refs)
}

#[test]
fn does_not_reserve_session_for_help() {
    let dirs = setup_session_dirs("ro-help");
    let r = run_readonly(&dirs, &["--session-id", "read-only-help", "--help"]);
    assert_eq!(r.code, Some(0));
    assert!(!has_session_with_id(
        &dirs.agent.join("sessions"),
        "read-only-help"
    ));
}

#[test]
fn allows_no_session_with_session_id() {
    let dirs = setup_session_dirs("ro-nosession");
    let r = run_readonly(
        &dirs,
        &["--no-session", "--session-id", "ephemeral-id", "--help"],
    );
    assert_eq!(r.code, Some(0));
    assert!(!has_session_with_id(
        &dirs.agent.join("sessions"),
        "ephemeral-id"
    ));
}

#[test]
fn does_not_reserve_session_for_list_models() {
    let dirs = setup_session_dirs("ro-models");
    let r = run_readonly(
        &dirs,
        &["--session-id", "read-only-models", "--list-models"],
    );
    assert_eq!(r.code, Some(0));
    assert!(!has_session_with_id(
        &dirs.agent.join("sessions"),
        "read-only-models"
    ));
}

#[test]
fn warns_when_missing_session_id_creates_new_session() {
    let dirs = setup_session_dirs("ro-missing");
    let r = run_readonly(
        &dirs,
        &[
            "--session-dir",
            dirs.sessions.to_str().unwrap(),
            "--session-id",
            "missing-session-id",
            "--model",
            "missing-model",
            "-p",
            "hi",
        ],
    );
    assert_eq!(r.code, Some(1));
    assert!(r.stderr.contains(
        "Warning: No project session found with id 'missing-session-id'; creating a new session with that id."
    ));
}

#[test]
fn does_not_warn_when_session_id_opens_existing_session() {
    let dirs = setup_session_dirs("ro-existing");
    fs::create_dir_all(&dirs.sessions).unwrap();
    write_session(&dirs.sessions, &dirs.project, "existing-session-id");
    let r = run_readonly(
        &dirs,
        &[
            "--session-dir",
            dirs.sessions.to_str().unwrap(),
            "--session-id",
            "existing-session-id",
            "--model",
            "missing-model",
            "-p",
            "hi",
        ],
    );
    assert_eq!(r.code, Some(1));
    assert!(!r
        .stderr
        .contains("No project session found with id 'existing-session-id'"));
}

#[test]
fn rejects_existing_fork_target_session_id() {
    let dirs = setup_session_dirs("ro-fork");
    fs::create_dir_all(&dirs.sessions).unwrap();
    write_session(&dirs.sessions, &dirs.project, "source-id");
    write_session(&dirs.sessions, &dirs.project, "existing-id");
    let r = run_readonly(
        &dirs,
        &[
            "--session-dir",
            dirs.sessions.to_str().unwrap(),
            "--fork",
            "source-id",
            "--session-id",
            "existing-id",
            "-p",
            "hi",
        ],
    );
    assert_eq!(r.code, Some(1));
    assert!(r
        .stderr
        .contains("Session already exists with id 'existing-id'"));
}

#[test]
fn rejects_invalid_session_ids_without_stack_traces() {
    let dirs = setup_session_dirs("ro-invalid");
    for id in ["-bad", "bad id"] {
        let r = run_readonly(&dirs, &["--session-id", id, "-p", "hi"]);
        assert_eq!(r.code, Some(1), "id {id:?}");
        assert!(
            r.stderr.contains("Session id must be non-empty"),
            "id {id:?}: {}",
            r.stderr
        );
        assert!(
            !r.stderr.contains("SessionManager.create"),
            "id {id:?} leaked a stack frame"
        );
    }
}

// =============================================================================
// startup-session-name.test.ts  (2 cases)
// =============================================================================

fn create_session_file_with_assistant(project: &Path, session_file: &Path) {
    let ts = iso_now();
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": "existing-session",
        "timestamp": ts, "cwd": project.to_str().unwrap(),
    });
    let message = serde_json::json!({
        "type": "message", "id": "assistant-1", "parentId": null, "timestamp": ts,
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "hello"}],
            "provider": "anthropic", "model": "claude-sonnet-4-5", "timestamp": 1_i64,
        },
    });
    fs::write(
        session_file,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&header).unwrap(),
            serde_json::to_string(&message).unwrap()
        ),
    )
    .unwrap();
}

fn read_session_info_names(session_file: &Path) -> Vec<String> {
    fs::read_to_string(session_file)
        .unwrap()
        .trim()
        .split('\n')
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("session_info"))
        .map(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

#[test]
fn sets_name_on_selected_session_before_model_validation() {
    let root = unique_temp_dir("name-set");
    let agent = root.join("agent");
    let project = root.join("project");
    let session_file = root.join("session.jsonl");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(&project).unwrap();
    create_session_file_with_assistant(&project, &session_file);

    let env = readonly_env(&agent);
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = run_cli(
        &[
            "--session",
            session_file.to_str().unwrap(),
            "--name",
            "  CLI Named Session  ",
            "--model",
            "missing-model",
            "-p",
            "hi",
        ],
        &project,
        &env_refs,
    );
    assert_eq!(r.code, Some(1));
    assert_eq!(
        read_session_info_names(&session_file),
        vec!["CLI Named Session".to_string()]
    );
}

#[test]
fn rejects_empty_name_without_appending_metadata() {
    let root = unique_temp_dir("name-empty");
    let agent = root.join("agent");
    let project = root.join("project");
    let session_file = root.join("session.jsonl");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(&project).unwrap();
    create_session_file_with_assistant(&project, &session_file);

    let env = readonly_env(&agent);
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = run_cli(
        &[
            "--session",
            session_file.to_str().unwrap(),
            "--name",
            "   ",
            "--model",
            "missing-model",
            "-p",
            "hi",
        ],
        &project,
        &env_refs,
    );
    assert_eq!(r.code, Some(1));
    assert!(r.stderr.contains("--name requires a non-empty value"));
    assert!(read_session_info_names(&session_file).is_empty());
}

// =============================================================================
// session-file-invalid.test.ts  (1 case)
// =============================================================================

#[test]
fn invalid_session_file_prints_friendly_error_and_preserves_content() {
    let root = unique_temp_dir("invalid-file");
    let agent = root.join("agent");
    let project = root.join("project");
    let session_file = root.join("not-a-session.log");
    fs::create_dir_all(&agent).unwrap();
    fs::create_dir_all(&project).unwrap();
    let original = "{\"type\":\"event\",\"data\":\"not a session\"}\n";
    fs::write(&session_file, original).unwrap();

    let env = readonly_env(&agent);
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = run_cli(
        &["--session", session_file.to_str().unwrap(), "-p", "hi"],
        &project,
        &env_refs,
    );
    assert_eq!(r.code, Some(1));
    assert!(r.stderr.contains(&format!(
        "Error: Session file is not a valid pi session: {}",
        session_file.to_str().unwrap()
    )));
    assert!(!r.stderr.contains("SessionManager.open"));
    assert!(!r.stderr.contains("at "));
    assert_eq!(fs::read_to_string(&session_file).unwrap(), original);
}
