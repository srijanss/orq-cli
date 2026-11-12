//! A failed run whose work was merged into the target branch by hand is listed as merged, and
//! clearing it leaves its feature completed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn status_of(repo: &Path, name: &str) -> String {
    let ledger: serde_json::Value = serde_json::from_slice(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap();
    let entry = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == name).unwrap();
    entry["status"].as_str().unwrap().to_string()
}

fn project_id(repo: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["project", "inspect", "--cwd"])
        .arg(repo)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    let project: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    project["id"].as_str().unwrap().to_string()
}

fn event(project_id: &str, kind: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{kind}"),
        run_id: "run-1".into(),
        project_id: project_id.into(),
        feature_id: "COM-01".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload,
    }
}

struct Merged {
    repo: GitFixture,
    home: PathBuf,
}

/// A failed run-1 of COM-01 with one commit, which was then merged into the target branch by hand
/// together with the ledger marking COM-01 completed.
fn merged_by_hand(tag: &str) -> Merged {
    let home = orq_test_support::process_temp_dir().join(format!("orq-cli-merged-by-hand-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    git(repo.root(), &["add", ".tdd-features.json"]);
    git(repo.root(), &["commit", "--quiet", "-m", "ledger"]);
    let target = git(repo.root(), &["symbolic-ref", "--short", "HEAD"]);
    let base = git(repo.root(), &["rev-parse", "HEAD"]);
    let worktree = repo.add_linked_worktree("orq-run-1");
    std::fs::write(worktree.join("feature.txt"), "done\n").unwrap();
    write_ledger(&worktree, &[LedgerEntry::new("COM-01", "completed")]);
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "--quiet", "-m", "feat: COM-01"]);
    git(repo.root(), &["merge", "--quiet", "--no-ff", "-m", "merge by hand", "orq-run-1"]);

    let project_id = project_id(repo.root());
    let store = RunStore::new(&home.join("orq"));
    store
        .start_run(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".into(),
            project_id: project_id.clone(),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: target,
            base_sha: base,
            branch: "orq-run-1".into(),
            worktree,
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store.append_event(&event(&project_id, "stage.started", serde_json::json!({}))).unwrap();
    store.append_event(&event(&project_id, "run.completed", serde_json::json!({ "ending": "failed" }))).unwrap();
    Merged { repo, home }
}

fn orq(m: &Merged, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_STATE_HOME", &m.home)
        .env_remove("TDD_FEATURES_PATH")
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap()
}

#[test]
fn a_failed_run_merged_by_hand_is_listed_as_merged_and_clearing_it_keeps_its_feature_completed() {
    let m = merged_by_hand("clear");
    let repo = m.repo.root().to_str().unwrap();

    let listed = orq(&m, &["runs", "--repo", repo, "--json"]);
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    let runs: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let run = runs["runs"].as_array().unwrap().iter().find(|r| r["run_id"] == "run-1").expect("run-1 listed");
    assert_eq!(run["phase"], "merged", "{runs}");

    let out = orq(&m, &["run", "discard", "run-1", "--yes", "--repo", repo]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(status_of(m.repo.root(), "COM-01"), "completed", "{}", text(&out.stdout));
    assert!(text(&out.stdout).contains("on the target branch"), "{}", text(&out.stdout));
    assert!(!text(&out.stdout).contains("pending"), "{}", text(&out.stdout));
    let listed = orq(&m, &["runs", "--repo", repo, "--json"]);
    let runs: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(runs["runs"].as_array().unwrap().iter().all(|r| r["run_id"] != "run-1"), "{runs}");
}
