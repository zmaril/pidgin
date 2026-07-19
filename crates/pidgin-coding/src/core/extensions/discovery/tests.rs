// straitjacket-allow-file:duplication
//! Discovery tests mirroring pi's `test/extensions-discovery.test.ts`.
//!
//! Each test builds a temp-directory extension layout (the Rust analog of the
//! vitest suite's `mkdtempSync` fixtures) and asserts that discovery finds,
//! resolves, orders, and de-duplicates entrypoints exactly as pi's tests expect.
//! Cases whose pi assertions require *executing* the extension (registering
//! tools/commands/handlers, or reporting a module/factory error) belong to the
//! JS-execution plane and are noted as deferred in the module docs and the PR
//! summary; here we cover the filesystem-convention + entrypoint-resolution half
//! those cases share.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::*;

/// Placeholder entrypoint contents. Discovery never executes these, so the body
/// is irrelevant — only the file's existence and suffix matter.
const EXT_CODE: &str = "export default function (pi) {}\n";

/// Create a temp dir with an `extensions/` child. The temp dir is used as both
/// `cwd` and `agent_dir`, so `agent_dir/extensions/` (the global root) is the
/// `extensions/` dir — matching pi's `discoverAndLoadExtensions([], temp, temp)`.
fn setup() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temp dir");
    let ext_dir = temp.path().join("extensions");
    fs::create_dir(&ext_dir).expect("extensions dir");
    (temp, ext_dir)
}

fn temp_str(temp: &TempDir) -> String {
    temp.path().to_string_lossy().into_owned()
}

/// Run discovery with the temp dir as both cwd and agent_dir.
fn discover(temp: &TempDir, configured: &[String]) -> DiscoveryResult {
    let t = temp_str(temp);
    discover_extensions(configured, &t, &t)
}

fn write(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    fs::write(path, contents).expect("write file");
}

/// Sorted entrypoint basenames.
fn basenames(result: &DiscoveryResult) -> Vec<String> {
    let mut names: Vec<String> = result
        .extensions
        .iter()
        .map(|e| {
            e.entrypoint_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn contains_path(result: &DiscoveryResult, needle: &str) -> bool {
    result
        .extensions
        .iter()
        .any(|e| e.entrypoint_path.to_string_lossy().contains(needle))
}

// ---------------------------------------------------------------------------
// Direct-file discovery
// ---------------------------------------------------------------------------

#[test]
fn discovers_direct_ts_files() {
    let (temp, ext_dir) = setup();
    write(&ext_dir.join("foo.ts"), EXT_CODE);
    write(&ext_dir.join("bar.ts"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 2);
    assert_eq!(basenames(&result), vec!["bar.ts", "foo.ts"]);
}

#[test]
fn discovers_direct_js_files() {
    let (temp, ext_dir) = setup();
    write(&ext_dir.join("foo.js"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(basenames(&result), vec!["foo.js"]);
    assert_eq!(result.extensions[0].language, ExtensionLanguage::JavaScript);
}

// ---------------------------------------------------------------------------
// Subdirectory discovery: index files
// ---------------------------------------------------------------------------

#[test]
fn discovers_subdirectory_with_index_ts() {
    let (temp, ext_dir) = setup();
    write(&ext_dir.join("my-extension").join("index.ts"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "my-extension"));
    assert!(contains_path(&result, "index.ts"));
    // The id falls back to the directory name for an index entrypoint.
    assert_eq!(result.extensions[0].id, "my-extension");
}

#[test]
fn discovers_subdirectory_with_index_js() {
    let (temp, ext_dir) = setup();
    write(&ext_dir.join("my-extension").join("index.js"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "index.js"));
}

#[test]
fn prefers_index_ts_over_index_js() {
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-extension");
    write(&subdir.join("index.ts"), EXT_CODE);
    write(&subdir.join("index.js"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "index.ts"));
    assert!(!contains_path(&result, "index.js"));
}

// ---------------------------------------------------------------------------
// Subdirectory discovery: package.json pi manifest
// ---------------------------------------------------------------------------

#[test]
fn discovers_subdirectory_with_package_json_pi_field() {
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-package");
    write(&subdir.join("src").join("main.ts"), EXT_CODE);
    write(
        &subdir.join("package.json"),
        r#"{ "name": "my-package", "pi": { "extensions": ["./src/main.ts"] } }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "src"));
    assert!(contains_path(&result, "main.ts"));
}

#[test]
fn keeps_package_json_tilde_entries_package_relative() {
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("tilde-package");
    let direct = subdir.join("~entry.ts");
    let slashed = subdir.join("~").join("entry.ts");
    write(&direct, EXT_CODE);
    write(&slashed, EXT_CODE);
    write(
        &subdir.join("package.json"),
        r#"{ "name": "tilde-package", "pi": { "extensions": ["~entry.ts", "~/entry.ts"] } }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    let mut got: Vec<String> = result
        .extensions
        .iter()
        .map(|e| e.entrypoint_path.to_string_lossy().into_owned())
        .collect();
    got.sort();
    let mut expected = vec![
        direct.to_string_lossy().into_owned(),
        slashed.to_string_lossy().into_owned(),
    ];
    expected.sort();
    // The tilde is treated literally and package-relative (no home expansion).
    assert_eq!(got, expected);
}

#[test]
fn package_json_can_declare_multiple_extensions() {
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-package");
    write(&subdir.join("ext1.ts"), EXT_CODE);
    write(&subdir.join("ext2.ts"), EXT_CODE);
    write(
        &subdir.join("package.json"),
        r#"{ "name": "my-package", "pi": { "extensions": ["./ext1.ts", "./ext2.ts"] } }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 2);
    assert_eq!(basenames(&result), vec!["ext1.ts", "ext2.ts"]);
}

#[test]
fn package_json_pi_field_takes_precedence_over_index() {
    // Path-level parity: the manifest-declared entrypoint wins over index.ts.
    // pi additionally asserts the registered tool came from custom.ts, not
    // index.ts — that assertion requires executing the factory and is deferred
    // to the JS-execution plane.
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-package");
    write(&subdir.join("index.ts"), EXT_CODE);
    write(&subdir.join("custom.ts"), EXT_CODE);
    write(
        &subdir.join("package.json"),
        r#"{ "name": "my-package", "pi": { "extensions": ["./custom.ts"] } }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "custom.ts"));
    assert!(!contains_path(&result, "index.ts"));
}

#[test]
fn ignores_package_json_without_pi_field_falls_back_to_index() {
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-package");
    write(&subdir.join("index.ts"), EXT_CODE);
    write(
        &subdir.join("package.json"),
        r#"{ "name": "my-package", "version": "1.0.0" }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "index.ts"));
}

#[test]
fn ignores_malformed_package_json_falls_back_to_index() {
    // pi's readPiManifest swallows JSON parse errors and returns null, so a
    // malformed package.json silently falls back to index resolution.
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-package");
    write(&subdir.join("index.ts"), EXT_CODE);
    write(&subdir.join("package.json"), "{ this is not valid json ");

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "index.ts"));
}

#[test]
fn skips_nonexistent_paths_declared_in_package_json() {
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-package");
    write(&subdir.join("exists.ts"), EXT_CODE);
    write(
        &subdir.join("package.json"),
        r#"{ "pi": { "extensions": ["./exists.ts", "./missing.ts"] } }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "exists.ts"));
}

#[test]
fn package_json_with_all_missing_entries_falls_back_to_index() {
    // When every declared entry is missing, pi's resolveExtensionEntries returns
    // no manifest entries and falls through to index.ts.
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("my-package");
    write(&subdir.join("index.ts"), EXT_CODE);
    write(
        &subdir.join("package.json"),
        r#"{ "pi": { "extensions": ["./missing.ts"] } }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "index.ts"));
}

// ---------------------------------------------------------------------------
// Non-recursion and negative cases
// ---------------------------------------------------------------------------

#[test]
fn ignores_subdirectory_without_index_or_package_json() {
    let (temp, ext_dir) = setup();
    let subdir = ext_dir.join("not-an-extension");
    write(&subdir.join("helper.ts"), EXT_CODE);
    write(&subdir.join("utils.ts"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 0);
}

#[test]
fn does_not_recurse_beyond_one_level() {
    let (temp, ext_dir) = setup();
    // container/ has no index or package.json; nested/index.ts must not be found.
    write(
        &ext_dir.join("container").join("nested").join("index.ts"),
        EXT_CODE,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 0);
}

#[test]
fn empty_roots_discover_nothing() {
    let (temp, _ext_dir) = setup();

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 0);
}

// ---------------------------------------------------------------------------
// Mixed and configured-path discovery
// ---------------------------------------------------------------------------

#[test]
fn handles_mixed_direct_files_and_subdirectories() {
    let (temp, ext_dir) = setup();
    write(&ext_dir.join("direct.ts"), EXT_CODE);
    write(&ext_dir.join("with-index").join("index.ts"), EXT_CODE);
    let manifest_dir = ext_dir.join("with-manifest");
    write(&manifest_dir.join("entry.ts"), EXT_CODE);
    write(
        &manifest_dir.join("package.json"),
        r#"{ "pi": { "extensions": ["./entry.ts"] } }"#,
    );

    let result = discover(&temp, &[]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 3);
}

#[test]
fn handles_explicitly_configured_file_path() {
    let (temp, _ext_dir) = setup();
    let custom = temp.path().join("custom-location").join("my-ext.ts");
    write(&custom, EXT_CODE);

    let result = discover(&temp, &[custom.to_string_lossy().into_owned()]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "my-ext.ts"));
    assert_eq!(result.extensions[0].origin, DiscoveryOrigin::Configured);
}

#[test]
fn configured_directory_resolves_index_entry() {
    // A configured *directory* is resolved like a discovered subdirectory:
    // its index.ts / package.json manifest is honored.
    let (temp, _ext_dir) = setup();
    let dir = temp.path().join("configured-pkg");
    write(&dir.join("index.ts"), EXT_CODE);

    let result = discover(&temp, &[dir.to_string_lossy().into_owned()]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert!(contains_path(&result, "index.ts"));
}

#[test]
fn configured_directory_without_entries_scans_direct_files() {
    // A configured directory with no index/manifest falls back to a direct-file
    // scan of that directory.
    let (temp, _ext_dir) = setup();
    let dir = temp.path().join("configured-loose");
    write(&dir.join("a.ts"), EXT_CODE);
    write(&dir.join("b.ts"), EXT_CODE);

    let result = discover(&temp, &[dir.to_string_lossy().into_owned()]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 2);
    assert_eq!(basenames(&result), vec!["a.ts", "b.ts"]);
}

#[test]
fn discovers_project_local_root() {
    // Extensions under cwd/.pi/extensions/ are discovered via the project-local
    // root with the matching origin.
    let temp = tempfile::tempdir().expect("temp dir");
    let local = temp.path().join(".pi").join("extensions");
    write(&local.join("local.ts"), EXT_CODE);
    let t = temp_str(&temp);

    // Use a distinct, empty agent dir so only the project-local root matches.
    let agent = tempfile::tempdir().expect("agent dir");
    let result = discover_extensions(&[], &t, &agent.path().to_string_lossy());

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(result.extensions[0].origin, DiscoveryOrigin::ProjectLocal);
    assert!(contains_path(&result, "local.ts"));
}

#[test]
fn deduplicates_paths_reachable_through_multiple_roots() {
    // foo.ts is discoverable via the global root and also passed as a configured
    // path; it must appear once (first occurrence wins).
    let (temp, ext_dir) = setup();
    let foo = ext_dir.join("foo.ts");
    write(&foo, EXT_CODE);

    let result = discover(&temp, &[foo.to_string_lossy().into_owned()]);

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    // First occurrence is the global root scan.
    assert_eq!(result.extensions[0].origin, DiscoveryOrigin::Global);
}

// ---------------------------------------------------------------------------
// Language inference and id derivation
// ---------------------------------------------------------------------------

#[test]
fn infers_language_from_entrypoint_suffix() {
    let (temp, ext_dir) = setup();
    write(&ext_dir.join("typed.ts"), EXT_CODE);
    write(&ext_dir.join("plain.js"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert_eq!(result.extensions.len(), 2);
    let ts = result.extensions.iter().find(|e| e.id == "typed").unwrap();
    let js = result.extensions.iter().find(|e| e.id == "plain").unwrap();
    assert_eq!(ts.language, ExtensionLanguage::TypeScript);
    assert_eq!(js.language, ExtensionLanguage::JavaScript);
}

#[test]
fn direct_file_id_and_root_reflect_the_entrypoint() {
    let (temp, ext_dir) = setup();
    write(&ext_dir.join("greet.ts"), EXT_CODE);

    let result = discover(&temp, &[]);

    assert_eq!(result.extensions.len(), 1);
    let ext = &result.extensions[0];
    assert_eq!(ext.id, "greet");
    assert_eq!(ext.root, ext_dir);
    assert_eq!(ext.entrypoint_path, ext_dir.join("greet.ts"));
}

// ---------------------------------------------------------------------------
// load_extensions: explicit-only, no discovery
// ---------------------------------------------------------------------------

#[test]
fn load_extensions_loads_only_explicit_paths_without_discovery() {
    let (temp, ext_dir) = setup();
    // Discoverable extension that discover_extensions would find.
    write(&ext_dir.join("discovered.ts"), EXT_CODE);
    // Explicit extension outside the discovery roots.
    let explicit = temp.path().join("explicit.ts");
    write(&explicit, EXT_CODE);

    let result = load_extensions(&[explicit.to_string_lossy().into_owned()], &temp_str(&temp));

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(result.extensions[0].id, "explicit");
    assert!(!contains_path(&result, "discovered.ts"));
}

#[test]
fn load_extensions_with_no_paths_loads_nothing() {
    let (temp, ext_dir) = setup();
    // A discoverable extension exists, but load_extensions must ignore it.
    write(&ext_dir.join("discovered.ts"), EXT_CODE);

    let result = load_extensions(&[], &temp_str(&temp));

    assert!(result.errors.is_empty());
    assert_eq!(result.extensions.len(), 0);
}
