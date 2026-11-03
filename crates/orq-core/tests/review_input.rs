use std::path::PathBuf;

use orq_core::ownership::{classify, may_orq_delete, Ownership};
use orq_core::review_input::{load_review_input, InputError};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("orq-review-input-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_file_reached_through_a_symlinked_directory_inside_the_root_may_not_be_deleted() {
    let d = dir("symlink");
    let root = d.join("orq");
    let outside = d.join("repo");
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Cargo.toml"), "x").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("runs")).unwrap();

    assert!(!may_orq_delete(&root, &root.join("runs/Cargo.toml")));
    assert!(may_orq_delete(&root, &root.join("real/review.json")), "a plain directory is fine, file or not yet created");
}

#[test]
fn an_exported_review_input_reports_the_rounds_it_actually_observed() {
    let d = dir("ok");
    let file = d.join("review-rounds.json");
    std::fs::write(
        &file,
        r#"{"schema_version":1,"outcome":"clean","rounds":[{"round":1,"fixed":2},{"round":2,"fixed":0}]}"#,
    )
    .unwrap();

    let input = load_review_input(&file).unwrap();

    assert_eq!(input.rounds_observed, Some(2));
    assert_eq!(input.outcome.as_deref(), Some("clean"));
}

#[test]
fn an_input_without_a_rounds_list_has_an_unknown_round_count_not_three() {
    let d = dir("unknown");
    let file = d.join("review-rounds.json");
    std::fs::write(&file, r#"{"schema_version":1,"outcome":"clean"}"#).unwrap();

    let input = load_review_input(&file).unwrap();

    assert_eq!(input.rounds_observed, None);
}

#[test]
fn an_outcome_that_is_present_but_not_text_is_malformed_while_null_means_absent() {
    let d = dir("outcome");
    let file = d.join("review-rounds.json");

    std::fs::write(&file, r#"{"schema_version":1,"outcome":42}"#).unwrap();
    assert!(matches!(load_review_input(&file), Err(InputError::Malformed(reason)) if reason.contains("outcome")));

    std::fs::write(&file, r#"{"schema_version":1,"outcome":null}"#).unwrap();
    assert_eq!(load_review_input(&file).unwrap().outcome, None);
}

#[test]
fn an_empty_rounds_list_is_a_known_zero_and_a_rounds_value_that_is_not_a_list_is_malformed() {
    let d = dir("rounds-edges");
    let file = d.join("review-rounds.json");
    let load = |body: &str| {
        std::fs::write(&file, body).unwrap();
        load_review_input(&file)
    };

    assert_eq!(load(r#"{"schema_version":1,"rounds":[]}"#).unwrap().rounds_observed, Some(0));
    for rounds in ["null", "3", r#""three""#, "{}"] {
        let result = load(&format!(r#"{{"schema_version":1,"rounds":{rounds}}}"#));
        assert!(matches!(result, Err(InputError::Malformed(_))), "{rounds}: {result:?}");
    }
    for version in ["\"1\"", "null", "0", "1.5"] {
        let result = load(&format!(r#"{{"schema_version":{version}}}"#));
        assert!(matches!(result, Err(InputError::Malformed(_))), "{version}: {result:?}");
    }
    assert!(matches!(load(r#"{"rounds":[]}"#), Err(InputError::Malformed(_))), "a missing schema_version");
}

#[test]
fn a_missing_malformed_unsupported_or_inaccessible_input_is_reported_not_guessed() {
    let d = dir("bad");
    assert_eq!(load_review_input(&d.join("nope.json")), Err(InputError::Missing));

    let malformed = d.join("malformed.json");
    std::fs::write(&malformed, "{").unwrap();
    assert!(matches!(load_review_input(&malformed), Err(InputError::Malformed(_))));

    let future = d.join("future.json");
    std::fs::write(&future, r#"{"schema_version":99}"#).unwrap();
    assert!(matches!(load_review_input(&future), Err(InputError::Malformed(reason)) if reason.contains("schema_version")));

    // A directory where a file is expected cannot be read.
    assert!(matches!(load_review_input(&d), Err(InputError::Inaccessible(_))));
}

#[test]
fn mcp_and_project_files_are_never_orq_deletable_while_orq_data_is() {
    assert_eq!(classify(".tdd-features.json"), Ownership::Project);
    for mcp in [".tdd-state.json", ".tdd-review-findings.json", ".tdd-review-rounds.json", ".project-mcp", ".mcpctl.toml", ".mcp.json"] {
        assert_eq!(classify(mcp), Ownership::Mcp, "{mcp}");
    }
    assert_eq!(classify("review.json"), Ownership::Unowned);
}

#[test]
fn only_files_inside_an_orq_root_that_are_not_tool_files_may_be_deleted() {
    let root = std::path::Path::new("/data/orq");
    assert!(may_orq_delete(root, &root.join("runs/run-1/review.json")));
    // Ordinary project files are not Orq's, wherever they are.
    assert!(!may_orq_delete(root, std::path::Path::new("/repo/Cargo.toml")));
    assert!(!may_orq_delete(root, std::path::Path::new("/repo/src/main.rs")));
    // Tool files stay protected even inside the root, and the root itself is not a file to delete.
    assert!(!may_orq_delete(root, &root.join(".tdd-state.json")));
    assert!(!may_orq_delete(root, root));
    // A path that climbs out of the root is refused.
    assert!(!may_orq_delete(root, &root.join("../other/file")));
    assert!(!may_orq_delete(root, &root.join("runs/../../escape")));
}
