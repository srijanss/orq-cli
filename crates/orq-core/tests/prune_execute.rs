use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orq_core::prune::{prune, PruneReport};
use orq_core::retention::{inventory, plan, Artifact, ArtifactClass, Candidate, PrunePlan, RetentionPolicy, RunFacts};
use orq_core::run::RunState;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const DAY: u64 = 86_400;
const NOW: u64 = 10_000 * DAY;

struct World {
    roots: StorageRoots,
    store: Store,
}

fn world(tag: &str) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-prune-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    World { roots, store }
}

fn put(root: &Path, rel: &str, bytes: usize) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

fn failed(run: &str, days_ago: u64) -> RunFacts {
    RunFacts { run_id: run.into(), state: RunState::Failed, ended_at: Some(NOW - days_ago * DAY), review: None, pinned: false, worktree_in_use: false }
}

fn table(list: Vec<RunFacts>) -> BTreeMap<String, RunFacts> {
    list.into_iter().map(|f| (f.run_id.clone(), f)).collect()
}

fn planned(w: &World, facts: &BTreeMap<String, RunFacts>) -> PrunePlan {
    plan(&RetentionPolicy::default(), facts, &inventory(&w.roots.data).unwrap(), NOW)
}

fn run_prune(w: &World, p: &PrunePlan, current: &BTreeMap<String, RunFacts>) -> PruneReport {
    prune(&w.roots.data, &w.store, &RetentionPolicy::default(), p, NOW, &|run| current.get(run).cloned()).unwrap()
}

fn exists(w: &World, rel: &str) -> bool {
    std::fs::symlink_metadata(w.roots.data.join(rel)).is_ok()
}

fn hand_made(run: &str, rel: &str) -> PrunePlan {
    PrunePlan {
        candidates: vec![Candidate {
            artifact: Artifact { run_id: run.into(), class: ArtifactClass::Log, rel: PathBuf::from(rel), bytes: 1 },
            reason: "test".into(),
        }],
        ..PrunePlan::default()
    }
}

#[test]
fn eligible_files_are_removed_and_the_space_is_reported() {
    let w = world("remove");
    put(&w.roots.data, "runs/old/events.jsonl", 100);
    put(&w.roots.data, "runs/old/tmp/x", 5);
    put(&w.roots.data, "runs/old/handoff.json", 7);
    put(&w.roots.data, "runs/new/events.jsonl", 100);
    let facts = table(vec![failed("old", 20), failed("new", 1)]);
    let report = run_prune(&w, &planned(&w, &facts), &facts);
    assert!(!exists(&w, "runs/old/events.jsonl") && !exists(&w, "runs/old/tmp/x"));
    assert!(exists(&w, "runs/old/handoff.json"), "a checkpoint is not old enough");
    assert!(exists(&w, "runs/new/events.jsonl"));
    assert_eq!(report.freed, 105);
    assert_eq!(report.removed.len(), 2);
    assert!(w.store.pending_removals().unwrap().is_empty());
}

#[test]
fn pruning_twice_is_harmless() {
    let w = world("twice");
    put(&w.roots.data, "runs/old/events.jsonl", 100);
    let facts = table(vec![failed("old", 20)]);
    let p = planned(&w, &facts);
    run_prune(&w, &p, &facts);
    let again = run_prune(&w, &p, &facts);
    assert_eq!(again.freed, 0);
    assert!(again.removed.is_empty());
    assert_eq!(again.missing, ["runs/old/events.jsonl"]);
    assert!(w.store.pending_removals().unwrap().is_empty());
}

#[test]
fn a_run_that_became_active_after_the_plan_is_left_alone() {
    let w = world("active");
    put(&w.roots.data, "runs/old/events.jsonl", 100);
    let facts = table(vec![failed("old", 20)]);
    let p = planned(&w, &facts);
    let mut now_running = failed("old", 20);
    now_running.state = RunState::Running;
    let report = run_prune(&w, &p, &table(vec![now_running]));
    assert!(exists(&w, "runs/old/events.jsonl"));
    assert_eq!(report.freed, 0);
    assert_eq!(report.skipped.len(), 1, "{report:?}");
}

#[test]
fn a_run_pinned_after_the_plan_is_left_alone() {
    let w = world("pinned");
    put(&w.roots.data, "runs/old/events.jsonl", 100);
    let facts = table(vec![failed("old", 20)]);
    let p = planned(&w, &facts);
    let mut pinned = failed("old", 20);
    pinned.pinned = true;
    run_prune(&w, &p, &table(vec![pinned]));
    assert!(exists(&w, "runs/old/events.jsonl"));
}

#[test]
fn a_run_whose_facts_can_no_longer_be_read_is_left_alone() {
    let w = world("nofacts");
    put(&w.roots.data, "runs/old/events.jsonl", 100);
    let facts = table(vec![failed("old", 20)]);
    let p = planned(&w, &facts);
    run_prune(&w, &p, &BTreeMap::new());
    assert!(exists(&w, "runs/old/events.jsonl"));
}

#[test]
fn a_review_that_changed_after_the_plan_keeps_the_run() {
    use orq_core::retention::ReviewFacts;
    use orq_core::review_lifecycle::ProviderState;
    let w = world("stale");
    put(&w.roots.data, "runs/old/review-rounds.json", 10);
    // Merged when planned, so reports were eligible after 90 days.
    let mut merged = failed("old", 100);
    merged.state = RunState::Completed;
    merged.review = Some(ReviewFacts { state: ProviderState::Merged, observed_at: NOW - DAY, last_error: None });
    let p = planned(&w, &table(vec![merged.clone()]));
    assert_eq!(p.candidates.len(), 1);
    // The provider now says it was reopened.
    let mut reopened = merged;
    reopened.review = Some(ReviewFacts { state: ProviderState::Open, observed_at: NOW, last_error: None });
    run_prune(&w, &p, &table(vec![reopened]));
    assert!(exists(&w, "runs/old/review-rounds.json"));
}

#[test]
fn a_symlink_swapped_in_for_a_run_directory_is_not_followed() {
    let w = world("symlink");
    put(&w.roots.data, "runs/old/events.jsonl", 100);
    let facts = table(vec![failed("old", 20)]);
    let p = planned(&w, &facts);
    // After planning, the run directory is replaced by a link to somewhere else.
    let outside = w.roots.data.parent().unwrap().join("outside");
    put(&outside, "events.jsonl", 9);
    std::fs::rename(w.roots.data.join("runs/old"), w.roots.data.join("runs/old-moved")).unwrap();
    std::os::unix::fs::symlink(&outside, w.roots.data.join("runs/old")).unwrap();
    let report = run_prune(&w, &p, &facts);
    assert!(outside.join("events.jsonl").exists(), "the file behind the link must survive");
    assert_eq!(report.freed, 0);
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert!(w.store.pending_removals().unwrap().is_empty());
}

#[test]
fn a_path_that_escapes_the_run_is_refused() {
    let w = world("escape");
    put(&w.roots.data, "runs/old/events.jsonl", 1);
    put(&w.roots.data, "runs/other/events.jsonl", 1);
    let outside = w.roots.data.parent().unwrap().join("secret.txt");
    std::fs::write(&outside, "keep").unwrap();
    let facts = table(vec![failed("old", 20)]);
    for rel in ["runs/old/../../../secret.txt", "runs/other/events.jsonl", "/etc/passwd", "events.jsonl"] {
        let report = run_prune(&w, &hand_made("old", rel), &facts);
        assert_eq!(report.freed, 0, "{rel}");
        assert_eq!(report.skipped.len(), 1, "{rel}: {report:?}");
    }
    assert!(outside.exists() && exists(&w, "runs/other/events.jsonl"));
}

#[test]
fn tool_and_project_files_are_never_removed() {
    let w = world("mcp");
    put(&w.roots.data, "runs/old/.mcp.json", 1);
    put(&w.roots.data, "runs/old/.tdd-features.json", 1);
    let facts = table(vec![failed("old", 20)]);
    for rel in ["runs/old/.mcp.json", "runs/old/.tdd-features.json"] {
        let report = run_prune(&w, &hand_made("old", rel), &facts);
        assert_eq!(report.skipped.len(), 1, "{rel}");
        assert!(exists(&w, rel));
    }
}

#[test]
fn a_file_that_is_already_gone_is_not_an_error() {
    let w = world("missing");
    put(&w.roots.data, "runs/old/events.jsonl", 100);
    let facts = table(vec![failed("old", 20)]);
    let p = planned(&w, &facts);
    std::fs::remove_file(w.roots.data.join("runs/old/events.jsonl")).unwrap();
    let report = run_prune(&w, &p, &facts);
    assert_eq!(report.missing, ["runs/old/events.jsonl"]);
    assert!(report.skipped.is_empty());
}

#[test]
fn a_removed_report_is_forgotten_by_the_index_and_ancient_metadata_goes_with_the_run() {
    let w = world("index");
    let sha = "a".repeat(40);
    let record = w.store.save_review(&w.roots.data, "old", "p-0a1b2c3d", "F", &sha, br#"{"status":"current","risks":[]}"#).unwrap();
    let mut ancient = failed("old", 400);
    ancient.state = RunState::Failed;
    let facts = table(vec![ancient]);
    let p = planned(&w, &facts);
    assert_eq!(p.metadata, ["old"]);
    let report = run_prune(&w, &p, &facts);
    assert!(!exists(&w, &record.report_path));
    assert!(w.store.reviews_for_run("old").unwrap().is_empty());
    assert_eq!(report.metadata_removed, ["old"]);
}
