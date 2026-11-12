use std::path::Path;

use orq_core::retention::{inventory, ArtifactClass};

fn data_root(tag: &str) -> std::path::PathBuf {
    let base = orq_test_support::process_temp_dir().join(format!("orq-inv-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("runs")).unwrap();
    base
}

fn put(root: &Path, rel: &str, bytes: usize) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

fn find<'a>(items: &'a [orq_core::retention::Artifact], rel: &str) -> Option<&'a orq_core::retention::Artifact> {
    items.iter().find(|a| a.rel == Path::new(rel))
}

#[test]
fn each_file_of_a_run_is_classified_and_sized() {
    let root = data_root("classes");
    let sha = "a".repeat(40);
    put(&root, "runs/run-1/events.jsonl", 10);
    put(&root, "runs/run-1/handoff.json", 20);
    put(&root, "runs/run-1/tests.json", 30);
    put(&root, "runs/run-1/review-rounds.json", 40);
    put(&root, &format!("runs/run-1/review/{sha}/review.json"), 50);
    put(&root, &format!("runs/run-1/review/{sha}/.review.json.1.2.tmp"), 5);
    put(&root, "runs/run-1/tmp/scratch.bin", 7);
    let items = inventory(&root).unwrap();
    let class = |rel: &str| find(&items, rel).map(|a| (a.class, a.bytes, a.run_id.as_str()));
    assert_eq!(class("runs/run-1/events.jsonl"), Some((ArtifactClass::Log, 10, "run-1")));
    assert_eq!(class("runs/run-1/handoff.json"), Some((ArtifactClass::Checkpoint, 20, "run-1")));
    assert_eq!(class("runs/run-1/tests.json"), Some((ArtifactClass::Report, 30, "run-1")));
    assert_eq!(class("runs/run-1/review-rounds.json"), Some((ArtifactClass::Report, 40, "run-1")));
    assert_eq!(class(&format!("runs/run-1/review/{sha}/review.json")), Some((ArtifactClass::Report, 50, "run-1")));
    assert_eq!(class(&format!("runs/run-1/review/{sha}/.review.json.1.2.tmp")), Some((ArtifactClass::Temporary, 5, "run-1")));
    assert_eq!(class("runs/run-1/tmp/scratch.bin"), Some((ArtifactClass::Temporary, 7, "run-1")));
}

#[test]
fn files_orq_does_not_know_are_never_listed() {
    let root = data_root("unknown");
    put(&root, "runs/run-1/events.jsonl", 1);
    put(&root, "runs/run-1/.mcp.json", 1);
    put(&root, "runs/run-1/notes.txt", 1);
    put(&root, "runs/run-1/worktree/src/main.rs", 1);
    let items = inventory(&root).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
}

#[test]
fn symlinks_and_unsafe_run_names_are_skipped() {
    let root = data_root("links");
    let outside = root.join("outside");
    put(&outside, "events.jsonl", 99);
    std::os::unix::fs::symlink(&outside, root.join("runs/run-linked")).unwrap();
    put(&root, "runs/run-2/handoff.json", 3);
    std::os::unix::fs::symlink(outside.join("events.jsonl"), root.join("runs/run-2/events.jsonl")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("runs/run-2/review")).unwrap();
    put(&root, "runs/bad name/events.jsonl", 4);
    put(&root, "runs/..hidden/events.jsonl", 4);
    let items = inventory(&root).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].rel, Path::new("runs/run-2/handoff.json"));
}

#[test]
fn a_missing_runs_directory_is_an_empty_inventory() {
    let base = orq_test_support::process_temp_dir().join(format!("orq-inv-{}-none", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    assert!(inventory(&base).unwrap().is_empty());
}

#[test]
fn the_inventory_is_sorted_by_run_then_path() {
    let root = data_root("sorted");
    put(&root, "runs/run-b/events.jsonl", 1);
    put(&root, "runs/run-a/handoff.json", 1);
    put(&root, "runs/run-a/events.jsonl", 1);
    let rels: Vec<_> = inventory(&root).unwrap().into_iter().map(|a| a.rel.to_string_lossy().into_owned()).collect();
    assert_eq!(rels, ["runs/run-a/events.jsonl", "runs/run-a/handoff.json", "runs/run-b/events.jsonl"]);
}
