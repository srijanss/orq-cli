use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RoleProfile, RunRecord, RunStore};
use orq_test_support::GitFixture;

fn runs(args: &[&str], state_home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .arg("runs")
        .args(args)
        .env("XDG_STATE_HOME", state_home)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

fn state_home(tag: &str) -> PathBuf {
    let home = orq_test_support::process_temp_dir().join(format!("orq-runs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// The id `orq` files this repository's runs under, as `project inspect` reports it.
fn project_id(repo: &Path) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["id"].as_str().unwrap().to_string()
}

fn seed(state_home: &Path, project: &str, run_id: &str, feature: &str, events: &[(&str, Option<&str>, serde_json::Value)]) {
    let store = RunStore::new(&state_home.join("orq"));
    let profile = |model: &str| RoleProfile { harness: "claude".into(), model: model.into(), thinking: None };
    let mut profiles = BTreeMap::new();
    profiles.insert("implementer".to_string(), profile("gpt-6"));
    profiles.insert("reviewer".to_string(), profile("opus"));
    store
        .create(&RunRecord {
            run_id: run_id.into(),
            project_id: project.into(),
            feature_id: feature.into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: format!("orq/{run_id}"),
            worktree: PathBuf::from("/tmp/w"),
            repository: String::new(),
            profiles,
        })
        .unwrap();
    for (index, (kind, stage, payload)) in events.iter().enumerate() {
        store
            .append_event(&Event {
                schema_version: 1,
                event_id: format!("{run_id}-{index}"),
                run_id: run_id.into(),
                project_id: project.into(),
                feature_id: feature.into(),
                kind: kind.to_string(),
                stage: stage.map(str::to_string),
                attempt: 1,
                occurred_at: "2026-11-02T10:00:00Z".into(),
                payload: payload.clone(),
            })
            .unwrap();
    }
}

#[test]
fn runs_prints_one_line_per_run_of_this_project_only() {
    let fixture = GitFixture::new();
    let home = state_home("text");
    let mine = project_id(fixture.root());
    seed(&home, &mine, "run-a", "COM-01", &[("stage.started", Some("push"), serde_json::json!({}))]);
    seed(
        &home,
        &mine,
        "run-b",
        "COM-02",
        &[
            ("stage.failed", Some("verify"), serde_json::json!({"reason": "tests failed"})),
        ],
    );
    seed(&home, "someone-else", "run-c", "OTH-01", &[]);

    let out = runs(&["--repo", fixture.root().to_str().unwrap()], &home);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains("run-a") && lines[0].contains("COM-01") && lines[0].contains("running") && lines[0].contains("push"), "{text}");
    assert!(lines[1].contains("run-b") && lines[1].contains("failed") && lines[1].contains("tests failed"), "{text}");
    assert!(!text.contains("run-c"));
}

#[test]
fn runs_json_lists_models_and_review_link() {
    let fixture = GitFixture::new();
    let home = state_home("json");
    let mine = project_id(fixture.root());
    seed(
        &home,
        &mine,
        "run-a",
        "COM-01",
        &[("review.created", Some("review"), serde_json::json!({"url": "https://example.test/pr/1", "id": "1"}))],
    );

    let out = runs(&["--repo", fixture.root().to_str().unwrap(), "--json"], &home);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let run = &value["runs"][0];
    assert_eq!(run["run_id"], "run-a");
    assert_eq!(run["feature_id"], "COM-01");
    assert_eq!(run["phase"], "running");
    assert_eq!(run["implementer_model"], "gpt-6");
    assert_eq!(run["reviewer_model"], "opus");
    assert_eq!(run["review_url"], "https://example.test/pr/1");
}

#[test]
fn runs_with_no_runs_prints_nothing_and_an_empty_json_list() {
    let fixture = GitFixture::new();
    let home = state_home("empty");
    let repo = fixture.root().to_str().unwrap();

    let text = runs(&["--repo", repo], &home);
    let json = runs(&["--repo", repo, "--json"], &home);

    assert!(text.status.success() && text.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["runs"], serde_json::json!([]));
}

#[test]
fn runs_without_repo_is_a_usage_error() {
    let home = state_home("usage");

    let out = runs(&[], &home);

    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage:"));
}

#[test]
fn runs_fails_when_the_repo_is_not_a_project() {
    let home = state_home("notrepo");
    let dir = home.join("plain");
    std::fs::create_dir_all(&dir).unwrap();

    let out = runs(&["--repo", dir.to_str().unwrap()], &home);

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot resolve project"));
}

#[test]
fn a_corrupt_event_log_is_left_out_of_the_listing_instead_of_failing_the_command() {
    let fixture = GitFixture::new();
    let home = state_home("corrupt-events");
    let mine = project_id(fixture.root());
    seed(&home, &mine, "run-a", "COM-01", &[]);
    std::fs::write(home.join("orq/runs/run-a/events.jsonl"), "not json\n").unwrap();

    let out = runs(&["--repo", fixture.root().to_str().unwrap(), "--json"], &home);

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["runs"].as_array().unwrap().is_empty());
    assert_eq!(value["unreadable"], 1);
}

#[test]
fn a_corrupt_run_record_is_left_out_of_the_listing_instead_of_failing_the_command() {
    let fixture = GitFixture::new();
    let home = state_home("corrupt-record");
    let mine = project_id(fixture.root());
    seed(&home, &mine, "run-a", "COM-01", &[]);
    std::fs::write(home.join("orq/runs/run-a/run.json"), "{").unwrap();

    let out = runs(&["--repo", fixture.root().to_str().unwrap()], &home);

    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("run-a"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 run could not be read"), "{}", String::from_utf8_lossy(&out.stderr));
}

fn runs_without_xdg(repo: &Path, home: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["runs", "--repo"]).arg(repo).env_remove("XDG_STATE_HOME").env_remove("HERDR_ACTIVE_PANE_CWD");
    match home {
        Some(home) => command.env("HOME", home),
        None => command.env_remove("HOME"),
    };
    command.output().unwrap()
}

#[test]
fn without_a_state_location_the_command_says_so() {
    let fixture = GitFixture::new();

    let out = runs_without_xdg(fixture.root(), None);

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("state directory"));
}

#[test]
fn the_state_directory_falls_back_to_home_when_xdg_state_home_is_unset() {
    let fixture = GitFixture::new();
    let home = state_home("home-fallback");
    let mine = project_id(fixture.root());
    // Seeds under <home>/.local/state/orq, the fallback location.
    seed(&home.join(".local/state"), &mine, "run-a", "COM-01", &[]);

    let out = runs_without_xdg(fixture.root(), Some(&home));

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("run-a"));
}
