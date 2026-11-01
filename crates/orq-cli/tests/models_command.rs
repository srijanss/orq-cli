use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::GitFixture;

const USER_CONFIG: &str = r#"
[models.opus]
harness = "claude"
model = "opus"
provider = "anthropic"

[models.gpt-6-astra]
harness = "pi"
model = "openai/gpt-6-astra"
thinking = "medium"
provider = "openai"

[models.other-pi]
harness = "pi"
model = "vendor/other"

[roles]
implementer = "opus"
reviewer = "gpt-6-astra"
"#;

fn models(repo: &Path, config_home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["models", "--repo"])
        .arg(repo)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn config_home(tag: &str, text: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("orq-models-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("orq")).unwrap();
    std::fs::write(home.join("orq/config.toml"), text).unwrap();
    home
}

#[test]
fn profiles_are_printed_with_valid_roles_and_resolved_defaults() {
    let repo = GitFixture::new();
    std::fs::write(
        repo.root().join(".tdd-config.json"),
        r#"{"reviewers":{"allowedModels":["openai/gpt-6-astra","opus"]}}"#,
    )
    .unwrap();
    let home = config_home("profiles", USER_CONFIG);

    let output = models(repo.nested_dir(), &home);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    let profiles = json["profiles"].as_array().unwrap();
    let names: Vec<&str> = profiles.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["opus", "gpt-6-astra", "other-pi"]);

    let opus = &profiles[0];
    assert_eq!(opus["harness"], "claude");
    assert_eq!(opus["model"], "opus");
    assert_eq!(opus["provider"], "anthropic");
    assert_eq!(opus["thinking"], serde_json::Value::Null);
    // Claude-only implementers; reviewers limited by allowedModels.
    assert_eq!(opus["roles"], serde_json::json!({"implementer": true, "reviewer": true}));
    let astra = &profiles[1];
    assert_eq!(astra["thinking"], "medium");
    assert_eq!(astra["roles"], serde_json::json!({"implementer": false, "reviewer": true}));
    let other = &profiles[2];
    assert_eq!(other["roles"], serde_json::json!({"implementer": false, "reviewer": false}));

    assert_eq!(json["allowedModels"], serde_json::json!(["openai/gpt-6-astra", "opus"]));
    assert_eq!(json["defaults"]["implementer"]["model"], "opus");
    assert_eq!(json["defaults"]["reviewer"]["model"], "openai/gpt-6-astra");
    assert_eq!(json["defaults"]["reviewer"]["harness"], "pi");
}

#[test]
fn unusable_defaults_and_unreadable_config_are_reported_not_hidden() {
    let repo = GitFixture::new();
    // The default reviewer is not allowed, so no default can be resolved.
    std::fs::write(repo.root().join(".tdd-config.json"), r#"{"reviewers":{"allowedModels":["nothing/else"]}}"#)
        .unwrap();
    let home = config_home("bad-default", USER_CONFIG);
    let output = models(repo.root(), &home);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["defaults"], serde_json::Value::Null);
    assert!(json["defaultsError"].as_str().unwrap().contains("allowedModels"));
    assert_eq!(json["profiles"].as_array().unwrap().len(), 3);

    // A config that is not TOML fails the command with the reason on stderr.
    let broken = config_home("broken", "[models.x\n");
    let output = models(repo.root(), &broken);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}
