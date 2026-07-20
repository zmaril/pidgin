//! Node-API surface for the tui autocomplete provider (`AutocompleteCore`).
//!
//! Backs the native `autocomplete.ts` conformance shim. See the type doc on
//! [`AutocompleteCore`] for the design; the module-level notes below carry the
//! detail that used to sit inline in `lib.rs`.

use napi_derive::napi;

// ---------------------------------------------------------------------------
// AutocompleteCore: the tui `CombinedAutocompleteProvider` (autocomplete.ts).
// ---------------------------------------------------------------------------
//
// Backs the native `autocomplete.ts` conformance shim. pi's provider reaches the
// outside world through four host seams — `readdirSync`, `statSync`, `homedir`,
// and `spawn(fd, args)`. The merged Rust port
// ([`pidgin_tui::CombinedAutocompleteProvider`]) abstracts those behind a
// [`pidgin_tui::FileProvider`] trait; the pure prefix-extraction / ranking /
// path-formatting logic is Rust. `AutocompleteCore` supplies a *native*
// `FileProvider` ([`HostFileProvider`]) implemented over `std::fs` and
// `std::process::Command`. Because pi's `autocomplete.test.ts` drives the
// provider against real temporary directories (and a real `fd` binary), reading
// the same filesystem from Rust — and spawning the same `fd` with the byte-exact
// args the Rust core builds — reproduces pi's suggestions exactly. Output order
// never depends on `readdir`/`fd` traversal order: file suggestions are sorted
// (directories first, then locale order) and fuzzy `@` results are scored and
// sorted before return.
//
// The suite always constructs with an empty command list, so slash-command
// argument callbacks (`getArgumentCompletions`, which cannot cross the addon
// boundary) never reach the core; the shim delegates any such command to pi's
// original class. Commands passed here are parsed without callbacks.

/// A native [`FileProvider`](pidgin_tui::FileProvider) over `std::fs` /
/// `std::process`. `fd_path` is the absolute path to the `fd` binary (mirrors
/// pi's `this.fdPath`), used to spawn the fuzzy `@` file walk.
struct HostFileProvider {
    fd_path: Option<String>,
}

impl pidgin_tui::FileProvider for HostFileProvider {
    fn read_dir(&self, dir: &str) -> Result<Vec<pidgin_tui::DirEntry>, pidgin_tui::ProviderError> {
        // `readdirSync(dir, { withFileTypes: true })`. On Unix `file_type()` uses
        // the dirent type (no symlink follow), matching Node's `Dirent`.
        let read = std::fs::read_dir(dir).map_err(|_| pidgin_tui::ProviderError)?;
        let mut out = Vec::new();
        for entry in read {
            let entry = entry.map_err(|_| pidgin_tui::ProviderError)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let ft = entry.file_type().map_err(|_| pidgin_tui::ProviderError)?;
            out.push(pidgin_tui::DirEntry {
                name,
                is_directory: ft.is_dir(),
                is_symbolic_link: ft.is_symlink(),
            });
        }
        Ok(out)
    }

    fn stat_is_directory(&self, path: &str) -> Result<bool, pidgin_tui::ProviderError> {
        // `statSync(path).isDirectory()` follows symlinks; `std::fs::metadata` does too.
        let md = std::fs::metadata(path).map_err(|_| pidgin_tui::ProviderError)?;
        Ok(md.is_dir())
    }

    fn home_dir(&self) -> String {
        // `os.homedir()`. Only used for `~` expansion, which the suite never exercises.
        std::env::var("HOME").unwrap_or_default()
    }

    fn run_fd(&self, args: &[String]) -> pidgin_tui::FdOutput {
        // `spawn(fdPath, args)` collecting stdout + exit code. A missing binary /
        // spawn error surfaces as `code: None`, which the Rust core treats as "no
        // results" (matching pi's `child.on("error")` path).
        let Some(fd_path) = self.fd_path.as_ref() else {
            return pidgin_tui::FdOutput {
                stdout: String::new(),
                code: None,
            };
        };
        match std::process::Command::new(fd_path).args(args).output() {
            Ok(output) => pidgin_tui::FdOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                code: output.status.code(),
            },
            Err(_) => pidgin_tui::FdOutput {
                stdout: String::new(),
                code: None,
            },
        }
    }
}

/// JSON shape of a command entry (pi's `SlashCommand | AutocompleteItem`). An
/// object with a `name` becomes a slash command (no argument callback — those
/// stay in the shim's original delegation); otherwise a plain item keyed by
/// `value`.
#[derive(serde::Deserialize)]
struct CommandInput {
    name: Option<String>,
    value: Option<String>,
    label: Option<String>,
    description: Option<String>,
    #[serde(rename = "argumentHint")]
    argument_hint: Option<String>,
}

/// JSON shape of an `AutocompleteItem` crossing into `applyCompletion`.
#[derive(serde::Deserialize)]
struct AutocompleteItemIn {
    value: String,
    label: String,
    description: Option<String>,
}

fn commands_from_json(commands_json: &str) -> napi::Result<Vec<pidgin_tui::Command>> {
    let parsed: Vec<CommandInput> = serde_json::from_str(commands_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid commands: {e}")))?;
    Ok(parsed
        .into_iter()
        .map(|c| match c.name {
            Some(name) => pidgin_tui::Command::Slash(pidgin_tui::SlashCommand {
                name,
                description: c.description,
                argument_hint: c.argument_hint,
                get_argument_completions: None,
            }),
            None => pidgin_tui::Command::Item(pidgin_tui::AutocompleteItem {
                value: c.value.unwrap_or_default(),
                label: c.label.unwrap_or_default(),
                description: c.description,
            }),
        })
        .collect())
}

fn suggestions_to_json(s: &pidgin_tui::AutocompleteSuggestions) -> serde_json::Value {
    serde_json::json!({
        "items": s.items.iter().map(|item| serde_json::json!({
            "value": item.value,
            "label": item.label,
            "description": item.description,
        })).collect::<Vec<_>>(),
        "prefix": s.prefix,
    })
}

/// The Rust-backed combined autocomplete provider, exposed to JavaScript as
/// `AutocompleteCore`. Wraps one [`CombinedAutocompleteProvider`] over a native
/// [`HostFileProvider`]; the JS shim constructs one per pi provider instance.
#[napi(js_name = "AutocompleteCore")]
pub struct AutocompleteCore {
    inner: pidgin_tui::CombinedAutocompleteProvider<HostFileProvider>,
}

#[napi]
impl AutocompleteCore {
    /// Build a provider from pi's `commands` (JSON array), `basePath`, and the
    /// optional absolute `fdPath` (present when the `fd` binary is available,
    /// gating the fuzzy `@` walk).
    #[napi(constructor)]
    pub fn new(
        commands_json: String,
        base_path: String,
        fd_path: Option<String>,
    ) -> napi::Result<Self> {
        let commands = commands_from_json(&commands_json)?;
        let provider = HostFileProvider {
            fd_path: fd_path.clone(),
        };
        Ok(Self {
            inner: pidgin_tui::CombinedAutocompleteProvider::new(
                commands, base_path, fd_path, provider,
            ),
        })
    }

    /// pi's `getSuggestions(lines, cursorLine, cursorCol, { force })` — the
    /// `signal` is not needed (the Rust path is synchronous and the suite never
    /// aborts). Returns the suggestions as JSON (`{ items, prefix }`), or `null`
    /// when none are available.
    #[napi(js_name = "getSuggestionsJson")]
    pub fn get_suggestions_json(
        &self,
        lines: Vec<String>,
        cursor_line: i64,
        cursor_col: i64,
        force: bool,
    ) -> napi::Result<Option<String>> {
        match self.inner.get_suggestions(
            &lines,
            cursor_line.max(0) as usize,
            cursor_col.max(0) as usize,
            force,
        ) {
            Some(s) => serde_json::to_string(&suggestions_to_json(&s))
                .map(Some)
                .map_err(|e| napi::Error::from_reason(e.to_string())),
            None => Ok(None),
        }
    }

    /// pi's `applyCompletion(lines, cursorLine, cursorCol, item, prefix)`.
    /// Returns the new document state as JSON (`{ lines, cursorLine, cursorCol }`).
    #[napi(js_name = "applyCompletionJson")]
    pub fn apply_completion_json(
        &self,
        lines: Vec<String>,
        cursor_line: i64,
        cursor_col: i64,
        item_json: String,
        prefix: String,
    ) -> napi::Result<String> {
        let item_in: AutocompleteItemIn = serde_json::from_str(&item_json)
            .map_err(|e| napi::Error::from_reason(format!("invalid item: {e}")))?;
        let item = pidgin_tui::AutocompleteItem {
            value: item_in.value,
            label: item_in.label,
            description: item_in.description,
        };
        let applied = self.inner.apply_completion(
            &lines,
            cursor_line.max(0) as usize,
            cursor_col.max(0) as usize,
            &item,
            &prefix,
        );
        serde_json::to_string(&serde_json::json!({
            "lines": applied.lines,
            "cursorLine": applied.cursor_line,
            "cursorCol": applied.cursor_col,
        }))
        .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// pi's `shouldTriggerFileCompletion(lines, cursorLine, cursorCol)`.
    #[napi(js_name = "shouldTriggerFileCompletion")]
    pub fn should_trigger_file_completion(
        &self,
        lines: Vec<String>,
        cursor_line: i64,
        cursor_col: i64,
    ) -> bool {
        self.inner.should_trigger_file_completion(
            &lines,
            cursor_line.max(0) as usize,
            cursor_col.max(0) as usize,
        )
    }
}
