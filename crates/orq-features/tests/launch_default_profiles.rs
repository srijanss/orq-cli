//! Which profile each role's launch step starts on: the same precedence a run without a choice
//! uses, then the profile matching the harness and model in `[implementer]` / `[reviewer]`.

use std::path::PathBuf;

use orq_features::{default_role_profiles, load_model_profiles, AgentProfile, ModelProfiles, TddReviewerConfig};

const MODELS: &str = "[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.opus-again]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.astra]\nharness = \"pi\"\nmodel = \"openai/astra\"\n\n[models.kimi]\nharness = \"pi\"\nmodel = \"kimi\"\n";

fn profiles(tag: &str, extra: &str) -> ModelProfiles {
    let dir: PathBuf = std::env::temp_dir().join(format!("orq-launch-defaults-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, format!("{MODELS}{extra}")).unwrap();
    load_model_profiles(&path, &dir.join("absent.toml")).unwrap()
}

fn agent(harness: &str, model: &str) -> AgentProfile {
    AgentProfile { harness: harness.into(), model: model.into() }
}

fn names(implementer: &str, reviewer: &str) -> (Option<String>, Option<String>) {
    (Some(implementer.to_string()), Some(reviewer.to_string()))
}

#[test]
fn the_implementer_and_reviewer_tables_pick_the_first_profile_with_their_harness_and_model() {
    let profiles = profiles("tables", "");

    let found = default_role_profiles(&profiles, &TddReviewerConfig::default(), Some(&agent("claude", "opus")), Some(&agent("pi", "openai/astra")));

    assert_eq!(found, names("opus", "astra"));
}

#[test]
fn roles_win_over_the_implementer_and_reviewer_tables() {
    let profiles = profiles("roles", "\n[roles]\nimplementer = \"opus-again\"\nreviewer = \"kimi\"\n");

    let found = default_role_profiles(&profiles, &TddReviewerConfig::default(), Some(&agent("claude", "opus")), Some(&agent("pi", "openai/astra")));

    assert_eq!(found, names("opus-again", "kimi"));
}

#[test]
fn the_tdd_config_reviewer_wins_when_a_profile_matches_it_as_it_does_for_a_run() {
    let profiles = profiles("tdd", "\n[roles]\nreviewer = \"kimi\"\n");
    let config = TddReviewerConfig { default_reviewer: Some(agent("pi", "openai/astra")), allowed_models: None };

    assert_eq!(default_role_profiles(&profiles, &config, None, None).1, Some("astra".to_string()));

    let unmatched = TddReviewerConfig { default_reviewer: Some(agent("pi", "no-such-model")), allowed_models: None };
    assert_eq!(default_role_profiles(&profiles, &unmatched, None, None).1, Some("kimi".to_string()));
}

#[test]
fn a_harness_and_model_must_both_match_and_nothing_configured_gives_no_default() {
    let profiles = profiles("none", "");

    assert_eq!(default_role_profiles(&profiles, &TddReviewerConfig::default(), Some(&agent("pi", "opus")), None), (None, None));
}
