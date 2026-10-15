use std::fs;
use std::path::PathBuf;

use orq_features::{AgentProfile, AgentRole, ProfileError, Settings};

fn path(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-profiles-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir.join("settings.toml")
}

fn profile(harness: &str, name: &str) -> AgentProfile {
    AgentProfile { harness: harness.into(), profile: name.into() }
}

#[test]
fn nothing_is_configured_by_default() {
    let settings = Settings::load(&path("empty")).unwrap();

    assert_eq!(settings.agent(AgentRole::Implementer).unwrap(), None);
    assert_eq!(settings.agent(AgentRole::Reviewer).unwrap(), None);
    assert_eq!(settings.generation_agent().unwrap(), None);
}

#[test]
fn roles_round_trip_independently_through_a_save() {
    let path = path("roundtrip");
    let mut settings = Settings::load(&path).unwrap();
    settings.set_agent(AgentRole::Implementer, "claude", "opus").unwrap();
    settings.set_agent(AgentRole::Reviewer, "pi", "gpt-6-astra").unwrap();
    settings.save().unwrap();

    let reloaded = Settings::load(&path).unwrap();

    assert_eq!(reloaded.agent(AgentRole::Implementer).unwrap(), Some(profile("claude", "opus")));
    assert_eq!(reloaded.agent(AgentRole::Reviewer).unwrap(), Some(profile("pi", "gpt-6-astra")));
}

#[test]
fn feature_generation_uses_the_implementer_entry() {
    let mut settings = Settings::load(&path("generation")).unwrap();
    settings.set_agent(AgentRole::Implementer, "claude", "opus").unwrap();
    settings.set_agent(AgentRole::Reviewer, "pi", "gpt").unwrap();

    assert_eq!(settings.generation_agent().unwrap(), Some(profile("claude", "opus")));
}

#[test]
fn blank_values_are_rejected_and_change_nothing() {
    let mut settings = Settings::load(&path("blank")).unwrap();
    settings.set_agent(AgentRole::Implementer, "claude", "opus").unwrap();

    for (harness, name) in [("", "opus"), ("claude", ""), ("  ", "opus"), ("claude", "\t")] {
        let error = settings.set_agent(AgentRole::Implementer, harness, name).unwrap_err();
        assert!(matches!(error, ProfileError::Blank { .. }), "{error:?}");
    }
    assert_eq!(settings.agent(AgentRole::Implementer).unwrap(), Some(profile("claude", "opus")));
}

#[test]
fn values_with_line_breaks_are_rejected() {
    let mut settings = Settings::load(&path("newline")).unwrap();

    let error = settings.set_agent(AgentRole::Reviewer, "pi", "a\nb").unwrap_err();

    assert!(matches!(error, ProfileError::Invalid { .. }), "{error:?}");
    assert_eq!(settings.agent(AgentRole::Reviewer).unwrap(), None);
}

#[test]
fn surrounding_whitespace_is_trimmed_when_saving() {
    let mut settings = Settings::load(&path("trim")).unwrap();

    settings.set_agent(AgentRole::Implementer, " claude ", " opus\t").unwrap();

    assert_eq!(settings.agent(AgentRole::Implementer).unwrap(), Some(profile("claude", "opus")));
}

#[test]
fn a_hand_edited_bad_entry_is_an_error_not_a_default() {
    let path = path("bad-file");
    for text in [
        "[implementer]\nharness = \"claude\"\n",
        "[implementer]\nharness = \"claude\"\nprofile = \"\"\n",
        "[implementer]\nharness = 3\nprofile = \"opus\"\n",
        "implementer = \"opus\"\n",
    ] {
        fs::write(&path, text).unwrap();
        let settings = Settings::load(&path).unwrap();

        let error = settings.agent(AgentRole::Implementer).unwrap_err();

        assert!(matches!(error, ProfileError::Invalid { .. } | ProfileError::Blank { .. }), "{text}: {error:?}");
    }
}

#[test]
fn saving_a_role_keeps_other_keys_and_the_other_role() {
    let path = path("keeps");
    fs::write(&path, "[reviewer]\nharness = \"pi\"\nprofile = \"gpt\"\nnote = \"x\"\n\n[other]\nk = 1\n").unwrap();
    let mut settings = Settings::load(&path).unwrap();

    settings.set_agent(AgentRole::Implementer, "claude", "opus").unwrap();
    settings.set_agent(AgentRole::Reviewer, "pi", "gpt-6").unwrap();
    settings.save().unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("note = \"x\""), "{text}");
    assert!(text.contains("[other]\nk = 1"), "{text}");
    assert!(text.contains("profile = \"gpt-6\""), "{text}");
}
