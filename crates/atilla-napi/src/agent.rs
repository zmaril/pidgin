//! Node-API exports for the agent tier (`crates/atilla-agent`), backing the
//! native `packages/agent/**` shims. Kept in its own module so the agent-tier
//! flips stay merge-clean beside the coding-agent/ai exports in `lib.rs`.
//!
//! As elsewhere in this crate, rich pi structures cross the boundary as JSON
//! strings: the shim `JSON.parse`s the result and re-adds pi's JS default
//! arguments (which the Rust ports drop). The JS-facing types come from pi's own
//! preserved `*.__pi_original__.ts`.

use std::collections::BTreeMap;

use napi::bindgen_prelude::Buffer;
use napi_derive::napi;
use serde::Deserialize;
use serde_json::{json, Value};

// --- agent harness: system-prompt ------------------------------------------
//
// Thin wrapper over `atilla_agent::harness::system_prompt`, backing the native
// `harness/system-prompt.ts` shim. pi's `Skill[]` crosses as a JSON array; the
// `Skill` struct derives `serde` with camelCase field names matching pi's
// `Skill` (`filePath`, optional `disableModelInvocation`).

/// `formatSkillsForSystemPrompt` (harness/system-prompt.ts): render the
/// model-visible `<available_skills>` block from pi's `Skill[]`. The shim
/// serializes the array to JSON; this parses it and returns the string.
#[napi(js_name = "formatSkillsForSystemPrompt")]
pub fn format_skills_for_system_prompt(skills_json: String) -> napi::Result<String> {
    use atilla_agent::harness::skills::Skill;
    let skills: Vec<Skill> = serde_json::from_str(&skills_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid skills array: {e}")))?;
    Ok(atilla_agent::harness::system_prompt::format_skills_for_system_prompt(&skills))
}

// --- agent harness: nodejs execution env -----------------------------------
//
// Stateful handle wrapping the host-backed `atilla_agent::harness::env::
// NodeExecutionEnv`, backing the native `harness/env/nodejs.ts` shim for the
// non-streaming, non-abort call paths (15 of the 20 pi cases). The five cases
// that need pi's async streaming/`AbortSignal`/large-output-capture behaviour
// (which the sync Rust port drops) stay on a private pi-original instance in the
// shim.
//
// Every fallible method crosses its `Result` as a JSON string
// `{"ok":true,"value":...}` or `{"ok":false,"error":{code,message,path?}}`;
// the shim `JSON.parse`s it and rebuilds pi's `Result`/`FileError`/
// `ExecutionError` shapes. `readBinaryFile` is the one exception: raw bytes
// cross as a `Buffer` (never routed through a Rust `String`), and its error is
// thrown as a `napi::Error` whose reason is the same `{code,message,path?}`
// JSON so the shim can reshape it.

use atilla_agent::harness::env::{
    ExecutionError, FileContent, FileError, FileInfo, FileSystem, NodeExecutionEnv, Shell,
    ShellExecOptions,
};

/// JSON shape of pi's `exec` options for the native (non-streaming, non-abort)
/// path. The shim only forwards `cwd`/`env`/`timeout`; `onStdout`/`onStderr`/
/// `abortSignal` route to the pi-original instance instead of here.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ExecOptionsJson {
    cwd: Option<String>,
    env: Option<BTreeMap<String, String>>,
    timeout: Option<f64>,
}

/// Serialize a [`FileInfo`] to pi's `FileInfo` JSON shape.
fn file_info_value(info: &FileInfo) -> Value {
    json!({
        "name": info.name,
        "path": info.path,
        "kind": info.kind.as_str(),
        "size": info.size,
        "mtimeMs": info.mtime_ms,
    })
}

/// Wrap a success value as `{"ok":true,"value":...}`.
fn ok_json(value: Value) -> String {
    json!({ "ok": true, "value": value }).to_string()
}

/// The `{code,message,path?}` object pi rebuilds into a `FileError`.
fn file_error_value(error: &FileError) -> Value {
    json!({ "code": error.code.as_str(), "message": error.message, "path": error.path })
}

/// Wrap a [`FileError`] as `{"ok":false,"error":{...}}`.
fn file_err_json(error: &FileError) -> String {
    json!({ "ok": false, "error": file_error_value(error) }).to_string()
}

/// Wrap an [`ExecutionError`] as `{"ok":false,"error":{...}}`.
fn exec_err_json(error: &ExecutionError) -> String {
    json!({
        "ok": false,
        "error": { "code": error.code.as_str(), "message": error.message },
    })
    .to_string()
}

/// Marshal a `Result<T, FileError>` whose `Ok` maps to `to_value`.
fn file_result_json<T>(result: Result<T, FileError>, to_value: impl FnOnce(T) -> Value) -> String {
    match result {
        Ok(value) => ok_json(to_value(value)),
        Err(error) => file_err_json(&error),
    }
}

/// The Rust-backed host execution environment, exposed to JavaScript as
/// `NodeExecutionEnvCore`. The name avoids colliding with the shim's own
/// `NodeExecutionEnv` class, which composes this handle with a pi-original one.
#[napi(js_name = "NodeExecutionEnvCore")]
pub struct NodeExecutionEnvCore {
    inner: NodeExecutionEnv,
}

#[napi]
impl NodeExecutionEnvCore {
    /// Build a host env rooted at `cwd`, optionally with a custom `shellPath`
    /// and base `shellEnv` (JSON object), mirroring pi's constructor options.
    #[napi(constructor)]
    pub fn new(
        cwd: String,
        shell_path: Option<String>,
        shell_env_json: Option<String>,
    ) -> napi::Result<Self> {
        let mut inner = NodeExecutionEnv::new(cwd);
        if let Some(shell_path) = shell_path {
            inner = inner.with_shell_path(shell_path);
        }
        if let Some(shell_env_json) = shell_env_json {
            let map: BTreeMap<String, String> = serde_json::from_str(&shell_env_json)
                .map_err(|e| napi::Error::from_reason(format!("invalid shellEnv: {e}")))?;
            inner = inner.with_shell_env(map);
        }
        Ok(Self { inner })
    }

    /// pi's `cwd` field.
    #[napi(js_name = "cwd")]
    pub fn cwd(&self) -> String {
        self.inner.cwd()
    }

    /// pi's `absolutePath`.
    #[napi(js_name = "absolutePath")]
    pub fn absolute_path(&self, path: String) -> String {
        file_result_json(self.inner.absolute_path(&path), Value::from)
    }

    /// pi's `joinPath`.
    #[napi(js_name = "joinPath")]
    pub fn join_path(&self, parts: Vec<String>) -> String {
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        file_result_json(self.inner.join_path(&refs), Value::from)
    }

    /// pi's `readTextFile` (non-abort path).
    #[napi(js_name = "readTextFile")]
    pub fn read_text_file(&self, path: String) -> String {
        file_result_json(self.inner.read_text_file(&path), Value::from)
    }

    /// pi's `readTextLines` (non-abort path). `max_lines < 0` means "no limit".
    #[napi(js_name = "readTextLines")]
    pub fn read_text_lines(&self, path: String, max_lines: Option<i64>) -> String {
        let max = max_lines.and_then(|n| if n < 0 { None } else { Some(n as usize) });
        file_result_json(self.inner.read_text_lines(&path, max), Value::from)
    }

    /// pi's `readBinaryFile` (non-abort path). Raw bytes cross as a `Buffer`;
    /// an error is thrown with a `{code,message,path?}` JSON reason.
    #[napi(js_name = "readBinaryFile")]
    pub fn read_binary_file(&self, path: String) -> napi::Result<Buffer> {
        match self.inner.read_binary_file(&path) {
            Ok(bytes) => Ok(Buffer::from(bytes)),
            Err(error) => Err(napi::Error::from_reason(file_error_value(&error).to_string())),
        }
    }

    /// pi's `writeFile` (non-abort path, string content).
    #[napi(js_name = "writeFile")]
    pub fn write_file(&self, path: String, content: String) -> String {
        file_result_json(
            self.inner.write_file(&path, FileContent::Text(&content)),
            |()| Value::Null,
        )
    }

    /// pi's `appendFile`.
    #[napi(js_name = "appendFile")]
    pub fn append_file(&self, path: String, content: String) -> String {
        file_result_json(
            self.inner.append_file(&path, FileContent::Text(&content)),
            |()| Value::Null,
        )
    }

    /// pi's `fileInfo`.
    #[napi(js_name = "fileInfo")]
    pub fn file_info(&self, path: String) -> String {
        file_result_json(self.inner.file_info(&path), |info| file_info_value(&info))
    }

    /// pi's `listDir` (non-abort path).
    #[napi(js_name = "listDir")]
    pub fn list_dir(&self, path: String) -> String {
        file_result_json(self.inner.list_dir(&path), |infos| {
            Value::Array(infos.iter().map(file_info_value).collect())
        })
    }

    /// pi's `canonicalPath`.
    #[napi(js_name = "canonicalPath")]
    pub fn canonical_path(&self, path: String) -> String {
        file_result_json(self.inner.canonical_path(&path), Value::from)
    }

    /// pi's `exists`.
    #[napi(js_name = "exists")]
    pub fn exists(&self, path: String) -> String {
        file_result_json(self.inner.exists(&path), Value::from)
    }

    /// pi's `createDir`.
    #[napi(js_name = "createDir")]
    pub fn create_dir(&self, path: String, recursive: bool) -> String {
        file_result_json(self.inner.create_dir(&path, recursive), |()| Value::Null)
    }

    /// pi's `remove`.
    #[napi(js_name = "remove")]
    pub fn remove(&self, path: String, recursive: bool, force: bool) -> String {
        file_result_json(self.inner.remove(&path, recursive, force), |()| Value::Null)
    }

    /// pi's `createTempDir`.
    #[napi(js_name = "createTempDir")]
    pub fn create_temp_dir(&self, prefix: String) -> String {
        file_result_json(self.inner.create_temp_dir(&prefix), Value::from)
    }

    /// pi's `createTempFile`.
    #[napi(js_name = "createTempFile")]
    pub fn create_temp_file(&self, prefix: String, suffix: String) -> String {
        file_result_json(self.inner.create_temp_file(&prefix, &suffix), Value::from)
    }

    /// pi's `exec` (non-streaming, non-abort path). `options_json` carries only
    /// `cwd`/`env`/`timeout`; the result crosses as `{stdout,stderr,exitCode}`.
    #[napi(js_name = "exec")]
    pub fn exec(&self, command: String, options_json: Option<String>) -> napi::Result<String> {
        let parsed: ExecOptionsJson = match options_json {
            None => ExecOptionsJson::default(),
            Some(json) if json.trim().is_empty() || json == "null" => ExecOptionsJson::default(),
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| napi::Error::from_reason(format!("invalid exec options: {e}")))?,
        };
        let options = ShellExecOptions {
            cwd: parsed.cwd,
            env: parsed.env,
            timeout: parsed.timeout,
            on_stdout: None,
            on_stderr: None,
        };
        Ok(match self.inner.exec(&command, options) {
            Ok(output) => ok_json(json!({
                "stdout": output.stdout,
                "stderr": output.stderr,
                "exitCode": output.exit_code,
            })),
            Err(error) => exec_err_json(&error),
        })
    }
}
