use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orq_core::retention::RunFacts;
use orq_core::review_lifecycle::{Gate, ProviderState, ReviewLifecycle};
use orq_core::run::RunState;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{collect_run_facts, Event, RunRecord, RunStore};

const ENDED: u64 = 1_793_613_600; // 2026-11-02T10:00:00Z

struct Fixture {
    runs: RunStore,
    store: Store,
    base: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let base = orq_test_support::process_temp_dir()
        .join(format!("orq-facts-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    Fixture {
        runs: RunStore::new(&base.join("runs-state")),
        store: Store::open(&roots.database()).unwrap(),
        base,
    }
}

fn record(run_id: &str, worktree: &Path) -> RunRecord {
    RunRecord {
        run_id: run_id.into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: "abc".into(),
        branch: "b".into(),
        worktree: worktree.to_path_buf(),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn event(r: &RunRecord, kind: &str, at: &str, payload: serde_json::Value) -> Event {
    Event {
        schema_version: 1,
        event_id: format!("e-{}-{kind}-{at}", r.run_id),
        run_id: r.run_id.clone(),
        project_id: "p".into(),
        feature_id: "F".into(),
        kind: kind.into(),
        stage: None,
        attempt: 1,
        occurred_at: at.into(),
        payload,
    }
}

fn facts(f: &Fixture) -> BTreeMap<String, RunFacts> {
    collect_run_facts(&f.runs, &f.store).unwrap()
}

#[test]
fn a_failed_run_is_failed_and_ended_when_its_last_event_happened() {
    let f = fixture("failed");
    let r = record("run-1", &f.base.join("gone"));
    f.runs.create(&r).unwrap();
    f.runs
        .append_event(&event(
            &r,
            "stage.started",
            "2026-11-01T09:00:00Z",
            serde_json::json!({}),
        ))
        .unwrap();
    f.runs
        .append_event(&event(
            &r,
            "run.failed",
            "2026-11-02T10:00:00Z",
            serde_json::json!({}),
        ))
        .unwrap();
    let all = facts(&f);
    let got = &all["run-1"];
    assert_eq!(got.state, RunState::Failed);
    assert_eq!(got.ended_at, Some(ENDED));
    assert!(!got.pinned && !got.worktree_in_use && got.review.is_none());
}

#[test]
fn running_and_blocked_runs_keep_their_state() {
    let f = fixture("live");
    let a = record("run-a", &f.base.join("gone"));
    let b = record("run-b", &f.base.join("gone"));
    f.runs.create(&a).unwrap();
    f.runs.create(&b).unwrap();
    f.runs
        .append_event(&event(
            &a,
            "stage.started",
            "2026-11-02T10:00:00Z",
            serde_json::json!({}),
        ))
        .unwrap();
    f.runs
        .append_event(&event(
            &b,
            "run.blocked",
            "2026-11-02T10:00:00Z",
            serde_json::json!({}),
        ))
        .unwrap();
    let all = facts(&f);
    assert_eq!(all["run-a"].state, RunState::Running);
    assert_eq!(all["run-b"].state, RunState::Blocked);
}

#[test]
fn a_run_with_no_events_has_no_end_time() {
    let f = fixture("noevents");
    f.runs
        .create(&record("run-1", &f.base.join("gone")))
        .unwrap();
    assert_eq!(facts(&f)["run-1"].ended_at, None);
}

#[test]
fn an_unreadable_timestamp_means_no_end_time() {
    let f = fixture("badtime");
    let r = record("run-1", &f.base.join("gone"));
    f.runs.create(&r).unwrap();
    f.runs
        .append_event(&event(
            &r,
            "run.failed",
            "yesterday-ish",
            serde_json::json!({}),
        ))
        .unwrap();
    assert_eq!(facts(&f)["run-1"].ended_at, None);
}

#[test]
fn a_worktree_that_still_exists_is_in_use() {
    let f = fixture("worktree");
    let present = f.base.join("worktree");
    std::fs::create_dir_all(&present).unwrap();
    f.runs.create(&record("run-a", &present)).unwrap();
    f.runs
        .create(&record("run-b", &f.base.join("gone")))
        .unwrap();
    let all = facts(&f);
    assert!(all["run-a"].worktree_in_use);
    assert!(!all["run-b"].worktree_in_use);
}

#[test]
fn pins_and_the_last_review_observation_are_carried() {
    let f = fixture("review");
    f.runs
        .create(&record("run-1", &f.base.join("gone")))
        .unwrap();
    f.store.pin("run-1").unwrap();
    f.store
        .save_lifecycle(
            "run-1",
            &ReviewLifecycle {
                provider: "github".into(),
                review_id: "7".into(),
                url: "https://example.test/7".into(),
                reported_head_sha: "a".repeat(40),
                observed_head_sha: None,
                state: ProviderState::Merged,
                gate: Gate::AwaitingHumanReview,
                stale: false,
                last_error: None,
                observed: true,
            },
        )
        .unwrap();
    let all = facts(&f);
    let got = &all["run-1"];
    assert!(got.pinned);
    assert_eq!(got.review.as_ref().unwrap().state, ProviderState::Merged);
}

#[test]
fn a_run_whose_record_cannot_be_read_has_no_facts() {
    let f = fixture("corrupt");
    f.runs
        .create(&record("run-ok", &f.base.join("gone")))
        .unwrap();
    let bad = f.base.join("runs-state").join("runs").join("run-bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("run.json"), "{not json").unwrap();
    let all = facts(&f);
    assert!(all.contains_key("run-ok"));
    assert!(!all.contains_key("run-bad"));
}
