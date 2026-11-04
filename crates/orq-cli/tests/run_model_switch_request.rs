use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use orq_orchestrator::{request_pause, Event, RunRecord, RunStore};

fn orq(state_home: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(args).stdin(Stdio::null());
    command.env("XDG_STATE_HOME", state_home);
    command.env("XDG_CONFIG_HOME", state_home.join("no-config"));
    command.output().unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn event(kind: &str, id: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: id.into(),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: Some("implement".into()),
        attempt: 1,
        occurred_at: "2026-11-02T10:00:00Z".into(),
        payload,
    }
}

/// A run in `kind`'s state, in a repository whose `.orq.toml` defines the profiles `fast-claude` and `team-pi`.
fn seed(tag: &str, kind: &str, pause: bool) -> (PathBuf, RunStore, PathBuf) {
    let home = orq_test_support::process_temp_dir().join(format!("orq-switch-req-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
    let wt = repo.join(".claude/worktrees/w1");
    git(&repo, &["worktree", "add", "-q", "-b", "orq/run-1", wt.to_str().unwrap()]);
    std::fs::write(
        repo.join(".orq.toml"),
        "[models.fast-claude]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[models.team-pi]\nharness = \"pi\"\nmodel = \"team/model\"\n",
    )
    .unwrap();
    let store = RunStore::new(&home.join("orq"));
    store
        .create(&RunRecord {
            run_id: "run-1".into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "abc".into(),
            branch: "orq/run-1".into(),
            worktree: wt,
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    store.append_event(&event(kind, "e0", serde_json::json!({}))).unwrap();
    if pause {
        request_pause(&store, "run-1").unwrap();
        assert!(orq_orchestrator::pause_at_boundary(&store, "run-1", &["implement"], "gate", None, None).unwrap());
    }
    (home, store, repo)
}

fn switch(home: &Path, repo: &Path, run: &str, role: &str, profile: &str) -> Output {
    orq(home, &["run", "switch", run, "--role", role, "--profile", profile, "--repo", repo.to_str().unwrap()])
}

fn switch_requests(store: &RunStore) -> Vec<Event> {
    store.events("run-1").unwrap().into_iter().filter(|e| e.kind == "run.model_switch_requested").collect()
}

#[test]
fn a_paused_run_records_the_requested_profile_for_the_role() {
    let (home, store, repo) = seed("ok", "stage.started", true);
    let out = switch(&home, &repo, "run-1", "reviewer", "team-pi");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let requests = switch_requests(&store);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].payload["role"], "reviewer");
    assert_eq!(requests[0].payload["profile"], "team-pi");
}

#[test]
fn a_run_that_is_not_paused_or_does_not_exist_is_refused_and_records_nothing() {
    for (tag, kind) in [("running", "stage.started"), ("failed", "run.failed")] {
        let (home, store, repo) = seed(tag, kind, false);
        let before = store.events("run-1").unwrap().len();
        let out = switch(&home, &repo, "run-1", "reviewer", "team-pi");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("only a paused run"), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(store.events("run-1").unwrap().len(), before);
    }
    let (home, store, repo) = seed("unknown-run", "stage.started", true);
    let before = store.events("run-1").unwrap().len();
    let out = switch(&home, &repo, "run-404", "reviewer", "team-pi");
    assert!(!out.status.success());
    assert_eq!(store.events("run-1").unwrap().len(), before);
}

#[test]
fn an_unknown_role_or_profile_is_refused_and_records_nothing() {
    let (home, store, repo) = seed("badargs", "stage.started", true);
    let before = store.events("run-1").unwrap().len();

    let out = switch(&home, &repo, "run-1", "janitor", "team-pi");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("implementer or reviewer"), "{}", String::from_utf8_lossy(&out.stderr));

    let out = switch(&home, &repo, "run-1", "reviewer", "nope");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope"), "{}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(store.events("run-1").unwrap().len(), before);
    assert!(switch_requests(&store).is_empty());
}
