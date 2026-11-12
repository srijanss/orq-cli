use std::path::{Path, PathBuf};

use orq_features::{load_model_profiles, AgentRole, FieldProblem, ModelProfilesError};

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

const USER: &str = r#"
# MVP 1.1 role tables keep working and are not model profiles
[implementer]
harness = "claude"
model = "opus"

[reviewer]
harness = "pi"
model = "openai/gpt-6-astra"

[models.opus]
harness = "claude"
model = "opus"
provider = "anthropic"

[models.gpt-6-astra]
harness = "pi"
model = "openai/gpt-6-astra"
thinking = "medium"
provider = "openai"

[roles]
implementer = "opus"
reviewer = "gpt-6-astra"
"#;

#[test]
fn profiles_and_roles_load_in_file_order_with_optional_fields() {
    let dir = tempfile_dir("load");
    let user = write(&dir, "config.toml", USER);
    let profiles = load_model_profiles(&user, &dir.join("absent.toml")).unwrap();

    let names: Vec<&str> = profiles.profiles().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["opus", "gpt-6-astra"]);

    let opus = profiles.get("opus").unwrap();
    assert_eq!(
        (opus.harness.as_str(), opus.model.as_str(), opus.thinking.as_deref(), opus.provider.as_deref()),
        ("claude", "opus", None, Some("anthropic"))
    );
    let astra = profiles.get("gpt-6-astra").unwrap();
    assert_eq!(astra.thinking.as_deref(), Some("medium"));

    assert_eq!(profiles.role_profile_name(AgentRole::Implementer), Some("opus"));
    assert_eq!(profiles.role(AgentRole::Reviewer).unwrap().model, "openai/gpt-6-astra");
    assert!(profiles.get("missing").is_none());
}

#[test]
fn absent_files_give_no_profiles_and_no_error() {
    let dir = tempfile_dir("absent");
    let profiles = load_model_profiles(&dir.join("a.toml"), &dir.join("b.toml")).unwrap();
    assert!(profiles.profiles().is_empty());
    assert_eq!(profiles.role(AgentRole::Implementer), None);
    assert_eq!(profiles.role_profile_name(AgentRole::Reviewer), None);
}

#[test]
fn a_config_with_only_the_mvp_1_1_role_tables_has_no_model_profiles() {
    let dir = tempfile_dir("legacy");
    let user = write(
        &dir,
        "config.toml",
        "[implementer]\nharness = \"claude\"\nmodel = \"opus\"\n",
    );
    let profiles = load_model_profiles(&user, &dir.join("absent.toml")).unwrap();
    assert!(profiles.profiles().is_empty());
}

#[test]
fn the_repo_file_replaces_a_profile_by_name_adds_new_ones_and_overrides_roles_one_by_one() {
    let dir = tempfile_dir("layer");
    let user = write(&dir, "config.toml", USER);
    let repo = write(
        &dir,
        ".orq.toml",
        r#"
[models.opus]
harness = "claude"
model = "opus-next"

[models.sonnet]
harness = "claude"
model = "sonnet"

[roles]
reviewer = "sonnet"
"#,
    );
    let profiles = load_model_profiles(&user, &repo).unwrap();

    let names: Vec<&str> = profiles.profiles().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["opus", "gpt-6-astra", "sonnet"]);

    let opus = profiles.get("opus").unwrap();
    assert_eq!(opus.model, "opus-next");
    assert_eq!(opus.provider, None, "an overriding profile replaces the whole profile");

    assert_eq!(profiles.role_profile_name(AgentRole::Implementer), Some("opus"));
    assert_eq!(profiles.role_profile_name(AgentRole::Reviewer), Some("sonnet"));
    assert_eq!(profiles.role(AgentRole::Implementer).unwrap().model, "opus-next");
}

#[test]
fn a_malformed_file_is_reported_with_its_path_and_never_rewritten() {
    let dir = tempfile_dir("malformed");
    let user = write(&dir, "config.toml", USER);
    let broken_text = "[models.opus\nharness = ";
    let repo = write(&dir, ".orq.toml", broken_text);
    match load_model_profiles(&user, &repo) {
        Err(ModelProfilesError::Malformed { path, .. }) => assert_eq!(path, repo),
        other => panic!("expected Malformed, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&repo).unwrap(), broken_text);
    assert_eq!(std::fs::read_to_string(&user).unwrap(), USER);
}

#[test]
fn invalid_profile_fields_name_the_file_profile_and_field() {
    let dir = tempfile_dir("invalid");
    let cases = [
        ("[models.a]\nmodel = \"m\"\n", "harness", FieldProblem::Missing),
        ("[models.a]\nharness = \"  \"\nmodel = \"m\"\n", "harness", FieldProblem::Blank),
        ("[models.a]\nharness = \"claude\"\nmodel = 3\n", "model", FieldProblem::NotASingleLineString),
        ("[models.a]\nharness = \"claude\"\nmodel = \"m\\nx\"\n", "model", FieldProblem::NotASingleLineString),
        (
            "[models.a]\nharness = \"claude\"\nmodel = \"m\"\nthinking = [\"high\"]\n",
            "thinking",
            FieldProblem::NotASingleLineString,
        ),
        (
            "[models.a]\nharness = \"claude\"\nmodel = \"m\"\nprovider = \"\"\n",
            "provider",
            FieldProblem::Blank,
        ),
    ];
    for (i, (text, field, problem)) in cases.into_iter().enumerate() {
        let user = write(&dir, &format!("config{i}.toml"), text);
        assert_eq!(
            load_model_profiles(&user, &dir.join("absent.toml")).unwrap_err(),
            ModelProfilesError::InvalidField {
                path: user.clone(),
                profile: "a".to_string(),
                field,
                problem,
            },
            "{text:?}"
        );
    }
}

#[test]
fn a_profile_that_is_not_a_table_and_a_bad_models_section_are_rejected() {
    let dir = tempfile_dir("shapes");
    let user = write(&dir, "config.toml", "[models]\na = \"claude\"\n");
    assert_eq!(
        load_model_profiles(&user, &dir.join("absent.toml")).unwrap_err(),
        ModelProfilesError::NotAProfile {
            path: user.clone(),
            profile: "a".to_string()
        }
    );
    let user = write(&dir, "config2.toml", "models = \"x\"\n");
    assert_eq!(
        load_model_profiles(&user, &dir.join("absent.toml")).unwrap_err(),
        ModelProfilesError::NotAProfile {
            path: user.clone(),
            profile: "models".to_string()
        }
    );
}

#[test]
fn a_role_must_name_a_string_that_matches_a_profile() {
    let dir = tempfile_dir("roles");
    let user = write(
        &dir,
        "config.toml",
        "[models.a]\nharness = \"claude\"\nmodel = \"m\"\n[roles]\nimplementer = \"ghost\"\n",
    );
    assert_eq!(
        load_model_profiles(&user, &dir.join("absent.toml")).unwrap_err(),
        ModelProfilesError::UnknownRoleProfile {
            role: AgentRole::Implementer,
            profile: "ghost".to_string()
        }
    );
    let user = write(
        &dir,
        "config2.toml",
        "[models.a]\nharness = \"claude\"\nmodel = \"m\"\n[roles]\nreviewer = 7\n",
    );
    assert_eq!(
        load_model_profiles(&user, &dir.join("absent.toml")).unwrap_err(),
        ModelProfilesError::InvalidRole {
            path: user.clone(),
            role: AgentRole::Reviewer
        }
    );
}

#[test]
fn an_unreadable_path_is_an_io_error_not_an_absent_file() {
    let dir = tempfile_dir("io");
    match load_model_profiles(&dir, &dir.join("absent.toml")) {
        Err(ModelProfilesError::Io { path, .. }) => assert_eq!(path, dir),
        other => panic!("expected Io, got {other:?}"),
    }
}

#[test]
fn profile_names_a_role_could_not_reference_are_rejected() {
    let dir = tempfile_dir("names");
    for name in [" opus ", "opus ", " opus", "", "a\\nb"] {
        let user = write(
            &dir,
            "config.toml",
            &format!("[models.\"{name}\"]\nharness = \"claude\"\nmodel = \"m\"\n"),
        );
        assert_eq!(
            load_model_profiles(&user, &dir.join("absent.toml")).unwrap_err(),
            ModelProfilesError::InvalidProfileName {
                path: user.clone(),
                profile: name.replace("\\n", "\n"),
            },
            "{name:?}"
        );
    }
    let user = write(&dir, "config.toml", "[models.\"my model\"]\nharness = \"claude\"\nmodel = \"m\"\n");
    assert!(load_model_profiles(&user, &dir.join("absent.toml")).unwrap().get("my model").is_some());
}

#[test]
fn role_references_are_checked_after_both_files_are_merged() {
    let dir = tempfile_dir("cross-layer");
    let absent = dir.join("absent.toml");
    let model = "harness = \"claude\"\nmodel = \"m\"\n";

    // A user role may name a profile only the repo file defines.
    let user = write(&dir, "u1.toml", "[roles]\nimplementer = \"repo-only\"\n");
    let repo = write(&dir, "r1.toml", &format!("[models.repo-only]\n{model}"));
    let merged = load_model_profiles(&user, &repo).unwrap();
    assert_eq!(merged.role(AgentRole::Implementer).unwrap().name, "repo-only");

    // A dangling user role is fine once the repo file replaces it.
    let user = write(&dir, "u2.toml", "[roles]\nreviewer = \"ghost\"\n");
    let repo = write(&dir, "r2.toml", &format!("[models.real]\n{model}[roles]\nreviewer = \"real\"\n"));
    let merged = load_model_profiles(&user, &repo).unwrap();
    assert_eq!(merged.role_profile_name(AgentRole::Reviewer), Some("real"));

    // A role still dangling after the merge is rejected.
    let user = write(&dir, "u3.toml", "[roles]\nreviewer = \"ghost\"\n");
    let repo = write(&dir, "r3.toml", &format!("[models.real]\n{model}"));
    assert_eq!(
        load_model_profiles(&user, &repo).unwrap_err(),
        ModelProfilesError::UnknownRoleProfile {
            role: AgentRole::Reviewer,
            profile: "ghost".to_string()
        }
    );

    // A repo role naming a profile neither file defines is rejected too.
    let user = write(&dir, "u4.toml", &format!("[models.real]\n{model}"));
    let repo = write(&dir, "r4.toml", "[roles]\nimplementer = \"ghost\"\n");
    assert!(matches!(
        load_model_profiles(&user, &repo),
        Err(ModelProfilesError::UnknownRoleProfile { role: AgentRole::Implementer, .. })
    ));
}

fn tempfile_dir(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!(
        "orq-model-profiles-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
