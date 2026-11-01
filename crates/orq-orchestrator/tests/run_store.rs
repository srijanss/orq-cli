use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{Event, RoleProfile, RunRecord, RunStore, StoreError};

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-store-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(run_id: &str) -> RunRecord {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "implementer".to_string(),
        RoleProfile {
            harness: "claude".to_string(),
            model: "sonnet-5-5".to_string(),
            thinking: Some("medium".to_string()),
        },
    );
    profiles.insert(
        "reviewer".to_string(),
        RoleProfile {
            harness: "pi".to_string(),
            model: "gpt-6-astra".to_string(),
            thinking: None,
        },
    );
    RunRecord {
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "git@github.com:acme/app.git".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        branch: "fix--COM-02".to_string(),
        worktree: PathBuf::from(".claude/worktrees/fix--COM-02"),
        profiles,
    }
}

fn event(run_id: &str, id: &str, kind: &str, attempt: u32) -> Event {
    Event {
        schema_version: 1,
        event_id: id.to_string(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        kind: kind.to_string(),
        stage: Some("implement".to_string()),
        attempt,
        occurred_at: "2026-11-01T09:00:00Z".to_string(),
        payload: serde_json::json!({ "note": "line one\nline two" }),
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_run_record_is_stored_under_runs_and_read_back_unchanged() {
    let root = root("record");
    let store = RunStore::new(&root);
    let original = record("run-1");

    store.create(&original).unwrap();

    // Exactly the spec layout, with no temporary file left beside it.
    assert_eq!(names(&root.join("runs").join("run-1")), vec!["run.json".to_string()]);
    assert_eq!(store.load("run-1").unwrap(), original);

    // The inputs are immutable: a second create never replaces them.
    let mut other = record("run-1");
    other.base_sha = "f".repeat(40);
    assert!(matches!(store.create(&other), Err(StoreError::AlreadyExists(_))));
    assert_eq!(store.load("run-1").unwrap(), original);
}

#[test]
fn run_ids_that_could_leave_the_store_are_refused_before_anything_is_written() {
    let root = root("ids");
    let store = RunStore::new(&root);
    for bad in ["", ".", "..", "../escape", "a/b", "a\\b", "with space", "nul\0byte", ".hidden"] {
        assert!(
            matches!(store.create(&record(bad)), Err(StoreError::InvalidRunId(_))),
            "{bad:?}"
        );
        assert!(matches!(store.load(bad), Err(StoreError::InvalidRunId(_))), "{bad:?}");
    }
    assert!(!root.join("runs").exists());
    assert!(matches!(store.load("run-missing"), Err(StoreError::NotFound(_))));
}

#[test]
fn events_are_appended_one_json_line_each_and_read_back_in_order() {
    let root = root("events");
    let store = RunStore::new(&root);
    store.create(&record("run-1")).unwrap();

    let first = event("run-1", "e1", "run.created", 0);
    let second = event("run-1", "e2", "stage.started", 1);
    store.append_event(&first).unwrap();
    store.append_event(&second).unwrap();

    assert_eq!(store.events("run-1").unwrap(), vec![first.clone(), second.clone()]);

    // The spec's envelope, one object per line, embedded newlines escaped.
    let text = fs::read_to_string(root.join("runs/run-1/events.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    for field in [
        "schema_version", "event_id", "run_id", "project_id", "feature_id", "type", "stage", "attempt",
        "occurred_at", "payload",
    ] {
        assert!(value.get(field).is_some(), "missing {field}");
    }
    assert_eq!(value["type"], "run.created");

    // Appending never rewrites what is there.
    store.append_event(&event("run-1", "e3", "stage.finished", 1)).unwrap();
    let after = fs::read_to_string(root.join("runs/run-1/events.jsonl")).unwrap();
    assert!(after.starts_with(&text));
    assert_eq!(store.events("run-1").unwrap().len(), 3);
}

#[test]
fn events_for_unknown_runs_or_with_damaged_lines_are_reported_not_skipped() {
    let root = root("damaged");
    let store = RunStore::new(&root);

    // An event cannot create a run behind the record's back.
    assert!(matches!(
        store.append_event(&event("ghost", "e1", "run.created", 0)),
        Err(StoreError::NotFound(_))
    ));
    assert!(!root.join("runs/ghost").exists());

    store.create(&record("run-1")).unwrap();
    assert_eq!(store.events("run-1").unwrap(), vec![]);

    store.append_event(&event("run-1", "e1", "run.created", 0)).unwrap();
    let path = root.join("runs/run-1/events.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("{\"event_id\": \"torn\n");
    fs::write(&path, text).unwrap();
    assert!(matches!(store.events("run-1"), Err(StoreError::Corrupt(why)) if why.contains("line 2")));
}
