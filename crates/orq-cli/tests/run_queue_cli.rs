//! `orq queue list|add|remove|pause|resume --repo <path>` works the same queue in orq.db as the web's
//! Queue section: features added from the terminal are marked as added by the CLI, a draft cannot be
//! queued, and a pause from the terminal holds the server's queue until resumed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-queue-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn queue(&self, repo: &Path, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.arg("queue").args(args).arg("--repo").arg(repo);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.env_remove("TDD_FEATURES_PATH");
        command.output().unwrap()
    }

    fn ok(&self, repo: &Path, args: &[&str]) -> String {
        let out = self.queue(repo, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }
}

#[test]
fn features_are_queued_listed_in_start_order_paused_resumed_and_removed_from_the_terminal() {
    let s = Sandbox::new("round-trip");
    let repo = GitFixture::new();
    write_ledger(
        repo.root(),
        &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending").depends_on(&["COM-01"]), LedgerEntry::new("COM-03", "draft")],
    );

    assert_eq!(s.ok(repo.root(), &["list"]), "The queue is empty.\n");
    assert_eq!(s.ok(repo.nested_dir(), &["add", "COM-02", "COM-01"]), "Queued COM-02, COM-01.\n");
    assert_eq!(s.ok(repo.root(), &["list"]), "COM-01  next\nCOM-02  waiting: waiting for COM-01\n");

    let refused = s.queue(repo.root(), &["add", "COM-03"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("COM-03 is draft: only an approved (pending) feature can be queued"));

    assert_eq!(s.ok(repo.root(), &["pause"]), "The queue is paused.\n");
    assert_eq!(s.ok(repo.root(), &["list"]), "Paused: paused from the cli\nCOM-01  next\nCOM-02  waiting: waiting for COM-01\n");
    assert_eq!(s.ok(repo.root(), &["resume"]), "The queue is running.\n");

    assert_eq!(s.ok(repo.root(), &["remove", "COM-01"]), "Removed COM-01.\n");
    let gone = s.queue(repo.root(), &["remove", "COM-01"]);
    assert!(!gone.status.success());
    assert!(String::from_utf8_lossy(&gone.stderr).contains("COM-01 is not queued"));
    let listed: serde_json::Value = s.ok(repo.root(), &["list", "--json"]).trim_end().parse().unwrap();
    assert_eq!(listed["entries"][0]["added_by"], "cli");

    assert_eq!(s.queue(repo.root(), &["shuffle"]).status.code(), Some(2), "an unknown verb is a usage error");
}

/// A run `run` of `feature` in the sandbox's run store, ended with `ending` when given (else running).
fn seed_run(s: &Sandbox, repo: &Path, run: &str, feature: &str, ending: Option<&str>) {
    use orq_orchestrator::{Event, RunRecord, RunStore};
    let store = RunStore::new(&s.home.join("state").join("orq"));
    let head = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repo).output().unwrap();
    let record = RunRecord {
        delivery: Default::default(),
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "gitlab".into(),
        target_branch: "main".into(),
        base_sha: String::from_utf8_lossy(&head.stdout).trim().to_string(),
        branch: format!("orq/{run}"),
        worktree: repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    let events = [("stage.started", serde_json::json!({}))].into_iter().chain(ending.map(|ending| ("run.completed", serde_json::json!({"ending": ending}))));
    for (n, (kind, payload)) in events.enumerate() {
        store
            .append_event(&Event {
                schema_version: 1,
                event_id: format!("{run}-{n}"),
                run_id: run.into(),
                project_id: record.project_id.clone(),
                feature_id: record.feature_id.clone(),
                kind: kind.into(),
                stage: Some("implement".into()),
                attempt: 1,
                occurred_at: format!("2026-01-01T10:00:0{n}Z"),
                payload,
            })
            .unwrap();
    }
}

#[test]
fn a_resume_with_an_answer_a_review_again_and_an_accept_of_existing_runs_are_queued_from_the_terminal() {
    let s = Sandbox::new("run-actions");
    let repo = GitFixture::new();
    // Each run of its own feature: a later run of the same feature would make the earlier no longer needed.
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending")]);
    seed_run(&s, repo.root(), "run-f", "COM-01", Some("failed"));
    seed_run(&s, repo.root(), "run-n", "COM-02", Some("needs_review"));
    seed_run(&s, repo.root(), "run-b", "COM-03", Some("blocked"));
    seed_run(&s, repo.root(), "run-going", "COM-04", None);

    assert_eq!(s.ok(repo.root(), &["add", "--resume", "run-f", "--answer", "use the v2 endpoint"]), "Queued Resume run-f.\n");
    assert_eq!(s.ok(repo.root(), &["add", "--review-again", "run-n"]), "Queued Review again run-n.\n");
    assert_eq!(s.ok(repo.root(), &["add", "--accept", "run-b"]), "Queued Accept run-b.\n");
    let listed: serde_json::Value = s.ok(repo.root(), &["list", "--json"]).trim_end().parse().unwrap();
    assert_eq!(listed["entries"][0]["answer"], "use the v2 endpoint");
    assert_eq!(listed["entries"][0]["added_by"], "cli");
    assert_eq!(s.ok(repo.root(), &["list"]), "Resume run-f  next\nReview again run-n  queued\nAccept run-b  queued\n");

    let unknown = s.queue(repo.root(), &["add", "--accept", "run-nope"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("run-nope is not one of the project's runs"), "{}", String::from_utf8_lossy(&unknown.stderr));
    let going = s.queue(repo.root(), &["add", "--resume", "run-going"]);
    assert!(String::from_utf8_lossy(&going.stderr).contains("Resume run-going cannot be queued: run-going is running"), "{}", String::from_utf8_lossy(&going.stderr));
    assert_eq!(s.queue(repo.root(), &["add", "--accept", "run-b", "--answer", "x"]).status.code(), Some(2), "only a Resume takes an answer");
    assert_eq!(s.queue(repo.root(), &["add", "--resume"]).status.code(), Some(2));

    assert_eq!(s.ok(repo.root(), &["remove", "accept:run-b"]), "Removed Accept run-b.\n");
}
