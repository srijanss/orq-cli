//! A session that writes its exit marker is closed by orq, so push and review follow without anyone
//! closing the session by hand.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use orq_orchestrator::{run_implement_stage, ImplementSession, RoleProfile, RunRecord, RunStore};

fn setup(tag: &str) -> (RunStore, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-exit-marker-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("worktree")).unwrap();
    let store = RunStore::new(&root.join("state"));
    let mut profiles = BTreeMap::new();
    profiles.insert("implementer".to_string(), RoleProfile { harness: "claude".to_string(), model: "opus".to_string(), thinking: None });
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            branch: "fix--COM-02".to_string(),
            worktree: PathBuf::from("worktree"),
            repository: String::new(),
            profiles,
        })
        .unwrap();
    (store, root)
}

fn args(script: &str) -> Vec<String> {
    vec!["-c".to_string(), script.to_string()]
}

#[test]
fn a_session_that_wrote_its_marker_but_keeps_running_is_closed_and_counts_as_finished() {
    let (store, root) = setup("closed");
    let marker = root.join("worktree/done.marker");
    let args = args(&format!("touch {}; sleep 30", marker.display()));
    let session = ImplementSession {
        program: "sh",
        args: &args,
        env: &[],
        cwd: &root.join("worktree"),
        session_id: None,
        log: &root.join("implement.log"),
        terminal: false,
        exit_marker: Some(&marker),
    };

    let started = Instant::now();
    let outcome = run_implement_stage(&store, "run-1", &session).unwrap();

    assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    assert!(outcome.success, "{outcome:?}");
    let kinds: Vec<String> = store.events("run-1").unwrap().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, ["stage.started", "stage.completed"]);
}

#[test]
fn without_a_marker_file_the_session_ends_on_its_own_exit_status() {
    let (store, root) = setup("failing");
    let marker = root.join("worktree/done.marker");
    let args = args("exit 3");
    let session = ImplementSession {
        program: "sh",
        args: &args,
        env: &[],
        cwd: &root.join("worktree"),
        session_id: None,
        log: &root.join("implement.log"),
        terminal: false,
        exit_marker: Some(&marker),
    };

    let outcome = run_implement_stage(&store, "run-1", &session).unwrap();

    assert!(!outcome.success);
    assert_eq!(outcome.exit_code, Some(3));
}

#[test]
fn a_marker_left_from_an_earlier_session_does_not_close_a_new_one_at_once() {
    let (store, root) = setup("stale");
    let marker = root.join("worktree/done.marker");
    fs::write(&marker, "old").unwrap();
    let args = args("sleep 1; exit 3");
    let session = ImplementSession {
        program: "sh",
        args: &args,
        env: &[],
        cwd: &root.join("worktree"),
        session_id: None,
        log: &root.join("implement.log"),
        terminal: false,
        exit_marker: Some(&marker),
    };

    let outcome = run_implement_stage(&store, "run-1", &session).unwrap();

    assert_eq!(outcome.exit_code, Some(3), "the stale marker ended the session early");
}

fn session<'a>(root: &'a std::path::Path, args: &'a [String], marker: &'a std::path::Path) -> ImplementSession<'a> {
    ImplementSession {
        program: "sh",
        args,
        env: &[],
        cwd: Box::leak(Box::new(root.join("worktree"))),
        session_id: None,
        log: Box::leak(Box::new(root.join("implement.log"))),
        terminal: false,
        exit_marker: Some(marker),
    }
}

#[test]
fn a_stale_marker_that_cannot_be_removed_fails_the_stage_instead_of_closing_the_new_session() {
    let (store, root) = setup("unremovable");
    let marker = root.join("worktree/done.marker");
    fs::create_dir(&marker).unwrap();
    let args = args("sleep 30");

    let started = Instant::now();
    let outcome = run_implement_stage(&store, "run-1", &session(&root, &args, &marker)).unwrap();

    assert!(!outcome.success, "{outcome:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_descendant_of_a_closed_session_does_not_outlive_the_stage() {
    let (store, root) = setup("descendant");
    let marker = root.join("worktree/done.marker");
    let late = root.join("worktree/late.txt");
    let args = args(&format!("(sleep 2; touch {}) & touch {}; sleep 30", late.display(), marker.display()));

    let outcome = run_implement_stage(&store, "run-1", &session(&root, &args, &marker)).unwrap();
    assert!(outcome.success, "{outcome:?}");
    std::thread::sleep(Duration::from_secs(3));

    assert!(!late.exists(), "a descendant kept running after the session was closed");
}

#[test]
fn a_descendant_of_a_session_that_wrote_its_marker_and_exited_does_not_outlive_the_stage() {
    let (store, root) = setup("exited");
    let marker = root.join("worktree/done.marker");
    let late = root.join("worktree/late-exit.txt");
    let args = args(&format!("(sleep 2; touch {}) & touch {}", late.display(), marker.display()));

    let outcome = run_implement_stage(&store, "run-1", &session(&root, &args, &marker)).unwrap();
    assert!(outcome.success, "{outcome:?}");
    std::thread::sleep(Duration::from_secs(3));

    assert!(!late.exists(), "a descendant kept running after the session exited");
}

fn terminal_session<'a>(root: &'a std::path::Path, args: &'a [String], marker: &'a std::path::Path) -> ImplementSession<'a> {
    ImplementSession { terminal: true, ..session(root, args, marker) }
}

#[test]
fn terminal_session_descendants_do_not_outlive_a_closed_or_exited_session() {
    for (tag, tail) in [("term-closed", "sleep 30"), ("term-exited", "true")] {
        let (store, root) = setup(tag);
        let marker = root.join("worktree/done.marker");
        let late = root.join("worktree/late.txt");
        let args = args(&format!("(sleep 2; touch {}) & touch {}; {tail}", late.display(), marker.display()));

        let outcome = run_implement_stage(&store, "run-1", &terminal_session(&root, &args, &marker)).unwrap();
        assert!(outcome.success, "{tag}: {outcome:?}");
        std::thread::sleep(Duration::from_secs(3));

        assert!(!late.exists(), "{tag}: a descendant kept running");
    }
}
