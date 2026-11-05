use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{Event, RoleProfile, RunRecord, RunStore, StoreError};

fn root(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir()
        .join(format!("orq-store-{}-{tag}", std::process::id()));
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
        repository: String::new(),
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
    assert_eq!(
        names(&root.join("runs").join("run-1")),
        vec!["run.json".to_string()]
    );
    assert_eq!(store.load("run-1").unwrap(), original);

    // The inputs are immutable: a second create never replaces them.
    let mut other = record("run-1");
    other.base_sha = "f".repeat(40);
    assert!(matches!(
        store.create(&other),
        Err(StoreError::AlreadyExists(_))
    ));
    assert_eq!(store.load("run-1").unwrap(), original);
}

#[test]
fn run_ids_that_could_leave_the_store_are_refused_before_anything_is_written() {
    let root = root("ids");
    let store = RunStore::new(&root);
    for bad in [
        "",
        ".",
        "..",
        "../escape",
        "a/b",
        "a\\b",
        "with space",
        "nul\0byte",
        ".hidden",
    ] {
        assert!(
            matches!(store.create(&record(bad)), Err(StoreError::InvalidRunId(_))),
            "{bad:?}"
        );
        assert!(
            matches!(store.load(bad), Err(StoreError::InvalidRunId(_))),
            "{bad:?}"
        );
    }
    assert!(!root.join("runs").exists());
    assert!(matches!(
        store.load("run-missing"),
        Err(StoreError::NotFound(_))
    ));
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

    assert_eq!(
        store.events("run-1").unwrap(),
        vec![first.clone(), second.clone()]
    );

    // The spec's envelope, one object per line, embedded newlines escaped.
    let text = fs::read_to_string(root.join("runs/run-1/events.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    for field in [
        "schema_version",
        "event_id",
        "run_id",
        "project_id",
        "feature_id",
        "type",
        "stage",
        "attempt",
        "occurred_at",
        "payload",
    ] {
        assert!(value.get(field).is_some(), "missing {field}");
    }
    assert_eq!(value["type"], "run.created");

    // Appending never rewrites what is there.
    store
        .append_event(&event("run-1", "e3", "stage.finished", 1))
        .unwrap();
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

    store
        .append_event(&event("run-1", "e1", "run.created", 0))
        .unwrap();
    let path = root.join("runs/run-1/events.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("{\"event_id\": \"torn\n");
    fs::write(&path, text).unwrap();
    assert!(
        matches!(store.events("run-1"), Err(StoreError::Corrupt(why)) if why.contains("line 2"))
    );
}

#[test]
fn a_second_start_for_the_same_feature_is_refused_with_the_active_run_id() {
    let root = root("guard");
    let store = RunStore::new(&root);

    store.start_run(&record("run-1")).unwrap();

    match store.start_run(&record("run-2")) {
        Err(StoreError::FeatureBusy { active_run_id }) => assert_eq!(active_run_id, "run-1"),
        other => panic!("expected FeatureBusy, got {other:?}"),
    }
    // The refused start left no run behind.
    assert!(matches!(store.load("run-2"), Err(StoreError::NotFound(_))));

    // Another feature of the same project, and the same feature id in another project, still start.
    let mut other_feature = record("run-3");
    other_feature.feature_id = "COM-03".to_string();
    store.start_run(&other_feature).unwrap();
    let mut other_project = record("run-4");
    other_project.project_id = "repo-2".to_string();
    store.start_run(&other_project).unwrap();
}

#[test]
fn finishing_a_run_frees_the_feature_and_only_its_owner_can_free_it() {
    let root = root("release");
    let store = RunStore::new(&root);
    store.start_run(&record("run-1")).unwrap();

    // A different run id cannot release someone else's lock.
    assert!(matches!(
        store.finish_run("repo-1", "COM-02", "run-2"),
        Err(StoreError::FeatureBusy { active_run_id }) if active_run_id == "run-1"
    ));
    assert!(matches!(
        store.start_run(&record("run-3")),
        Err(StoreError::FeatureBusy { .. })
    ));

    store.finish_run("repo-1", "COM-02", "run-1").unwrap();
    store.start_run(&record("run-3")).unwrap();
    // Releasing twice is harmless for the next owner's lock.
    assert!(store.finish_run("repo-1", "COM-02", "run-1").is_err());
    assert!(matches!(
        store.start_run(&record("run-5")),
        Err(StoreError::FeatureBusy { .. })
    ));

    // Nothing but the lock file is left in the locks directory.
    assert_eq!(
        names(&root.join("locks/repo-1")),
        vec!["COM-02.lock".to_string()]
    );
}

#[cfg(unix)]
#[test]
fn a_planted_symlink_at_the_old_temporary_name_is_never_written_through() {
    let root = root("symlink");
    let outside = root.join("outside.txt");
    fs::write(&outside, "precious").unwrap();
    let locks = root.join("locks/repo-1");
    fs::create_dir_all(&locks).unwrap();
    // Every name the process could pick next: links, and regular files too.
    for n in 0..5000 {
        let candidate = locks.join(format!(".run-1.{}.{n}.tmp", std::process::id()));
        if n % 2 == 0 {
            std::os::unix::fs::symlink(&outside, &candidate).unwrap();
        } else {
            fs::write(&candidate, "stale").unwrap();
        }
    }

    let store = RunStore::new(&root);
    store.start_run(&record("run-1")).unwrap();

    assert_eq!(fs::read_to_string(&outside).unwrap(), "precious");
    assert_eq!(
        fs::read_to_string(
            locks.join(".run-1.".to_string() + &std::process::id().to_string() + ".1.tmp")
        )
        .unwrap(),
        "stale"
    );
    assert_eq!(
        fs::read_to_string(locks.join("COM-02.lock")).unwrap(),
        "run-1"
    );
}

#[test]
fn a_duplicate_release_never_frees_the_next_owners_lock() {
    use std::sync::{Arc, Barrier};

    let root = root("release-race");
    let store = Arc::new(RunStore::new(&root));
    for round in 0..200 {
        let (a, b, c) = (
            format!("a-{round}"),
            format!("b-{round}"),
            format!("c-{round}"),
        );
        store.start_run(&record(&a)).unwrap();

        let gate = Arc::new(Barrier::new(3));
        let releasers: Vec<_> = (0..2)
            .map(|_| {
                let (store, gate, a) = (store.clone(), gate.clone(), a.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    store.finish_run("repo-1", "COM-02", &a).is_ok()
                })
            })
            .collect();
        gate.wait();
        // The next run may start at any point while the duplicate releases race.
        let started = loop {
            // Read before the attempt: a refusal after the releases finished is final.
            let done = releasers.iter().all(|h| h.is_finished());
            match store.start_run(&record(&b)) {
                Ok(()) => break true,
                Err(StoreError::FeatureBusy { .. }) if done => break false,
                Err(StoreError::FeatureBusy { .. }) => std::thread::yield_now(),
                Err(other) => panic!("{other:?}"),
            }
        };
        let released: usize = releasers
            .into_iter()
            .map(|h| h.join().unwrap() as usize)
            .sum();
        assert_eq!(released, 1, "exactly one release of {a} succeeds");
        assert!(started, "{b} starts once {a} is released");

        // {b} is active, so nothing else may take the feature.
        assert!(
            matches!(store.start_run(&record(&c)), Err(StoreError::FeatureBusy { active_run_id }) if active_run_id == b),
            "round {round}: {b}'s lock was freed by a duplicate release of {a}"
        );
        store.finish_run("repo-1", "COM-02", &b).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn a_record_that_cannot_be_serialized_leaves_nothing_behind_and_can_be_retried() {
    use std::os::unix::ffi::OsStringExt;

    let root = root("unserializable");
    let store = RunStore::new(&root);
    let mut broken = record("run-1");
    broken.worktree = PathBuf::from(std::ffi::OsString::from_vec(vec![b'w', 0xff, b'k']));

    assert!(store.create(&broken).is_err());
    assert!(!root.join("runs/run-1").exists());

    // The same run id is still free for a corrected record.
    store.create(&record("run-1")).unwrap();
    assert_eq!(store.load("run-1").unwrap(), record("run-1"));
}
