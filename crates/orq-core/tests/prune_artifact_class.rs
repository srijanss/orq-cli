use std::path::{Path, PathBuf};

use orq_core::prune::prune;
use orq_core::retention::{inventory, Artifact, ArtifactClass, Candidate, PrunePlan, RetentionPolicy, RunFacts};
use orq_core::run::RunState;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const DAY: u64 = 86_400;
const NOW: u64 = 10_000 * DAY;

fn setup(tag: &str) -> (StorageRoots, Store) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-prune-class-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    (roots, store)
}

fn put(root: &Path, rel: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"x").unwrap();
}

// A failed run 20 days old: logs (14 days) are past retention, checkpoints (30 days) are not.
fn facts() -> RunFacts {
    RunFacts { run_id: "r1".into(), state: RunState::Failed, ended_at: Some(NOW - 20 * DAY), review: None, pinned: false, worktree_in_use: false }
}

fn plan_for(rel: &str, claimed: ArtifactClass) -> PrunePlan {
    PrunePlan {
        candidates: vec![Candidate { artifact: Artifact { run_id: "r1".into(), class: claimed, rel: PathBuf::from(rel), bytes: 1 }, reason: "test".into() }],
        ..PrunePlan::default()
    }
}

fn run(roots: &StorageRoots, store: &Store, plan: &PrunePlan) {
    let f = facts();
    prune(&roots.data, store, &RetentionPolicy::default(), plan, NOW, &|_| Some(f.clone())).unwrap();
}

#[test]
fn a_checkpoint_labelled_as_a_log_is_not_removed() {
    let (roots, store) = setup("mislabel");
    put(&roots.data, "runs/r1/handoff.json");
    run(&roots, &store, &plan_for("runs/r1/handoff.json", ArtifactClass::Log));
    assert!(roots.data.join("runs/r1/handoff.json").exists());
}

#[test]
fn a_file_orq_does_not_recognise_is_not_removed() {
    let (roots, store) = setup("unknown");
    put(&roots.data, "runs/r1/notes.txt");
    run(&roots, &store, &plan_for("runs/r1/notes.txt", ArtifactClass::Log));
    assert!(roots.data.join("runs/r1/notes.txt").exists());
}

#[test]
fn everything_the_inventory_lists_is_still_removable_with_its_own_class() {
    let (roots, store) = setup("honest");
    put(&roots.data, "runs/r1/events.jsonl");
    put(&roots.data, "runs/r1/tmp/a/b.bin");
    put(&roots.data, "runs/r1/review/x.json");
    let items = inventory(&roots.data).unwrap();
    let plan = PrunePlan {
        candidates: items.into_iter().map(|artifact| Candidate { artifact, reason: "t".into() }).collect(),
        ..PrunePlan::default()
    };
    run(&roots, &store, &plan);
    assert!(!roots.data.join("runs/r1/events.jsonl").exists());
    assert!(!roots.data.join("runs/r1/tmp/a/b.bin").exists());
}
