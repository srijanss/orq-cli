use std::fs;
use std::path::PathBuf;

use orq_features::{load_reviewer_config, resolve_reviewer, AgentProfile, EffectiveReviewer, ReviewerConfigError};

fn config_file(label: &str, text: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-tddcfg-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-config.json");
    if let Some(text) = text {
        fs::write(&path, text).unwrap();
    }
    path
}

fn model(harness: &str, name: &str) -> AgentProfile {
    AgentProfile { harness: harness.into(), model: name.into() }
}

const FULL: &str = r#"{"adapter":"x","reviewers":{"default":{"reviewer":"pi","model":"openai/gpt-6-astra","thinking":"medium"},"allowedModels":["openai/gpt-6-astra","opus"]}}"#;

#[test]
fn a_missing_config_says_nothing_and_is_not_an_error() {
    let config = load_reviewer_config(&config_file("missing", None)).unwrap();

    assert_eq!(config.default_reviewer, None);
    assert_eq!(config.allowed_models, None);
}

#[test]
fn reviewers_default_and_allowed_models_are_read() {
    let config = load_reviewer_config(&config_file("full", Some(FULL))).unwrap();

    assert_eq!(config.default_reviewer, Some(model("pi", "openai/gpt-6-astra")));
    assert_eq!(config.allowed_models, Some(vec!["openai/gpt-6-astra".to_string(), "opus".to_string()]));
}

#[test]
fn a_config_without_a_reviewers_section_says_nothing() {
    let config = load_reviewer_config(&config_file("none", Some(r#"{"adapter":"x"}"#))).unwrap();

    assert_eq!(config.default_reviewer, None);
    assert_eq!(config.allowed_models, None);
}

#[test]
fn malformed_or_mistyped_config_is_an_error_not_silence() {
    for (label, text) in [
        ("garbage", "{nope"),
        ("allowed-type", r#"{"reviewers":{"allowedModels":"opus"}}"#),
        ("allowed-item", r#"{"reviewers":{"allowedModels":[1]}}"#),
        ("default-type", r#"{"reviewers":{"default":"pi"}}"#),
    ] {
        let error = load_reviewer_config(&config_file(label, Some(text))).unwrap_err();
        assert!(matches!(error, ReviewerConfigError::Malformed { .. }), "{label}: {error:?}");
    }
}

#[test]
fn loading_never_modifies_the_config() {
    let path = config_file("readonly", Some(FULL));
    let before = fs::read(&path).unwrap();

    load_reviewer_config(&path).unwrap();

    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn the_config_default_wins_over_a_different_settings_reviewer() {
    let config = load_reviewer_config(&config_file("wins", Some(FULL))).unwrap();

    let resolved = resolve_reviewer(Some(&model("claude", "opus")), &config);

    assert_eq!(resolved.effective, EffectiveReviewer::Config(model("pi", "openai/gpt-6-astra")));
    assert!(resolved.overrides_settings);
    assert_eq!(resolved.invalid, None);
}

#[test]
fn a_settings_reviewer_equal_to_the_config_default_is_not_an_override() {
    let config = load_reviewer_config(&config_file("same", Some(FULL))).unwrap();

    let resolved = resolve_reviewer(Some(&model("pi", "openai/gpt-6-astra")), &config);

    assert!(!resolved.overrides_settings);
}

#[test]
fn settings_apply_when_the_config_names_no_default() {
    let config = load_reviewer_config(&config_file("settings", Some(r#"{"reviewers":{"allowedModels":["opus"]}}"#))).unwrap();

    let resolved = resolve_reviewer(Some(&model("claude", "opus")), &config);

    assert_eq!(resolved.effective, EffectiveReviewer::Settings(model("claude", "opus")));
    assert!(!resolved.overrides_settings);
    assert_eq!(resolved.invalid, None);
}

#[test]
fn nothing_configured_anywhere_is_unset() {
    let config = load_reviewer_config(&config_file("unset", None)).unwrap();

    let resolved = resolve_reviewer(None, &config);

    assert_eq!(resolved.effective, EffectiveReviewer::Unset);
    assert_eq!(resolved.invalid, None);
}

#[test]
fn a_settings_reviewer_outside_allowed_models_is_flagged_invalid() {
    let config = load_reviewer_config(&config_file("outside", Some(r#"{"reviewers":{"allowedModels":["opus"]}}"#))).unwrap();

    let resolved = resolve_reviewer(Some(&model("pi", "gpt-9")), &config);

    let reason = resolved.invalid.expect("flagged");
    assert!(reason.contains("gpt-9") && reason.contains("allowedModels"), "{reason}");
}

#[test]
fn the_config_default_is_checked_against_allowed_models_too() {
    let text = r#"{"reviewers":{"default":{"reviewer":"pi","model":"gpt-9"},"allowedModels":["opus"]}}"#;
    let config = load_reviewer_config(&config_file("default-outside", Some(text))).unwrap();

    let resolved = resolve_reviewer(None, &config);

    assert!(resolved.invalid.is_some());
}

#[test]
fn an_absent_allowed_models_list_restricts_nothing() {
    let config = load_reviewer_config(&config_file("open", Some(r#"{"reviewers":{}}"#))).unwrap();

    let resolved = resolve_reviewer(Some(&model("pi", "anything")), &config);

    assert_eq!(resolved.invalid, None);
}
