//! CLI extension loading for print / json mode.
//!
//! Consumes the `-e`/`--extension` and `-ne`/`--no-extensions` args (parsed at
//! [`crate::cli::args`] but otherwise dropped) and, under the `deno` feature,
//! constructs a [`DefaultResourceLoader`] with the real deno_core-backed
//! extension loader injected, `reload()`s it, and reports the loaded
//! extensions / errors to **stderr**. Stdout stays clean (a stdout-cleanliness
//! conformance test guards that), so nothing here ever writes to stdout.
//!
//! This mirrors the feature-gated fn-pair pattern PR #177 used for native-http
//! (`builtin_registry_providers` in `pidgin-coding`): a `#[cfg(feature =
//! "deno")]` implementation that pulls V8 and a `#[cfg(not(...))]` graceful
//! fallback, so default `--workspace` builds stay V8-free.
//!
//! This slice is load + register + report only. There is NO dispatch surface in
//! print / json today (`build_harness` takes `tools: None`, `resources: None`),
//! so the report is a stderr side effect and is not fed into the harness —
//! dispatch is deferred to the AgentSession lane (#186), which would reuse the
//! existing one-shot execution primitive `invoke_stored`
//! (`crates/pidgin-extensions/src/runtime.rs:239`); no general invocation
//! primitive is built here.

use crate::cli::args::Args;
use crate::cli::output_guard::err_line;

/// A small report of what [`load_and_report_extensions`] loaded, available in
/// both feature configurations so tests can assert against it without V8.
///
/// Without the `deno` feature the fields are only populated by the (empty)
/// fallback, so mark them dead-code-exempt there; under `deno` and in tests
/// they are read.
#[derive(Default, Debug)]
#[cfg_attr(not(any(feature = "deno", feature = "python")), allow(dead_code))]
pub(crate) struct ExtensionsReport {
    /// Loaded extension names (derived from the file stem of each path).
    pub names: Vec<String>,
    /// Command names registered across the loaded extensions.
    pub commands: Vec<String>,
    /// Tool names registered across the loaded extensions.
    pub tools: Vec<String>,
    /// Per-path load errors.
    pub errors: Vec<String>,
}

/// Derive a display name for an extension from its path, using the file stem
/// (e.g. `.../task-list/index.ts` → `index`, `.../foo.ts` → `foo`).
#[cfg(any(feature = "deno", feature = "python"))]
fn extension_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Loads any extensions requested via `-e`/`--extension` (honoring
/// `-ne`/`--no-extensions`) using the REAL combined deno+python loader, reporting
/// the outcome to stderr. Returns a small report so tests can assert. Which
/// engines are live is derived from the compiled feature set: a `.ts`/`.js` path
/// loads through the deno engine (`--features deno`), a `.py` path through the
/// python engine (`--features python`), and both together under
/// `--features deno,python`.
#[cfg(any(feature = "deno", feature = "python"))]
pub(crate) fn load_and_report_extensions(parsed: &Args, cwd: &str) -> ExtensionsReport {
    use pidgin_coding::core::resource_loader_orchestrator::{
        DefaultResourceLoader, DefaultResourceLoaderOptions, ReloadOptions,
    };

    let paths = parsed.extensions.clone().unwrap_or_default();

    let opts = DefaultResourceLoaderOptions {
        cwd: cwd.to_string(),
        additional_extension_paths: paths,
        no_extensions: parsed.no_extensions,
        // `spawn` returns a boxed trait object; auto-enable each engine that is
        // compiled in via `cfg!` so a `--features deno,python` build runs both.
        extension_loader: Some(pidgin_extensions::CombinedExtensionLoader::spawn(
            pidgin_extensions::EngineSelection {
                deno: cfg!(feature = "deno"),
                python: cfg!(feature = "python"),
            },
        )),
        ..Default::default()
    };

    let mut loader = DefaultResourceLoader::new(opts);
    loader.reload(ReloadOptions::default());
    let result = loader.get_extensions();

    let mut report = ExtensionsReport::default();
    for ext in &result.extensions {
        report.names.push(extension_name(&ext.path));
        report.commands.extend(ext.commands.iter().cloned());
        report.tools.extend(ext.tools.iter().cloned());
    }
    for err in &result.errors {
        report.errors.push(format!("{}: {}", err.path, err.error));
    }

    // Report to STDERR only — stdout must stay clean.
    if report.names.is_empty() {
        if !report.errors.is_empty() {
            err_line(&format!(
                "failed to load {} extension(s):",
                report.errors.len()
            ));
        }
    } else {
        err_line(&format!(
            "loaded {} extension(s): {}",
            report.names.len(),
            report.names.join(", ")
        ));
        if !report.commands.is_empty() {
            err_line(&format!("  commands: {}", report.commands.join(", ")));
        }
        if !report.tools.is_empty() {
            err_line(&format!("  tools: {}", report.tools.join(", ")));
        }
    }
    for err in &report.errors {
        err_line(&format!("  error: {err}"));
    }

    report
}

/// Engine-free fallback: this build compiled in no extension engine. If the user
/// asked to load extensions, print a graceful notice to stderr (matching pi's UX
/// honesty) and return an empty report. Both cfg arms flip together — a build
/// with EITHER engine reaches the real loader above, so this notice only shows on
/// a build with neither.
#[cfg(not(any(feature = "deno", feature = "python")))]
pub(crate) fn load_and_report_extensions(parsed: &Args, _cwd: &str) -> ExtensionsReport {
    if parsed.extensions.as_ref().is_some_and(|v| !v.is_empty()) {
        err_line(
            "note: this build has no extension support; rebuild with `--features deno` (JS/TS) or `--features python` to load extensions",
        );
    }
    ExtensionsReport::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Args;

    /// Assert every field of a report is empty (the "loaded nothing" shape).
    fn assert_empty(report: &ExtensionsReport) {
        assert!(report.names.is_empty());
        assert!(report.commands.is_empty());
        assert!(report.tools.is_empty());
        assert!(report.errors.is_empty());
    }

    /// With NO extension engine compiled in, requesting `-e foo.ts` yields an
    /// empty report and does not panic (the graceful-notice path). This asserts
    /// the arg→behavior contract on the engine-free build.
    #[cfg(not(any(feature = "deno", feature = "python")))]
    #[test]
    fn no_deno_build_returns_empty_report_for_requested_extension() {
        let parsed = Args {
            extensions: Some(vec!["foo.ts".to_string()]),
            ..Args::default()
        };
        assert_empty(&load_and_report_extensions(&parsed, "."));
    }

    /// A default report is empty across both configs.
    #[test]
    fn default_report_is_empty() {
        assert_empty(&ExtensionsReport::default());
    }

    /// Under the `deno` feature, loading the inline `task-list.ts` fixture with
    /// the REAL loader registers command `task` and tool `list_tasks`, and the
    /// returned [`ExtensionsReport`] reflects them. Runs only in the dedicated
    /// V8 CI job. Asserts the actual report struct (the arg→load contract) — the
    /// end-to-end stderr surface is covered by `tests/deno_cli_extensions.rs`.
    #[cfg(feature = "deno")]
    #[test]
    fn deno_build_loads_task_list_fixture() {
        let fixture = format!("{}/tests/fixtures/task-list.ts", env!("CARGO_MANIFEST_DIR"));
        let parsed = Args {
            extensions: Some(vec![fixture]),
            ..Args::default()
        };
        // cwd is irrelevant for an absolute CLI extension path.
        let report = load_and_report_extensions(&parsed, ".");
        assert!(
            report.errors.is_empty(),
            "unexpected load errors: {:?}",
            report.errors
        );
        assert_eq!(report.names, vec!["task-list".to_string()]);
        assert!(
            report.commands.iter().any(|c| c == "task"),
            "commands: {:?}",
            report.commands
        );
        assert!(
            report.tools.iter().any(|t| t == "list_tasks"),
            "tools: {:?}",
            report.tools
        );
    }
}
