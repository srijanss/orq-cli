//! Which run a resume of a feature continues, and why an earlier run whose feature a later run took
//! is not resumed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{check_group_resume, later_run_of, newest_run_of, record_group, resumable_members, RunRecord, RunStore};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct World {
    store: RunStore,
    repo: PathBuf,
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-resume-later-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    World { store: RunStore::new(&root.join("state")), repo }
}

/// A failed run of `feature` (its own branch and worktree) that started at `at`.
fn failed_run(w: &World, run_id: &str, feature: &str, at: &str) -> RunRecord {
    let branch = format!("fix--{run_id}");
    let worktree = w.repo.join(".claude/worktrees").join(&branch);
    git(&w.repo, &["worktree", "add", "-q", "-b", &branch, worktree.to_str().unwrap()]);
    let record = RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: feature.to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: git(&w.repo, &["rev-parse", "main"]),
        branch,
        worktree,
        repository: "https://github.example/o/r".to_string(),
        profiles: BTreeMap::new(),
    };
    w.store.create(&record).unwrap();
    for kind in ["run.started", "run.failed"] {
        w.store
            .append_event(&orq_orchestrator::Event {
                schema_version: 1,
                event_id: format!("{run_id}-{kind}"),
                run_id: run_id.to_string(),
                project_id: "repo-1".to_string(),
                feature_id: feature.to_string(),
                kind: kind.to_string(),
                stage: None,
                attempt: 1,
                occurred_at: at.to_string(),
                payload: serde_json::json!({}),
            })
            .unwrap();
    }
    record
}

#[test]
fn the_newest_run_of_a_feature_is_the_one_that_started_last_and_none_when_it_never_ran() {
    let w = world("newest");
    failed_run(&w, "run-1700000099-5", "COM-02", "2026-01-01T00:00:01Z");
    failed_run(&w, "run-1700000001-5", "COM-02", "2026-01-01T00:00:50Z");
    failed_run(&w, "run-1700000200-5", "COM-03", "2026-01-01T00:01:00Z");

    assert_eq!(newest_run_of(&w.store, "repo-1", "COM-02").unwrap().as_deref(), Some("run-1700000001-5"));
    assert_eq!(newest_run_of(&w.store, "repo-2", "COM-02").unwrap(), None);
    assert_eq!(newest_run_of(&w.store, "repo-1", "COM-09").unwrap(), None);
}

#[test]
fn an_earlier_run_whose_feature_a_later_group_run_took_is_refused_saying_so() {
    let w = world("taken");
    failed_run(&w, "run-1700000001-1", "COM-02", "2026-01-01T00:00:01Z");
    let later = failed_run(&w, "run-1700000002-1", "COM-02", "2026-01-01T00:00:02Z");
    record_group(&w.store, &later, &["COM-02".to_string(), "COM-03".to_string()]).unwrap();

    let refused = check_group_resume(&w.store, &w.repo, "run-1700000001-1").unwrap_err();

    assert!(refused.contains("run-1700000002-1") && refused.contains("later run") && refused.contains("COM-02"), "{refused}");
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| n.to_string()).collect()
}

#[test]
fn a_resume_may_take_back_the_members_no_run_after_the_newest_took_whatever_led_it() {
    let w = world("resumable");
    // Two runs of COM-02 in the same second: the newest is the one with the larger id, and the
    // other is not later than it.
    failed_run(&w, "run-1700000001-1", "COM-02", "2026-01-01T00:00:01Z");
    failed_run(&w, "run-1700000001-2", "COM-02", "2026-01-01T00:00:01Z");
    assert_eq!(resumable_members(&w.store, "repo-1", &names(&["COM-02", "COM-03"])).unwrap(), names(&["COM-02", "COM-03"]));

    // A later group led by COM-04 took COM-03.
    let group = failed_run(&w, "run-1700000002-1", "COM-04", "2026-01-01T00:00:02Z");
    record_group(&w.store, &group, &names(&["COM-04", "COM-03"])).unwrap();
    assert_eq!(resumable_members(&w.store, "repo-1", &names(&["COM-02", "COM-03"])).unwrap(), names(&["COM-02"]));

    // A feature that never ran has nothing that took it.
    assert_eq!(resumable_members(&w.store, "repo-1", &names(&["COM-09"])).unwrap(), names(&["COM-09"]));
}

#[test]
fn a_run_of_the_project_whose_history_cannot_be_read_stops_the_resume_rather_than_being_passed_over() {
    let w = world("unreadable");
    failed_run(&w, "run-1700000001-1", "COM-02", "2026-01-01T00:00:01Z");
    let group = failed_run(&w, "run-1700000002-1", "COM-04", "2026-01-01T00:00:02Z");
    record_group(&w.store, &group, &names(&["COM-04", "COM-02"])).unwrap();
    let events = w.repo.parent().unwrap().join("state/runs/run-1700000002-1/events.jsonl");
    let mut text = fs::read_to_string(&events).unwrap();
    text.push_str("not an event\n");
    fs::write(&events, text).unwrap();

    let refused = resumable_members(&w.store, "repo-1", &names(&["COM-02"])).unwrap_err();

    assert!(refused.contains("run-1700000002-1"), "{refused}");
}

#[test]
fn later_run_of_names_the_later_run_of_a_run_s_feature_and_the_features_it_took() {
    let w = world("later-of");
    let group = failed_run(&w, "run-1700000001-5", "COM-02", "2026-01-01T00:00:01Z");
    record_group(&w.store, &group, &["COM-02".to_string(), "COM-03".to_string()]).unwrap();
    failed_run(&w, "run-1700000099-5", "COM-02", "2026-01-01T00:00:50Z");
    failed_run(&w, "run-1700000200-5", "COM-09", "2026-01-01T00:01:00Z");

    assert_eq!(
        later_run_of(&w.store, "run-1700000001-5").unwrap(),
        Some(("run-1700000099-5".to_string(), vec!["COM-02".to_string()]))
    );
    assert_eq!(later_run_of(&w.store, "run-1700000099-5").unwrap(), None);
    assert_eq!(later_run_of(&w.store, "run-1700000200-5").unwrap(), None);
}
