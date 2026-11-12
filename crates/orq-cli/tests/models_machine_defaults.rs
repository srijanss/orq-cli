//! A run that names no agents, in a project whose config assigns no `[roles]`, uses the machine
//! defaults that Settings saves in `config.toml` (`[implementer]` / `[reviewer]`: harness, model and
//! effort, the effort becoming the run's thinking level) before falling back to the built-in agents.
//! `orq models` prints the defaults a run would use, so it shows the resolution `orq run` makes.

use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::GitFixture;

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
    let home = orq_test_support::process_temp_dir().join(format!("orq-machine-defaults-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("orq")).unwrap();
    std::fs::write(home.join("orq/config.toml"), text).unwrap();
    home
}

fn defaults(repo: &GitFixture, home: &Path) -> serde_json::Value {
    let output = models(repo.root(), home);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    json["defaults"].clone()
}

const MACHINE: &str = r#"
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
fn without_roles_a_run_uses_the_machine_defaults_with_their_effort() {
    let repo = GitFixture::new();
    let home = config_home("plain", MACHINE);
    let defaults = defaults(&repo, &home);
    assert_eq!(defaults["implementer"], serde_json::json!({"harness": "claude", "model": "sonnet", "thinking": "high"}));
    assert_eq!(defaults["reviewer"], serde_json::json!({"harness": "pi", "model": "vendor/checker", "thinking": "low"}));
}

#[test]
fn a_broken_machine_default_stops_only_the_runs_that_would_use_it() {
    let repo = GitFixture::new();
    let broken = "[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[implementer]\nharness = 1\n";
    let home = config_home("broken-used", broken);
    let output = models(repo.root(), &home);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["profiles"][0]["name"], "opus", "the profiles are still listed");
    let error = json["defaultsError"].as_str().unwrap_or_default();
    assert!(error.contains("implementer harness"), "a run would fall back to it, so it is reported: {json}");

    let covered = format!("{broken}\n[roles]\nimplementer = \"opus\"\n");
    let home = config_home("broken-unused", &covered);
    let defaults = defaults(&repo, &home);
    assert_eq!(defaults["implementer"]["model"], "opus", "[roles] covers the role, so the broken table is never used");
}
