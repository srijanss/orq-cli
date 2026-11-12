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

[models.mystery]
harness = "bogus"
model = "m1"

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
fn policy_errors_are_typed_for_unknown_names_unknown_implementer_harnesses_and_unlisted_reviewers() {
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
        resolve_run_models(&configured, &none, &pick(Some("mystery"), None)),
        Err(RunModelsError::ImplementerHarnessUnknown { ref harness, .. }) if harness == "bogus"
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

#[test]
fn a_pi_profile_is_accepted_as_the_implementer_from_the_flag_and_from_the_roles_table() {
    let flagged = resolve_run_models(&profiles("pi-flag", USER), &config(None, None), &pick(Some("kimi"), None)).unwrap();
    assert_eq!((flagged.implementer.harness.as_str(), flagged.implementer.model.as_str()), ("pi", "kimi-k3"));

    let roles = USER.replace("implementer = \"opus\"", "implementer = \"gpt-6-astra\"");
    let from_roles = resolve_run_models(&profiles("pi-role", &roles), &config(None, None), &pick(None, None)).unwrap();
    assert_eq!(from_roles.implementer.harness, "pi");
    assert_eq!(from_roles.implementer.thinking.as_deref(), Some("medium"));
}

const MACHINE: &str = r#"
[models.opus]
harness = "claude"
model = "opus"

[implementer]
harness = "claude"
model = "sonnet"
effort = "high"

[reviewer]
harness = "pi"
model = "vendor/checker"
effort = "low"
"#;

#[test]
fn the_machine_defaults_come_after_roles_and_tdd_config_and_before_the_built_in() {
    let resolved = |text: &str, tag: &str, reviewers: &TddReviewerConfig| {
        resolve_run_models(&profiles(tag, text), reviewers, &RunSelection::default()).unwrap()
    };
    let plain = resolved(MACHINE, "machine-plain", &config(None, None));
    assert_eq!((plain.implementer.harness.as_str(), plain.implementer.model.as_str(), plain.implementer.thinking.as_deref()), ("claude", "sonnet", Some("high")));
    assert_eq!((plain.reviewer.harness.as_str(), plain.reviewer.model.as_str(), plain.reviewer.thinking.as_deref()), ("pi", "vendor/checker", Some("low")));

    let with_roles = format!("{MACHINE}\n[roles]\nimplementer = \"opus\"\nreviewer = \"opus\"\n");
    let roles = resolved(&with_roles, "machine-roles", &config(None, None));
    assert_eq!(roles.implementer.model, "opus", "[roles] wins over the machine default");
    assert_eq!(roles.reviewer.model, "opus", "[roles] wins over the machine default");

    let tdd = resolved(MACHINE, "machine-tdd", &config(Some(("pi", "kimi-k3")), None));
    assert_eq!(tdd.reviewer.model, "kimi-k3", ".tdd-config.json wins over the machine default");

    // Settings written before the rename keep the model under `profile`.
    let legacy = resolved("[implementer]\nharness = \"claude\"\nprofile = \"haiku\"\n", "machine-legacy", &config(None, None));
    assert_eq!(legacy.implementer.model, "haiku");
    assert_eq!(legacy.implementer.thinking, None);
}

#[test]
fn a_half_filled_machine_default_is_an_error_only_for_a_run_that_falls_back_to_it() {
    let loaded = profiles("machine-half", "[models.kimi]\nharness = \"pi\"\nmodel = \"kimi-k3\"\n\n[reviewer]\nharness = \"pi\"\n");
    let error = resolve_run_models(&loaded, &config(None, None), &RunSelection::default()).unwrap_err();
    assert!(error.to_string().contains("reviewer model is missing"), "{error}");

    let chosen = RunSelection { implementer: None, reviewer: Some("kimi") };
    let resolved = resolve_run_models(&loaded, &config(None, None), &chosen).unwrap();
    assert_eq!(resolved.reviewer.model, "kimi-k3", "a run that names its reviewer never reads the table");
}

#[test]
fn a_project_s_own_agents_come_after_the_flags_and_before_every_configured_source() {
    use orq_features::{resolve_project_run_models, ProjectAgents, ResolvedModel};
    let model = |harness: &str, model: &str, thinking: Option<&str>| ResolvedModel { harness: harness.into(), model: model.into(), thinking: thinking.map(str::to_string) };
    let project = ProjectAgents { implementer: Some(model("claude", "haiku", Some("low"))), reviewer: Some(model("pi", "vendor/project", Some("high"))) };
    let configured = profiles("project-agents", &format!("{USER}\n[implementer]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[reviewer]\nharness = \"pi\"\nmodel = \"vendor/checker\"\n"));
    let tdd = config(Some(("pi", "kimi-k3")), None);

    let resolved = resolve_project_run_models(&configured, &tdd, &RunSelection::default(), &project).unwrap();
    assert_eq!(resolved.implementer, model("claude", "haiku", Some("low")), "before [roles] and the machine default");
    assert_eq!(resolved.reviewer, model("pi", "vendor/project", Some("high")), "before .tdd-config.json, [roles] and the machine default");

    let flagged = resolve_project_run_models(&configured, &tdd, &pick(Some("sonnet"), Some("kimi")), &project).unwrap();
    assert_eq!((flagged.implementer.model.as_str(), flagged.reviewer.model.as_str()), ("sonnet", "kimi-k3"), "a flag still wins");

    let unset = ProjectAgents { implementer: None, reviewer: None };
    let fallback = resolve_project_run_models(&configured, &tdd, &RunSelection::default(), &unset).unwrap();
    assert_eq!((fallback.implementer.model.as_str(), fallback.reviewer.model.as_str()), ("opus", "kimi-k3"), "an unset role falls back");

    let allowed = config(None, Some(&["kimi-k3"]));
    assert!(matches!(
        resolve_project_run_models(&configured, &allowed, &RunSelection::default(), &project),
        Err(RunModelsError::ReviewerNotAllowed { model, .. }) if model == "vendor/project"
    ));
    let bogus = ProjectAgents { implementer: Some(model("bogus", "m1", None)), reviewer: None };
    assert!(matches!(
        resolve_project_run_models(&configured, &tdd, &RunSelection::default(), &bogus),
        Err(RunModelsError::ImplementerHarnessUnknown { .. })
    ));
}
