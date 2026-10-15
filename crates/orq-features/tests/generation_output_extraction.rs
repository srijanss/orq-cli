use std::fs;

use orq_features::{extract_candidates, ExtractError};

#[test]
fn a_bare_json_array_is_extracted() {
    let out = r#"[{"featureName":"a","description":"d","dependsOn":["b"],"testFile":"-p x --test t","parallelizable":true},{"featureName":"b"}]"#;

    let candidates = extract_candidates(out).unwrap();

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].draft.feature_name, "a");
    assert_eq!(candidates[0].draft.depends_on.as_deref(), Some(&["b".to_string()][..]));
    assert_eq!(candidates[0].draft.parallelizable, Some(true));
    assert_eq!(candidates[1].draft.parallelizable, None);
}

#[test]
fn an_array_inside_a_markdown_fence_or_prose_is_extracted() {
    let fenced = "Here you go:\n```json\n[{\"featureName\":\"a\"}]\n```\nDone.";
    let prose = "Sure! [{\"featureName\":\"a\"}] hope that helps";

    assert_eq!(extract_candidates(fenced).unwrap().len(), 1);
    assert_eq!(extract_candidates(prose).unwrap().len(), 1);
}

#[test]
fn invalid_json_and_wrong_shapes_are_rejected() {
    for bad in [
        "",
        "no json at all",
        "[{\"featureName\":",
        r#"{"featureName":"a"}"#,
        r#"["a"]"#,
        r#"[{"status":"draft"}]"#,
        r#"[{"featureName":""}]"#,
        r#"[{"featureName":5}]"#,
        "[{\"featureName\":\"a\\u001b[31m\"}]",
        r#"[{"featureName":"a","dependsOn":"b"}]"#,
        r#"[{"featureName":"a","dependsOn":[1]}]"#,
        r#"[{"featureName":"a","parallelizable":"yes"}]"#,
        r#"[{"featureName":"a","description":["x"]}]"#,
        r#"[{"featureName":"a","testFile":7}]"#,
        r#"[{"featureName":"a","targetFiles":"src"}]"#,
    ] {
        assert!(extract_candidates(bad).is_err(), "accepted: {bad}");
    }
    assert!(matches!(extract_candidates("nothing"), Err(ExtractError::NoJson)));
}

#[test]
fn rejection_leaves_an_existing_ledger_untouched() {
    let dir = std::env::temp_dir().join(format!("orq-extract-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let ledger = dir.join(".tdd-features.json");
    let original = r#"[{"featureName":"old","status":"pending"}]"#;
    fs::write(&ledger, original).unwrap();

    assert!(extract_candidates("[{\"featureName\":5}]").is_err());

    assert_eq!(fs::read_to_string(&ledger).unwrap(), original);
}

#[test]
fn no_field_is_ever_executed_and_testfile_is_kept_verbatim() {
    let dir = std::env::temp_dir().join(format!("orq-extract-exec-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("ran");
    let test_file = format!("touch {}; $(touch {})", marker.display(), marker.display());
    let out = serde_json::json!([{"featureName": "x", "testFile": test_file, "description": test_file}]).to_string();

    let candidates = extract_candidates(&out).unwrap();

    assert_eq!(candidates[0].draft.test_file.as_deref(), Some(test_file.as_str()));
    assert!(!marker.exists());
}

#[test]
fn ledger_owned_fields_are_dropped_and_reported() {
    let out = r#"[{"featureName":"a","status":"completed","recordedAt":"2026-01-01","cyclesCompleted":9}]"#;

    let candidates = extract_candidates(out).unwrap();

    assert_eq!(candidates[0].ignored_fields, ["status", "recordedAt", "cyclesCompleted"]);
    assert_eq!(candidates[0].draft.to_json(), r#"{"featureName":"a","status":"draft"}"#);
}
