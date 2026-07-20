//! Node-API exports for the agent tier (`crates/pidgin-agent`), backing the
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

/// Serialize a slice of `serde`-able items (e.g. loader diagnostics) to a
/// `Vec<Value>`, mapping any per-item serialization failure to `Value::Null`.
/// Shared by the `loadSkills` / `loadPromptTemplates` loaders below, whose
/// diagnostics arrays are otherwise identical.
fn to_json_values<T: serde::Serialize>(items: &[T]) -> Vec<Value> {
    items
        .iter()
        .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
        .collect()
}

// --- agent harness: system-prompt ------------------------------------------
//
// Thin wrapper over `pidgin_agent::harness::system_prompt`, backing the native
// `harness/system-prompt.ts` shim. pi's `Skill[]` crosses as a JSON array; the
// `Skill` struct derives `serde` with camelCase field names matching pi's
// `Skill` (`filePath`, optional `disableModelInvocation`).

/// `formatSkillsForSystemPrompt` (harness/system-prompt.ts): render the
/// model-visible `<available_skills>` block from pi's `Skill[]`. The shim
/// serializes the array to JSON; this parses it and returns the string.
#[napi(js_name = "formatSkillsForSystemPrompt")]
pub fn format_skills_for_system_prompt(skills_json: String) -> napi::Result<String> {
    use pidgin_agent::harness::skills::Skill;
    let skills: Vec<Skill> = serde_json::from_str(&skills_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid skills array: {e}")))?;
    Ok(pidgin_agent::harness::system_prompt::format_skills_for_system_prompt(&skills))
}

// --- agent harness: skills / prompt-templates formatting --------------------
//
// Pure, synchronous formatters ported to `pidgin_agent::harness::skills` and
// `pidgin_agent::harness::prompt_templates`, backing the native `harness/
// skills.ts` and `harness/prompt-templates.ts` shims. pi's rich `Skill` /
// `PromptTemplate` cross as JSON strings (serialized in the shim, parsed with
// serde here); the string result is returned unchanged. The stateful loaders
// (`loadSkills`/`loadPromptTemplates`) are methods on `NodeExecutionEnvCore`
// below, so they can borrow the same host-backed Rust env the shim already
// holds for `nodejs.ts`.

/// Serialize a [`Skill`](pidgin_agent::harness::skills::Skill) to pi's `Skill`
/// JSON shape, always emitting `disableModelInvocation` (pi's loader tests
/// compare against an explicit `false`, but the Rust serde derive skips the
/// default, so it is written unconditionally here).
fn skill_to_value(skill: &pidgin_agent::harness::skills::Skill) -> Value {
    json!({
        "name": skill.name,
        "description": skill.description,
        "content": skill.content,
        "filePath": skill.file_path,
        "disableModelInvocation": skill.disable_model_invocation,
    })
}

/// Serialize a [`PromptTemplate`](pidgin_agent::harness::prompt_templates::PromptTemplate)
/// to pi's `PromptTemplate` JSON shape. The Rust loader always sets
/// `description` (falling back to the empty string), matching pi.
fn prompt_template_to_value(
    template: &pidgin_agent::harness::prompt_templates::PromptTemplate,
) -> Value {
    json!({
        "name": template.name,
        "description": template.description,
        "content": template.content,
    })
}

/// `formatSkillInvocation` (harness/skills.ts): render a `<skill>` invocation
/// block, optionally appending user instructions. pi's `Skill` crosses as a
/// JSON object; `additionalInstructions` is an optional string.
#[napi(js_name = "formatSkillInvocation")]
pub fn format_skill_invocation(
    skill_json: String,
    additional_instructions: Option<String>,
) -> napi::Result<String> {
    use pidgin_agent::harness::skills::{format_skill_invocation, Skill};
    let skill: Skill = serde_json::from_str(&skill_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid skill: {e}")))?;
    Ok(format_skill_invocation(
        &skill,
        additional_instructions.as_deref(),
    ))
}

/// `parseCommandArgs` (harness/prompt-templates.ts): split an argument string
/// using simple shell-style single and double quotes.
#[napi(js_name = "parseCommandArgs")]
pub fn parse_command_args(args_string: String) -> Vec<String> {
    pidgin_agent::harness::prompt_templates::parse_command_args(&args_string)
}

/// `substituteArgs` (harness/prompt-templates.ts): substitute prompt-template
/// placeholders (`$1`, `$@`, `$ARGUMENTS`, `${@:N}`, `${@:N:L}`) with args.
#[napi(js_name = "substituteArgs")]
pub fn substitute_args(content: String, args: Vec<String>) -> String {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    pidgin_agent::harness::prompt_templates::substitute_args(&content, &refs)
}

/// `formatPromptTemplateInvocation` (harness/prompt-templates.ts): substitute
/// positional arguments into a template's content. pi's `PromptTemplate`
/// crosses as a JSON object; the argument list crosses as a string array.
#[napi(js_name = "formatPromptTemplateInvocation")]
pub fn format_prompt_template_invocation(
    template_json: String,
    args: Vec<String>,
) -> napi::Result<String> {
    use pidgin_agent::harness::prompt_templates::{
        format_prompt_template_invocation, PromptTemplate,
    };
    let template: PromptTemplate = serde_json::from_str(&template_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid prompt template: {e}")))?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(format_prompt_template_invocation(&template, &refs))
}

// --- agent harness: nodejs execution env -----------------------------------
//
// Stateful handle wrapping the host-backed `pidgin_agent::harness::env::
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

use pidgin_agent::harness::env::{
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
        file_result_json(self.inner.absolute_path(&path, None), Value::from)
    }

    /// pi's `joinPath`.
    #[napi(js_name = "joinPath")]
    pub fn join_path(&self, parts: Vec<String>) -> String {
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        file_result_json(self.inner.join_path(&refs, None), Value::from)
    }

    /// pi's `readTextFile` (non-abort path).
    #[napi(js_name = "readTextFile")]
    pub fn read_text_file(&self, path: String) -> String {
        file_result_json(self.inner.read_text_file(&path, None), Value::from)
    }

    /// pi's `readTextLines` (non-abort path). `max_lines < 0` means "no limit".
    #[napi(js_name = "readTextLines")]
    pub fn read_text_lines(&self, path: String, max_lines: Option<i64>) -> String {
        let max = max_lines.and_then(|n| if n < 0 { None } else { Some(n as usize) });
        file_result_json(self.inner.read_text_lines(&path, max, None), Value::from)
    }

    /// pi's `readBinaryFile` (non-abort path). Raw bytes cross as a `Buffer`;
    /// an error is thrown with a `{code,message,path?}` JSON reason.
    #[napi(js_name = "readBinaryFile")]
    pub fn read_binary_file(&self, path: String) -> napi::Result<Buffer> {
        match self.inner.read_binary_file(&path, None) {
            Ok(bytes) => Ok(Buffer::from(bytes)),
            Err(error) => Err(napi::Error::from_reason(
                file_error_value(&error).to_string(),
            )),
        }
    }

    /// pi's `writeFile` (non-abort path, string content).
    #[napi(js_name = "writeFile")]
    pub fn write_file(&self, path: String, content: String) -> String {
        file_result_json(
            self.inner
                .write_file(&path, FileContent::Text(&content), None),
            |()| Value::Null,
        )
    }

    /// pi's `appendFile`.
    #[napi(js_name = "appendFile")]
    pub fn append_file(&self, path: String, content: String) -> String {
        file_result_json(
            self.inner
                .append_file(&path, FileContent::Text(&content), None),
            |()| Value::Null,
        )
    }

    /// pi's `fileInfo`.
    #[napi(js_name = "fileInfo")]
    pub fn file_info(&self, path: String) -> String {
        file_result_json(self.inner.file_info(&path, None), |info| {
            file_info_value(&info)
        })
    }

    /// pi's `listDir` (non-abort path).
    #[napi(js_name = "listDir")]
    pub fn list_dir(&self, path: String) -> String {
        file_result_json(self.inner.list_dir(&path, None), |infos| {
            Value::Array(infos.iter().map(file_info_value).collect())
        })
    }

    /// pi's `canonicalPath`.
    #[napi(js_name = "canonicalPath")]
    pub fn canonical_path(&self, path: String) -> String {
        file_result_json(self.inner.canonical_path(&path, None), Value::from)
    }

    /// pi's `exists`.
    #[napi(js_name = "exists")]
    pub fn exists(&self, path: String) -> String {
        file_result_json(self.inner.exists(&path, None), Value::from)
    }

    /// pi's `createDir`.
    #[napi(js_name = "createDir")]
    pub fn create_dir(&self, path: String, recursive: bool) -> String {
        file_result_json(self.inner.create_dir(&path, recursive, None), |()| {
            Value::Null
        })
    }

    /// pi's `remove`.
    #[napi(js_name = "remove")]
    pub fn remove(&self, path: String, recursive: bool, force: bool) -> String {
        file_result_json(self.inner.remove(&path, recursive, force, None), |()| {
            Value::Null
        })
    }

    /// pi's `createTempDir`.
    #[napi(js_name = "createTempDir")]
    pub fn create_temp_dir(&self, prefix: String) -> String {
        file_result_json(self.inner.create_temp_dir(&prefix, None), Value::from)
    }

    /// pi's `createTempFile`.
    #[napi(js_name = "createTempFile")]
    pub fn create_temp_file(&self, prefix: String, suffix: String) -> String {
        file_result_json(
            self.inner.create_temp_file(&prefix, &suffix, None),
            Value::from,
        )
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
            abort_signal: None,
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

    /// `loadSkills` (harness/skills.ts): traverse the given directories through
    /// this host env and return `{skills,diagnostics}` as a JSON string. The
    /// sourced/mapper variants are composed in the shim over this method, so no
    /// opaque JS `source` value ever crosses the boundary. `disableModelInvocation`
    /// is always present on each skill (see [`skill_to_value`]).
    #[napi(js_name = "loadSkills")]
    pub fn load_skills(&self, dirs: Vec<String>) -> String {
        let refs: Vec<&str> = dirs.iter().map(String::as_str).collect();
        let loaded = pidgin_agent::harness::skills::load_skills(&self.inner, &refs);
        let skills: Vec<Value> = loaded.skills.iter().map(skill_to_value).collect();
        let diagnostics = to_json_values(&loaded.diagnostics);
        json!({ "skills": skills, "diagnostics": diagnostics }).to_string()
    }

    /// `loadPromptTemplates` (harness/prompt-templates.ts): load `.md` templates
    /// from the given paths through this host env and return
    /// `{promptTemplates,diagnostics}` as a JSON string. The sourced/mapper
    /// variants are composed in the shim over this method.
    #[napi(js_name = "loadPromptTemplates")]
    pub fn load_prompt_templates(&self, paths: Vec<String>) -> String {
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let loaded =
            pidgin_agent::harness::prompt_templates::load_prompt_templates(&self.inner, &refs);
        let prompt_templates: Vec<Value> = loaded
            .prompt_templates
            .iter()
            .map(prompt_template_to_value)
            .collect();
        let diagnostics = to_json_values(&loaded.diagnostics);
        json!({ "promptTemplates": prompt_templates, "diagnostics": diagnostics }).to_string()
    }
}
