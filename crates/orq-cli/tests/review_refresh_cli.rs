use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::{FakeProvider, GitFixture};

const REPO: &str = "https://github.example/o/r";
const OTHER: &str = "2222222222222222222222222222222222222222";

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-refresh-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn orq(&self, args: &[&str], provider: Option<&FakeProvider>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.env_remove("GITHUB_TOKEN");
        let system = std::env::var("PATH").unwrap_or_default();
        match provider {
            Some(p) => command.env("PATH", format!("{}:{system}", p.bin_dir().display())),
            None => command.env("PATH", system),
        };
        command.output().unwrap()
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A run with a built report; returns the report's head commit.
fn run_with_report(sandbox: &Sandbox, fixture: &GitFixture) -> String {
    let base = git(fixture.root(), &["rev-parse", "HEAD"]);
    std::fs::write(fixture.root().join("a.txt"), "one\n").unwrap();
    git(fixture.root(), &["add", "-A"]);
    git(fixture.root(), &["commit", "--quiet", "-m", "add a"]);
    let head = git(fixture.root(), &["rev-parse", "HEAD"]);
    git(fixture.root(), &["remote", "add", "origin", "https://github.example/o/r.git"]);

    let store = RunStore::new(&sandbox.home.join("state").join("orq"));
    let record = RunRecord {
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: base.clone(),
        branch: "fix--COM-02".to_string(),
        worktree: fixture.root().to_path_buf(),
        repository: REPO.to_string(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e1".to_string(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            kind: "review.created".to_string(),
            stage: Some("review_request".to_string()),
            attempt: 1,
            occurred_at: "2026-01-01T00:00:00Z".to_string(),
            payload: serde_json::json!({ "url": "https://github.example/o/r/pull/7", "id": "7", "provider": "github" }),
        })
        .unwrap();
    let built = sandbox.orq(&["review", "build", "run-1", "--repo", fixture.root().to_str().unwrap(), "--feature", "COM-02", "--base", &base], None);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    head
}

fn listing(state: &str, head: &str) -> String {
    format!(r#"[{{"number":7,"url":"https://github.example/o/r/pull/7","state":"{state}","headRefOid":"{head}","isCrossRepository":false}}]"#)
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn a_pushed_review_waits_for_a_person_on_the_reported_commit() {
    let sandbox = Sandbox::new("open");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("OPEN", &head)).build();

    let refreshed = sandbox.orq(&["review", "refresh", "run-1"], Some(&provider));
    assert!(refreshed.status.success(), "{}", text(&refreshed));
    let out = text(&refreshed);
    assert!(out.contains("awaiting_human_review") && out.contains("open"), "{out}");
    assert!(out.contains(&head) && !out.contains("stale"), "{out}");

    let shown = sandbox.orq(&["review", "show", "run-1"], None);
    let shown = text(&shown);
    assert!(shown.contains("awaiting_human_review") && shown.contains("pull/7"), "{shown}");
}

#[test]
fn a_review_merged_by_a_person_resolves_the_gate_and_orq_never_merges() {
    let sandbox = Sandbox::new("merged");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("MERGED", &head)).build();

    let out = text(&sandbox.orq(&["review", "refresh", "run-1"], Some(&provider)));
    assert!(out.contains("merged") && out.contains("resolved"), "{out}");
    for call in provider.calls() {
        assert!(call.args.iter().all(|a| a != "merge" && a != "approve" && a != "create" && a != "close"), "{:?}", call.args);
    }
}

#[test]
fn a_force_pushed_head_marks_the_report_stale() {
    let sandbox = Sandbox::new("stale");
    let fixture = GitFixture::new();
    run_with_report(&sandbox, &fixture);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("OPEN", OTHER)).build();
    let out = text(&sandbox.orq(&["review", "refresh", "run-1"], Some(&provider)));
    assert!(out.contains("stale") && out.contains(OTHER), "{out}");
    let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
    assert!(shown.contains("stale"), "{shown}");
}

#[test]
fn closed_without_merging_is_reported_as_closed() {
    let sandbox = Sandbox::new("closed");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    let provider = FakeProvider::builder().reply(&["pr", "list"], &listing("CLOSED", &head)).build();
    let out = text(&sandbox.orq(&["review", "refresh", "run-1"], Some(&provider)));
    assert!(out.contains("closed") && !out.contains("merged"), "{out}");
}

#[test]
fn an_unreachable_or_refusing_provider_keeps_the_last_known_state_and_says_why() {
    let sandbox = Sandbox::new("errors");
    let fixture = GitFixture::new();
    let head = run_with_report(&sandbox, &fixture);
    let good = FakeProvider::builder().reply(&["pr", "list"], &listing("OPEN", &head)).build();
    assert!(sandbox.orq(&["review", "refresh", "run-1"], Some(&good)).status.success());

    for (stderr, word) in [("HTTP 403: Forbidden\n", "permission"), ("HTTP 502: Bad Gateway\n", "could not be reached")] {
        let bad = FakeProvider::builder().fail(&["pr", "list"], 1, stderr).build();
        let refreshed = sandbox.orq(&["review", "refresh", "run-1"], Some(&bad));
        assert!(!refreshed.status.success());
        assert!(text(&refreshed).to_lowercase().contains(word), "{}", text(&refreshed));
        let shown = text(&sandbox.orq(&["review", "show", "run-1"], None));
        assert!(shown.contains("awaiting_human_review") && shown.to_lowercase().contains(word), "{shown}");
    }
}

#[test]
fn refresh_needs_a_built_report_and_a_valid_run_id() {
    let sandbox = Sandbox::new("noreport");
    let provider = FakeProvider::builder().reply(&["pr", "list"], "[]").build();
    let none = sandbox.orq(&["review", "refresh", "run-1"], Some(&provider));
    assert!(!none.status.success());
    assert!(provider.calls().is_empty());
    assert!(!sandbox.orq(&["review", "refresh", "../x"], Some(&provider)).status.success());
    assert!(!sandbox.orq(&["review", "refresh"], Some(&provider)).status.success());
}
