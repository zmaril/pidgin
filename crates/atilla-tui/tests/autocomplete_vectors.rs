// straitjacket-allow-file:duplication — the `load()` vector-reading helper and
// the per-scenario replay loop intentionally mirror the two-line boilerplate in
// input_list_vectors.rs / components_vectors.rs; each integration-test binary is
// standalone and cannot share a private helper without a common module.
//! Drives the Rust port of pi's autocomplete provider
//! (`CombinedAutocompleteProvider`) against vectors extracted from pi itself
//! (`crates/atilla-tui/vectors/gen/generate_autocomplete.mjs`). Every assertion
//! is byte-identical: pi's `getSuggestions` items/prefix and `applyCompletion`
//! output are the source of truth. The scenarios replay the exact cases from
//! pi's `test/autocomplete.test.ts`.
//!
//! The filesystem/process host calls pi makes (`readdirSync`/`statSync` and the
//! `fd` subprocess) are recorded per-scenario and replayed through an injected
//! deterministic [`FileProvider`], so the test needs neither a real filesystem
//! nor the `fd` binary. fd-gated scenarios carry recorded fd output.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use atilla_tui::autocomplete::{
    AutocompleteItem, CombinedAutocompleteProvider, DirEntry, FdOutput, FileProvider, ProviderError,
};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> Vec<T> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vectors")
        .join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

#[derive(Deserialize)]
struct RecDirEntry {
    name: String,
    dir: bool,
    link: bool,
}

#[derive(Deserialize)]
struct FdCall {
    args: Vec<String>,
    stdout: String,
    code: Option<i32>,
}

#[derive(Deserialize)]
struct ExpItem {
    value: String,
    label: String,
    description: Option<String>,
}

#[derive(Deserialize)]
struct Expected {
    prefix: String,
    items: Vec<ExpItem>,
}

#[derive(Deserialize)]
struct Apply {
    #[serde(rename = "itemValue")]
    item_value: String,
    #[serde(rename = "expectedLines")]
    expected_lines: Vec<String>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    #[serde(rename = "basePath")]
    base_path: String,
    #[serde(rename = "hasFd")]
    has_fd: bool,
    lines: Vec<String>,
    #[serde(rename = "cursorLine")]
    cursor_line: usize,
    #[serde(rename = "cursorCol")]
    cursor_col: usize,
    force: bool,
    readdir: HashMap<String, Vec<RecDirEntry>>,
    stat: HashMap<String, Option<bool>>,
    homedir: String,
    #[serde(rename = "fdCalls")]
    fd_calls: Vec<FdCall>,
    expected: Option<Expected>,
    #[serde(rename = "assertMode")]
    assert_mode: String,
    #[serde(default)]
    apply: Option<Apply>,
}

// Normalize a filesystem path key: strip a trailing slash (except for root),
// so recorded keys (no trailing slash) match queries where Node's `path.join`
// preserved one (e.g. scoped fuzzy `baseDir` = ".../outside/").
fn norm_key(path: &str) -> String {
    if path.len() > 1 {
        path.trim_end_matches('/').to_string()
    } else {
        path.to_string()
    }
}

struct RecordedProvider {
    readdir: HashMap<String, Vec<RecDirEntry>>,
    stat: HashMap<String, Option<bool>>,
    homedir: String,
    fd_calls: Vec<FdCall>,
    scenario: String,
}

impl FileProvider for RecordedProvider {
    fn read_dir(&self, dir: &str) -> Result<Vec<DirEntry>, ProviderError> {
        match self.readdir.get(&norm_key(dir)) {
            Some(entries) => Ok(entries
                .iter()
                .map(|e| DirEntry {
                    name: e.name.clone(),
                    is_directory: e.dir,
                    is_symbolic_link: e.link,
                })
                .collect()),
            None => Err(ProviderError),
        }
    }

    fn stat_is_directory(&self, path: &str) -> Result<bool, ProviderError> {
        match self.stat.get(&norm_key(path)) {
            Some(Some(b)) => Ok(*b),
            // Recorded as a thrown statSync, or not recorded (ENOENT) -> error.
            _ => Err(ProviderError),
        }
    }

    fn home_dir(&self) -> String {
        self.homedir.clone()
    }

    fn run_fd(&self, args: &[String]) -> FdOutput {
        for c in &self.fd_calls {
            if c.args == args {
                return FdOutput {
                    stdout: c.stdout.clone(),
                    code: c.code,
                };
            }
        }
        panic!(
            "[{}] unrecorded fd invocation: {:?}\nrecorded: {:?}",
            self.scenario,
            args,
            self.fd_calls.iter().map(|c| &c.args).collect::<Vec<_>>()
        );
    }
}

#[test]
fn autocomplete_scenarios() {
    let scenarios: Vec<Scenario> = load("autocomplete_scenarios");
    assert!(!scenarios.is_empty(), "no autocomplete scenarios loaded");

    let mut fd_count = 0;
    for s in &scenarios {
        if s.has_fd {
            fd_count += 1;
        }
        let provider = RecordedProvider {
            readdir: s
                .readdir
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.iter()
                            .map(|e| RecDirEntry {
                                name: e.name.clone(),
                                dir: e.dir,
                                link: e.link,
                            })
                            .collect(),
                    )
                })
                .collect(),
            stat: s.stat.clone(),
            homedir: s.homedir.clone(),
            fd_calls: s
                .fd_calls
                .iter()
                .map(|c| FdCall {
                    args: c.args.clone(),
                    stdout: c.stdout.clone(),
                    code: c.code,
                })
                .collect(),
            scenario: s.name.clone(),
        };

        let fd_path = if s.has_fd {
            Some("fd".to_string())
        } else {
            None
        };
        let ac = CombinedAutocompleteProvider::new(vec![], s.base_path.clone(), fd_path, provider);

        let result = ac.get_suggestions(&s.lines, s.cursor_line, s.cursor_col, s.force);

        match (&s.expected, &result) {
            (None, None) => {}
            (None, Some(r)) => panic!(
                "[{}] expected null, got prefix {:?} with {} items",
                s.name,
                r.prefix,
                r.items.len()
            ),
            (Some(exp), None) => panic!(
                "[{}] expected prefix {:?} ({} items), got null",
                s.name,
                exp.prefix,
                exp.items.len()
            ),
            (Some(exp), Some(r)) => {
                assert_eq!(r.prefix, exp.prefix, "[{}] prefix mismatch", s.name);
                match s.assert_mode.as_str() {
                    "items" => {
                        assert_eq!(
                            r.items.len(),
                            exp.items.len(),
                            "[{}] item count mismatch",
                            s.name
                        );
                        for (i, (got, want)) in r.items.iter().zip(exp.items.iter()).enumerate() {
                            assert_eq!(got.value, want.value, "[{}] item {i} value", s.name);
                            assert_eq!(got.label, want.label, "[{}] item {i} label", s.name);
                            assert_eq!(
                                got.description, want.description,
                                "[{}] item {i} description",
                                s.name
                            );
                        }
                    }
                    "prefixAndValueSet" => {
                        // System-root listing order is not part of pi's contract;
                        // assert every value string via a sorted comparison.
                        let mut got: Vec<String> =
                            r.items.iter().map(|i| i.value.clone()).collect();
                        let mut want: Vec<String> =
                            exp.items.iter().map(|i| i.value.clone()).collect();
                        got.sort();
                        want.sort();
                        assert_eq!(got, want, "[{}] value set mismatch", s.name);
                    }
                    other => panic!("[{}] unknown assertMode {other}", s.name),
                }
            }
        }

        if let Some(apply) = &s.apply {
            let r = result
                .as_ref()
                .unwrap_or_else(|| panic!("[{}] apply requires a result", s.name));
            let item = r
                .items
                .iter()
                .find(|i| i.value == apply.item_value)
                .unwrap_or_else(|| {
                    panic!("[{}] apply item {} not found", s.name, apply.item_value)
                });
            let item = AutocompleteItem {
                value: item.value.clone(),
                label: item.label.clone(),
                description: item.description.clone(),
            };
            let applied =
                ac.apply_completion(&s.lines, s.cursor_line, s.cursor_col, &item, &r.prefix);
            assert_eq!(
                applied.lines, apply.expected_lines,
                "[{}] applied lines mismatch",
                s.name
            );
        }
    }

    eprintln!(
        "autocomplete_scenarios: {} scenarios ({} fd-recorded, {} non-fd) all byte-exact",
        scenarios.len(),
        fd_count,
        scenarios.len() - fd_count
    );
}
