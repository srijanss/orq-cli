use std::fs;

use orq_features::{load_reviewer_config, ReviewerConfigError};

fn load(label: &str, text: &str) -> Result<orq_features::TddReviewerConfig, ReviewerConfigError> {
    let dir = std::env::temp_dir().join(format!("orq-strict-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-config.json");
    fs::write(&path, text).unwrap();
    load_reviewer_config(&path)
}

#[test]
fn present_but_unusable_shapes_are_errors_not_silence() {
    for (label, text) in [
        ("root-array", "[]"),
        ("reviewers-string", r#"{"reviewers":"pi"}"#),
        ("reviewers-null", r#"{"reviewers":null}"#),
        ("model-number", r#"{"reviewers":{"default":{"reviewer":"pi","model":3}}}"#),
        ("model-missing", r#"{"reviewers":{"default":{"reviewer":"pi"}}}"#),
        ("reviewer-missing", r#"{"reviewers":{"default":{"model":"opus"}}}"#),
        ("model-blank", r#"{"reviewers":{"default":{"reviewer":"pi","model":"  "}}}"#),
        ("model-control", r#"{"reviewers":{"default":{"reviewer":"pi","model":"a\nb"}}}"#),
        ("allowed-blank", r#"{"reviewers":{"allowedModels":[""]}}"#),
    ] {
        let result = load(label, text);
        assert!(matches!(result, Err(ReviewerConfigError::Malformed { .. })), "{label}: {result:?}");
    }
}

#[test]
fn an_empty_default_object_is_an_error_too() {
    let result = load("empty-default", r#"{"reviewers":{"default":{}}}"#);

    assert!(matches!(result, Err(ReviewerConfigError::Malformed { .. })), "{result:?}");
}
