use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{collect_run_facts, collect_run_facts_for, Event, RunRecord, RunStore};

struct Fixture {
    runs: RunStore,
    store: Store,
    base: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let base = orq_test_support::process_temp_dir()
        .join(format!("orq-facts1-{}-{tag}", std::process::id()));
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

fn add_failed_run(f: &Fixture, run_id: &str, worktree: &Path) {
    let record = RunRecord {
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
    };
    f.runs.create(&record).unwrap();
    f.runs
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("e-{run_id}"),
            run_id: run_id.into(),
            project_id: "p".into(),
            feature_id: "F".into(),
            kind: "run.failed".into(),
            stage: None,
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();
}

#[test]
fn one_runs_facts_equal_what_the_full_collection_says_about_it() {
    let f = fixture("same");
    add_failed_run(&f, "run-1", &f.base.join("gone"));
    add_failed_run(&f, "run-2", &f.base.join("gone"));
    f.store.pin("run-2").unwrap();
    let all = collect_run_facts(&f.runs, &f.store).unwrap();
    for id in ["run-1", "run-2"] {
        let one = collect_run_facts_for(&f.runs, &f.store, id)
            .unwrap()
            .expect("the run is known");
        let from_all = &all[id];
        assert_eq!(
            (one.state, one.ended_at, one.pinned, one.worktree_in_use),
            (
                from_all.state,
                from_all.ended_at,
                from_all.pinned,
                from_all.worktree_in_use
            )
        );
    }
}

#[test]
fn an_unknown_or_unsafe_run_id_has_no_facts() {
    let f = fixture("unknown");
    assert!(collect_run_facts_for(&f.runs, &f.store, "no-such-run")
        .unwrap()
        .is_none());
    assert!(collect_run_facts_for(&f.runs, &f.store, "../escape")
        .unwrap()
        .is_none());
}
