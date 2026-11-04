use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{pause_at_boundary, request_pause, validate_checkpoint, Event, RunRecord, RunStore};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn paused(tag: &str) -> (RunStore, PathBuf, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-ckpt-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let wt = repo.join(".claude").join("worktrees").join("w1");
    git(&repo, &["worktree", "add", "-q", "-b", "orq/run-1", wt.to_str().unwrap()]);
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: "orq/run-1".into(),
            worktree: wt.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e0".into(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            kind: "stage.started".into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();
    request_pause(&store, "run-1").unwrap();
    assert!(pause_at_boundary(&store, "run-1", &["implement"], "gate", None, None).unwrap());
    (store, repo, wt)
}

#[test]
fn an_untouched_paused_run_has_a_valid_checkpoint() {
    let (store, repo, _) = paused("ok");
    let checkpoint = validate_checkpoint(&store, &repo, "run-1").unwrap();
    assert_eq!(checkpoint.next_stage.as_deref(), Some("gate"));
}

#[test]
fn a_changed_checkout_is_refused_with_the_reason() {
    let (store, repo, wt) = paused("moved");
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "later"]);
    assert!(validate_checkpoint(&store, &repo, "run-1").unwrap_err().contains("HEAD moved"));

    let (store, repo, wt) = paused("rebase");
    let git_dir = Command::new("git").arg("-C").arg(&wt).args(["rev-parse", "--path-format=absolute", "--git-dir"]).output().unwrap();
    std::fs::create_dir(Path::new(String::from_utf8_lossy(&git_dir.stdout).trim()).join("rebase-merge")).unwrap();
    assert!(validate_checkpoint(&store, &repo, "run-1").unwrap_err().contains("rebase"));

    let (store, repo, wt) = paused("gone");
    git(&repo, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
    assert!(validate_checkpoint(&store, &repo, "run-1").unwrap_err().contains("no longer exists"));

    let (store, repo, wt) = paused("branch");
    git(&wt, &["checkout", "-q", "--detach"]);
    assert!(validate_checkpoint(&store, &repo, "run-1").unwrap_err().contains("not on the branch"));
}

#[test]
fn only_a_paused_run_can_be_validated() {
    let (store, repo, _) = paused("phase");
    assert!(validate_checkpoint(&store, &repo, "nope").is_err());
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e9".into(),
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            kind: "run.resumed".into(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-11-02T10:01:00Z".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();
    assert!(validate_checkpoint(&store, &repo, "run-1").unwrap_err().contains("only a paused run"));
}
