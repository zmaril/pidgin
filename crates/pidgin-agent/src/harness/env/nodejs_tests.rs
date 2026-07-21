//! Tests for [`NodeExecutionEnv`], porting
//! `packages/agent/test/harness/nodejs-env.test.ts`. These exercise the real
//! filesystem and shell against unique temp directories that are cleaned up on
//! drop.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use super::*;
use crate::harness::env::ExecutionEnv;
use crate::harness::utils::shell_output::execute_shell_with_capture;

/// A unique temp directory removed on drop.
struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new() -> Self {
        let base = std::env::temp_dir();
        let dir = base.join(format!("pidgin-nodejs-env-{}", random_token()));
        fs::create_dir_all(&dir).expect("create temp root");
        Self { path: dir }
    }

    fn as_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn env_at(root: &TempRoot) -> NodeExecutionEnv {
    NodeExecutionEnv::new(root.as_str())
}

// ---------------------------------------------------------------------------
// Filesystem behavior
// ---------------------------------------------------------------------------

#[test]
fn reads_writes_lists_and_removes_files_and_directories() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let base = root.as_str();

    assert_eq!(
        env.absolute_path("nested/child", None).unwrap(),
        format!("{base}/nested/child")
    );
    assert_eq!(
        env.join_path(&[&base, "nested", "child"], None).unwrap(),
        format!("{base}/nested/child")
    );
    env.create_dir("nested/child", true, None).unwrap();
    // pi writes a partial word then appends the rest; split as "h" + "ello"
    // to keep the resulting "hello" while avoiding a codespell word token.
    env.write_file("nested/child/file.txt", FileContent::Text("h"), None)
        .unwrap();
    env.append_file("nested/child/file.txt", FileContent::Text("ello"), None)
        .unwrap();
    assert_eq!(
        env.read_text_file("nested/child/file.txt", None).unwrap(),
        "hello"
    );
    assert_eq!(
        env.read_text_lines("nested/child/file.txt", Some(1), None)
            .unwrap(),
        vec!["hello".to_string()]
    );
    assert_eq!(
        env.read_binary_file("nested/child/file.txt", None).unwrap(),
        b"hello".to_vec()
    );

    let entries = env.list_dir("nested/child", None).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "file.txt");
    assert_eq!(entries[0].path, format!("{base}/nested/child/file.txt"));
    assert_eq!(entries[0].kind, FileKind::File);
    assert_eq!(entries[0].size, 5);

    assert!(env.exists("nested/child/file.txt", None).unwrap());
    env.remove("nested/child/file.txt", false, false, None)
        .unwrap();
    assert!(!env.exists("nested/child/file.txt", None).unwrap());
}

#[cfg(unix)]
#[test]
fn returns_file_info_without_following_symlinks() {
    use std::os::unix::fs::symlink;

    let root = TempRoot::new();
    let env = env_at(&root);
    let base = root.as_str();
    env.create_dir("dir", true, None).unwrap();
    env.write_file("dir/file.txt", FileContent::Text("hello"), None)
        .unwrap();
    symlink(format!("{base}/dir/file.txt"), format!("{base}/file-link")).unwrap();
    symlink(format!("{base}/dir"), format!("{base}/dir-link")).unwrap();

    let dir_info = env.file_info("dir", None).unwrap();
    assert_eq!(dir_info.name, "dir");
    assert_eq!(dir_info.path, format!("{base}/dir"));
    assert_eq!(dir_info.kind, FileKind::Directory);

    let file_info = env.file_info("dir/file.txt", None).unwrap();
    assert_eq!(file_info.kind, FileKind::File);
    assert_eq!(file_info.size, 5);

    assert_eq!(
        env.file_info("file-link", None).unwrap().kind,
        FileKind::Symlink
    );
    assert_eq!(
        env.file_info("dir-link", None).unwrap().kind,
        FileKind::Symlink
    );

    let canonical = env.canonical_path("file-link", None).unwrap();
    let expected = fs::canonicalize(format!("{base}/dir/file.txt"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(canonical, expected);
}

#[cfg(unix)]
#[test]
fn lists_symlinks_as_symlinks() {
    use std::os::unix::fs::symlink;

    let root = TempRoot::new();
    let env = env_at(&root);
    let base = root.as_str();
    env.write_file("target.txt", FileContent::Text("hello"), None)
        .unwrap();
    symlink(format!("{base}/target.txt"), format!("{base}/link.txt")).unwrap();

    let mut entries: Vec<(String, FileKind)> = env
        .list_dir(".", None)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.name, entry.kind))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        entries,
        vec![
            ("link.txt".to_string(), FileKind::Symlink),
            ("target.txt".to_string(), FileKind::File),
        ]
    );
}

#[test]
fn stops_reading_text_lines_at_the_requested_limit() {
    let root = TempRoot::new();
    let env = env_at(&root);
    env.write_file("file.txt", FileContent::Text("one\ntwo\nthree"), None)
        .unwrap();
    assert_eq!(
        env.read_text_lines("file.txt", Some(1), None).unwrap(),
        vec!["one".to_string()]
    );
    assert_eq!(
        env.read_text_lines("file.txt", None, None).unwrap(),
        vec!["one".to_string(), "two".to_string(), "three".to_string()]
    );
}

#[test]
fn returns_file_error_for_missing_paths_and_keeps_exists_false() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let base = root.as_str();
    let info = env.file_info("missing.txt", None);
    let error = info.unwrap_err();
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(
        error.path.as_deref(),
        Some(format!("{base}/missing.txt").as_str())
    );
    assert!(!env.exists("missing.txt", None).unwrap());
}

#[test]
fn returns_file_error_for_listing_non_directories() {
    let root = TempRoot::new();
    let env = env_at(&root);
    env.write_file("file.txt", FileContent::Text("hello"), None)
        .unwrap();
    let error = env.list_dir("file.txt", None).unwrap_err();
    assert_eq!(error.code, FileErrorCode::NotDirectory);
}

#[test]
fn appends_to_new_files_and_creates_parent_directories() {
    let root = TempRoot::new();
    let env = env_at(&root);
    env.append_file("new/nested/file.txt", FileContent::Text("a"), None)
        .unwrap();
    env.append_file("new/nested/file.txt", FileContent::Text("b"), None)
        .unwrap();
    assert_eq!(
        env.read_text_file("new/nested/file.txt", None).unwrap(),
        "ab"
    );
}

#[test]
fn creates_temporary_directories_and_files() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let temp_dir = env.create_temp_dir("node-env-test-", None).unwrap();
    assert!(path_exists(&temp_dir));
    assert!(basename(&temp_dir).starts_with("node-env-test-"));
    let temp_file = env.create_temp_file("prefix-", ".txt", None).unwrap();
    assert!(path_exists(&temp_file));
    assert!(temp_file.ends_with(".txt"));
    // clean up the spilled temp entries
    let _ = fs::remove_dir_all(&temp_dir);
    if let Some(parent) = Path::new(&temp_file).parent() {
        let _ = fs::remove_dir_all(parent);
    }
}

#[test]
fn honors_create_dir_recursive_false_and_remove_options() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let create_error = env.create_dir("missing/child", false, None).unwrap_err();
    assert_eq!(create_error.code, FileErrorCode::NotFound);

    env.write_file("dir/child/file.txt", FileContent::Text("hello"), None)
        .unwrap();
    assert!(env.remove("dir", false, false, None).is_err());
    env.remove("dir", true, false, None).unwrap();
    assert!(!env.exists("dir", None).unwrap());

    assert!(env.remove("missing", false, false, None).is_err());
    env.remove("missing", false, true, None).unwrap();
}

#[test]
fn cleanup_is_best_effort() {
    let root = TempRoot::new();
    let env = env_at(&root);
    FileSystem::cleanup(&env);
    Shell::cleanup(&env);
}

// ---------------------------------------------------------------------------
// Shell behavior
// ---------------------------------------------------------------------------

#[test]
fn executes_commands_in_cwd_with_env_overrides() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let mut overrides = BTreeMap::new();
    overrides.insert("NODE_ENV_TEST".to_string(), "ok".to_string());
    let result = env
        .exec(
            "printf '%s:%s' \"$PWD\" \"$NODE_ENV_TEST\"",
            ShellExecOptions {
                env: Some(overrides),
                ..Default::default()
            },
        )
        .unwrap();
    let expected_pwd = env.canonical_path(".", None).unwrap();
    assert_eq!(result.stdout, format!("{expected_pwd}:ok"));
    assert_eq!(result.stderr, "");
    assert_eq!(result.exit_code, 0);
}

#[test]
fn streams_stdout_and_stderr_chunks() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let stdout = Rc::new(RefCell::new(String::new()));
    let stderr = Rc::new(RefCell::new(String::new()));
    let stdout_sink = stdout.clone();
    let stderr_sink = stderr.clone();
    let result = env
        .exec(
            "printf out; printf err >&2",
            ShellExecOptions {
                on_stdout: Some(Box::new(move |chunk: &str| {
                    stdout_sink.borrow_mut().push_str(chunk);
                })),
                on_stderr: Some(Box::new(move |chunk: &str| {
                    stderr_sink.borrow_mut().push_str(chunk);
                })),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.stdout, "out");
    assert_eq!(result.stderr, "err");
    assert_eq!(result.exit_code, 0);
    assert_eq!(*stdout.borrow(), "out");
    assert_eq!(*stderr.borrow(), "err");
}

#[test]
fn returns_non_zero_command_exit_codes_as_successful_results() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let result = env.exec("exit 7", ShellExecOptions::default()).unwrap();
    assert_eq!(result.stdout, "");
    assert_eq!(result.stderr, "");
    assert_eq!(result.exit_code, 7);
}

#[test]
fn returns_timeout_errors_for_commands_exceeding_the_timeout() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let error = env
        .exec(
            "sleep 5",
            ShellExecOptions {
                timeout: Some(0.01),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
}

#[test]
fn returns_shell_unavailable_and_spawn_errors() {
    let root = TempRoot::new();
    let base = root.as_str();

    let missing_shell_env =
        NodeExecutionEnv::new(base.clone()).with_shell_path(format!("{base}/missing-shell"));
    let missing = missing_shell_env
        .exec("printf ok", ShellExecOptions::default())
        .unwrap_err();
    assert_eq!(missing.code, ExecutionErrorCode::ShellUnavailable);

    let shell_path = format!("{base}/not-executable-shell");
    let env = env_at(&root);
    env.write_file(&shell_path, FileContent::Text("not executable"), None)
        .unwrap();
    let spawn_error_env = NodeExecutionEnv::new(base).with_shell_path(shell_path);
    let spawn_error = spawn_error_env
        .exec("printf ok", ShellExecOptions::default())
        .unwrap_err();
    assert_eq!(spawn_error.code, ExecutionErrorCode::SpawnError);
}

#[test]
fn captures_large_shell_output_to_a_full_output_file() {
    let root = TempRoot::new();
    let env = env_at(&root);
    let env_ref: &dyn ExecutionEnv = &env;
    let result = execute_shell_with_capture(env_ref, "yes line | head -n 15000", None).unwrap();
    assert!(result.truncated);
    let full_output_path = result.full_output_path.expect("full output path");
    let full_output = env.read_text_file(&full_output_path, None).unwrap();
    assert!(full_output.split('\n').count() > 10000);
    assert!(result.output.len() < full_output.len());
    if let Some(parent) = Path::new(&full_output_path).parent() {
        let _ = fs::remove_dir_all(parent);
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the internal helpers (branches not reachable end-to-end on a
// POSIX host: WSL stdin transport, timeout validation).
// ---------------------------------------------------------------------------

#[test]
fn resolve_timeout_ms_validates_bounds() {
    assert_eq!(resolve_timeout_ms(None).unwrap(), None);
    assert_eq!(resolve_timeout_ms(Some(1.5)).unwrap(), Some(1500.0));

    let zero = resolve_timeout_ms(Some(0.0)).unwrap_err();
    assert_eq!(zero.code, ExecutionErrorCode::Timeout);
    assert_eq!(
        zero.message,
        "Invalid timeout: must be a finite number of seconds"
    );

    let negative = resolve_timeout_ms(Some(-1.0)).unwrap_err();
    assert_eq!(negative.code, ExecutionErrorCode::Timeout);

    let too_large = resolve_timeout_ms(Some(MAX_TIMEOUT_SECONDS + 1.0)).unwrap_err();
    assert_eq!(too_large.code, ExecutionErrorCode::Timeout);
    assert!(too_large.message.contains("maximum is"));
}

#[test]
fn legacy_wsl_bash_paths_use_stdin_transport() {
    assert!(is_legacy_wsl_bash_path("C:\\Windows\\System32\\bash.exe"));
    assert!(is_legacy_wsl_bash_path("c:/windows/sysnative/bash.exe"));
    assert!(!is_legacy_wsl_bash_path("/bin/bash"));
    assert!(!is_legacy_wsl_bash_path(
        "C:\\Program Files\\Git\\bin\\bash.exe"
    ));

    let wsl = get_bash_shell_config("C:\\Windows\\System32\\bash.exe");
    assert!(wsl.stdin_transport);
    assert_eq!(wsl.args, vec!["-s".to_string()]);

    let normal = get_bash_shell_config("/bin/bash");
    assert!(!normal.stdin_transport);
    assert_eq!(normal.args, vec!["-c".to_string()]);
}

#[test]
fn path_helpers_mirror_node() {
    assert_eq!(resolve_path("/work", "nested/child"), "/work/nested/child");
    assert_eq!(resolve_path("/work", "/abs"), "/abs");
    assert_eq!(resolve_path("/work", "a/../b"), "/work/b");
    assert_eq!(
        join_parts(&["/work", "nested", "child"]),
        "/work/nested/child"
    );
    assert_eq!(join_parts(&["a", "", "b"]), "a/b");
    assert_eq!(basename("/work/nested/child/"), "child");
    assert_eq!(basename("file.txt"), "file.txt");
    assert_eq!(normalize("/a/./b/../c"), "/a/c");
}
