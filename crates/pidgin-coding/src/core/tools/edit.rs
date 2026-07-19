//! Argument coercion, validation, and the pure edit pipeline for the edit tool.
//!
//! Ported from pi's `core/tools/edit.ts`. [`prepare_edit_arguments`] handles the
//! legacy `oldText`/`newText` input shape and edits-as-JSON-string coercion that
//! some models emit; [`validate_edit_input`] enforces a non-empty edits array.
//! [`compute_edit_result`] composes the pure edit-diff core (BOM strip, line
//! ending detect/normalize/restore, edit application, diff + patch generation)
//! over already-read file content.
//!
//! Deferred: the filesystem read/write, the per-file mutation queue, the abort
//! plumbing, and all TUI rendering. Those need an execution environment and the
//! interactive theme layer, so the tool's `execute` shell is not ported here.

use serde_json::{Map, Value};

use super::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom, Edit,
};

/// Coerce raw tool input into the canonical `{ path, edits }` shape.
///
/// - `edits` supplied as a JSON string is parsed into an array when valid.
/// - Legacy top-level `oldText`/`newText` (both strings) are folded into a
///   trailing `edits[]` entry and removed from the object.
/// - Non-object input and already-valid input pass through unchanged.
pub fn prepare_edit_arguments(input: Value) -> Value {
    let mut args = match input {
        Value::Object(m) => m,
        other => return other,
    };

    // Some models send edits as a JSON string instead of an array.
    if let Some(Value::String(s)) = args.get("edits").cloned() {
        if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
            if parsed.is_array() {
                args.insert("edits".to_string(), parsed);
            }
        }
    }

    let old_is_str = matches!(args.get("oldText"), Some(Value::String(_)));
    let new_is_str = matches!(args.get("newText"), Some(Value::String(_)));
    if !(old_is_str && new_is_str) {
        return Value::Object(args);
    }

    let old_text = args.get("oldText").cloned().unwrap();
    let new_text = args.get("newText").cloned().unwrap();
    let mut edits = match args.get("edits") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let mut edit_obj = Map::new();
    edit_obj.insert("oldText".to_string(), old_text);
    edit_obj.insert("newText".to_string(), new_text);
    edits.push(Value::Object(edit_obj));

    args.remove("oldText");
    args.remove("newText");
    args.insert("edits".to_string(), Value::Array(edits));
    Value::Object(args)
}

/// The validated `{ path, edits }` extracted from prepared input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEditInput {
    /// Target file path.
    pub path: String,
    /// The replacements to apply.
    pub edits: Vec<Edit>,
}

/// Validate prepared input: `edits` must be a non-empty array of
/// `{ oldText, newText }` objects.
pub fn validate_edit_input(input: &Value) -> Result<ValidatedEditInput, String> {
    let edits_val = input.get("edits");
    let edits_arr = match edits_val {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => {
            return Err(
                "Edit tool input is invalid. edits must contain at least one replacement."
                    .to_string(),
            );
        }
    };

    let mut edits = Vec::with_capacity(edits_arr.len());
    for e in edits_arr {
        let old_text = e.get("oldText").and_then(Value::as_str).ok_or_else(|| {
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string()
        })?;
        let new_text = e.get("newText").and_then(Value::as_str).ok_or_else(|| {
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string()
        })?;
        edits.push(Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        });
    }

    let path = input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(ValidatedEditInput { path, edits })
}

/// The pure result of applying edits to file content (no filesystem write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditComputation {
    /// The full new file content, with BOM and original line endings restored.
    pub final_content: String,
    /// Display-oriented diff.
    pub diff: String,
    /// 1-based first changed line in the new file.
    pub first_changed_line: Option<usize>,
    /// jsdiff-compatible unified patch.
    pub patch: String,
    /// The success message the tool returns.
    pub message: String,
}

/// Compute the edit result for `raw_content` (the file's contents as read).
///
/// Mirrors pi's `execute` minus the filesystem: strip BOM, detect the original
/// line ending, LF-normalize, apply edits, then restore endings and BOM.
pub fn compute_edit_result(
    raw_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<EditComputation, String> {
    let stripped = strip_bom(raw_content);
    let original_ending = detect_line_ending(&stripped.text);
    let normalized = normalize_to_lf(&stripped.text);
    let applied = apply_edits_to_normalized_content(&normalized, edits, path)?;
    let final_content = format!(
        "{}{}",
        stripped.bom,
        restore_line_endings(&applied.new_content, original_ending)
    );
    let diff = generate_diff_string(&applied.base_content, &applied.new_content, 4);
    let patch = generate_unified_patch(path, &applied.base_content, &applied.new_content, 4);
    Ok(EditComputation {
        final_content,
        diff: diff.diff,
        first_changed_line: diff.first_changed_line,
        patch,
        message: format!(
            "Successfully replaced {} block(s) in {}.",
            edits.len(),
            path
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(old: &str, new: &str) -> Edit {
        Edit {
            old_text: old.to_string(),
            new_text: new.to_string(),
        }
    }

    // --- prepare_edit_arguments (pi edit-legacy-input.test.ts) ---

    #[test]
    fn folds_top_level_old_new_into_edits() {
        let prepared = prepare_edit_arguments(json!({
            "path": "file.txt",
            "oldText": "before",
            "newText": "after",
        }));
        assert_eq!(
            prepared,
            json!({ "path": "file.txt", "edits": [{ "oldText": "before", "newText": "after" }] })
        );
    }

    #[test]
    fn appends_legacy_replacement_to_existing_edits() {
        let prepared = prepare_edit_arguments(json!({
            "path": "file.txt",
            "edits": [{ "oldText": "a", "newText": "b" }],
            "oldText": "c",
            "newText": "d",
        }));
        assert_eq!(
            prepared,
            json!({
                "path": "file.txt",
                "edits": [
                    { "oldText": "a", "newText": "b" },
                    { "oldText": "c", "newText": "d" }
                ]
            })
        );
    }

    #[test]
    fn passes_through_valid_input_unchanged() {
        let input = json!({ "path": "file.txt", "edits": [{ "oldText": "a", "newText": "b" }] });
        assert_eq!(prepare_edit_arguments(input.clone()), input);
    }

    #[test]
    fn passes_through_non_object_input_unchanged() {
        assert_eq!(prepare_edit_arguments(Value::Null), Value::Null);
        assert_eq!(prepare_edit_arguments(json!("garbage")), json!("garbage"));
    }

    #[test]
    fn parses_edits_from_json_string() {
        let prepared = prepare_edit_arguments(json!({
            "path": "file.txt",
            "edits": "[{\"oldText\": \"a\", \"newText\": \"b\"}]",
        }));
        assert_eq!(
            prepared,
            json!({ "path": "file.txt", "edits": [{ "oldText": "a", "newText": "b" }] })
        );
    }

    #[test]
    fn leaves_edits_alone_when_not_valid_json() {
        let prepared = prepare_edit_arguments(json!({ "path": "file.txt", "edits": "not json" }));
        assert_eq!(prepared, json!({ "path": "file.txt", "edits": "not json" }));
    }

    // --- validate_edit_input ---

    #[test]
    fn validate_rejects_empty_edits() {
        let err = validate_edit_input(&json!({ "path": "f.txt", "edits": [] })).unwrap_err();
        assert!(err.contains("edits must contain at least one replacement"));
    }

    #[test]
    fn validate_extracts_path_and_edits() {
        let v = validate_edit_input(&json!({
            "path": "f.txt",
            "edits": [{ "oldText": "a", "newText": "b" }]
        }))
        .unwrap();
        assert_eq!(v.path, "f.txt");
        assert_eq!(v.edits, vec![edit("a", "b")]);
    }

    #[test]
    fn prepared_args_validate_and_compute() {
        let prepared = prepare_edit_arguments(json!({
            "path": "legacy.txt",
            "oldText": "before",
            "newText": "after",
        }));
        let validated = validate_edit_input(&prepared).unwrap();
        let result = compute_edit_result("before\n", &validated.edits, &validated.path).unwrap();
        assert_eq!(
            result.message,
            "Successfully replaced 1 block(s) in legacy.txt."
        );
        assert_eq!(result.final_content, "after\n");
    }

    // --- compute_edit_result: CRLF / BOM handling (pi tools.test.ts) ---

    #[test]
    fn matches_lf_old_text_against_crlf_file() {
        let result = compute_edit_result(
            "line one\r\nline two\r\nline three\r\n",
            &[edit("line two\n", "replaced line\n")],
            "f.txt",
        )
        .unwrap();
        assert!(result.message.contains("Successfully replaced"));
    }

    #[test]
    fn preserves_crlf_after_edit() {
        let result = compute_edit_result(
            "first\r\nsecond\r\nthird\r\n",
            &[edit("second\n", "REPLACED\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.final_content, "first\r\nREPLACED\r\nthird\r\n");
    }

    #[test]
    fn preserves_lf_for_lf_files() {
        let result = compute_edit_result(
            "first\nsecond\nthird\n",
            &[edit("second\n", "REPLACED\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.final_content, "first\nREPLACED\nthird\n");
    }

    #[test]
    fn detects_duplicates_across_crlf_lf_variants() {
        let err = compute_edit_result(
            "hello\r\nworld\r\n---\r\nhello\nworld\n",
            &[edit("hello\nworld\n", "replaced\n")],
            "f.txt",
        )
        .unwrap_err();
        assert!(err.contains("Found 2 occurrences"));
    }

    #[test]
    fn preserves_utf8_bom_after_edit() {
        let result = compute_edit_result(
            "\u{FEFF}first\r\nsecond\r\nthird\r\n",
            &[edit("second\n", "REPLACED\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(
            result.final_content,
            "\u{FEFF}first\r\nREPLACED\r\nthird\r\n"
        );
    }

    #[test]
    fn preserves_crlf_and_bom_in_multi_edit() {
        let result = compute_edit_result(
            "\u{FEFF}first\r\nsecond\r\nthird\r\nfourth\r\n",
            &[edit("second\n", "SECOND\n"), edit("fourth\n", "FOURTH\n")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(
            result.final_content,
            "\u{FEFF}first\r\nSECOND\r\nthird\r\nFOURTH\r\n"
        );
    }
}
