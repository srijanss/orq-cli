use std::path::PathBuf;

use orq_features::{
    load_model_profiles, resolve_run_models, AgentProfile, ModelProfiles, RunModelsError, RunSelection,
    TddReviewerConfig,
};

const USER: &str = r#"
[models.opus]
harness = "claude"
model = "opus"

[models.sonnet]
harness = "claude"
model = "sonnet"
thinking = "high"

[models.gpt-6-astra]
harness = "pi"
model = "openai/gpt-6-astra"
thinking = "medium"

[models.kimi]
harness = "pi"
model = "kimi-k3"

[roles]
implementer = "opus"
reviewer = "gpt-6-astra"
"#;

fn profiles(tag: &str, text: &str) -> ModelProfiles {
    let dir: PathBuf = orq_test_support::process_temp_dir().join(format!("orq-run-models-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), text).unwrap();
    load_model_profiles(&dir.join("config.toml"), &dir.join("absent.toml")).unwrap()
}

fn config(default: Option<(&str, &str)>, allowed: Option<&[&str]>) -> TddReviewerConfig {
    TddReviewerConfig {
        default_reviewer: default.map(|(harness, model)| AgentProfile {
            harness: harness.to_string(),
            model: model.to_string(),
        }),
        allowed_models: allowed.map(|models| models.iter().map(|m| m.to_string()).collect()),
    }
}

fn pick(implementer: Option<&str>, reviewer: Option<&str>) -> RunSelection<'static> {
    RunSelection {
        implementer: implementer.map(|s| Box::leak(s.to_string().into_boxed_str()) as &'static str),
        reviewer: reviewer.map(|s| Box::leak(s.to_string().into_boxed_str()) as &'static str),
    }
}

#[test]
fn the_implementer_comes_from_the_flag_then_the_roles_table_then_the_built_in() {
    let configured = profiles("impl", USER);
    let none = config(None, None);

    let by_role = resolve_run_models(&configured, &none, &pick(None, None)).unwrap();
    assert_eq!((by_role.implementer.harness.as_str(), by_role.implementer.model.as_str()), ("claude", "opus"));

    let by_flag = resolve_run_models(&configured, &none, &pick(Some("sonnet"), None)).unwrap();
    assert_eq!(by_flag.implementer.model, "sonnet");
    assert_eq!(by_flag.implementer.thinking.as_deref(), Some("high"));

    let bare = profiles("impl-bare", "");
    let built_in = resolve_run_models(&bare, &none, &pick(None, None)).unwrap();
    assert_eq!(built_in.implementer.harness, "claude");
    assert!(!built_in.implementer.model.is_empty());
}

#[test]
fn the_reviewer_comes_from_the_flag_then_tdd_config_then_roles_then_the_built_in() {
    let configured = profiles("rev", USER);

    let by_role = resolve_run_models(&configured, &config(None, None), &pick(None, None)).unwrap();
    assert_eq!(by_role.reviewer.model, "openai/gpt-6-astra");
    assert_eq!(by_role.reviewer.thinking.as_deref(), Some("medium"));

    // reviewers.default is used directly (harness + model), needing no profile, and beats [roles].
    let by_config = resolve_run_models(&configured, &config(Some(("pi", "deepseek-r2")), None), &pick(None, None))
        .unwrap();
    assert_eq!((by_config.reviewer.harness.as_str(), by_config.reviewer.model.as_str()), ("pi", "deepseek-r2"));
    assert_eq!(by_config.reviewer.thinking, None);

    // The flag beats both.
    let by_flag = resolve_run_models(&configured, &config(Some(("pi", "deepseek-r2")), None), &pick(None, Some("kimi")))
        .unwrap();
    assert_eq!(by_flag.reviewer.model, "kimi-k3");

    let bare = profiles("rev-bare", "");
    let built_in = resolve_run_models(&bare, &config(None, None), &pick(None, None)).unwrap();
    assert!(!built_in.reviewer.harness.is_empty() && !built_in.reviewer.model.is_empty());
}

#[test]
fn policy_errors_are_typed_for_unknown_names_non_claude_implementers_and_unlisted_reviewers() {
    let configured = profiles("errors", USER);
    let none = config(None, None);

    assert!(matches!(
        resolve_run_models(&configured, &none, &pick(Some("nope"), None)),
        Err(RunModelsError::UnknownProfile { .. })
    ));
    assert!(matches!(
        resolve_run_models(&configured, &none, &pick(None, Some("nope"))),
        Err(RunModelsError::UnknownProfile { .. })
    ));
    assert!(matches!(
        resolve_run_models(&configured, &none, &pick(Some("kimi"), None)),
        Err(RunModelsError::ImplementerNotClaude { .. })
    ));

    // allowedModels overrides everything, whichever source chose the reviewer.
    let allowed = config(None, Some(&["openai/gpt-6-astra"]));
    assert!(resolve_run_models(&configured, &allowed, &pick(None, None)).is_ok());
    assert!(matches!(
        resolve_run_models(&configured, &allowed, &pick(None, Some("kimi"))),
        Err(RunModelsError::ReviewerNotAllowed { .. })
    ));
    let default_unlisted = config(Some(("pi", "deepseek-r2")), Some(&["openai/gpt-6-astra"]));
    assert!(matches!(
        resolve_run_models(&configured, &default_unlisted, &pick(None, None)),
        Err(RunModelsError::ReviewerNotAllowed { .. })
    ));
}
