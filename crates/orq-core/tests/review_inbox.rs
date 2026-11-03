use std::path::PathBuf;

use orq_core::review_inbox::{detail, inbox};
use orq_core::review_lifecycle::{Observation, ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use serde_json::json;

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OURS: &str = "orq-cli-0a1b2c3d";
const THEIRS: &str = "other-app-99887766";

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn report(status: &str, risks: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "status": status,
        "summary": {"files_changed": 2, "added": 10, "deleted": 3, "binary_files": 1},
        "files": [
            {"path": "src/a.rs", "old_path": null, "added": 10, "deleted": 3, "binary": false},
            {"path": "logo.png", "old_path": null, "added": null, "deleted": null, "binary": true}
        ],
        "tests": {"status": "passed", "commands": [{"command": "cargo test", "exit_code": 0}]},
        "risks": risks,
        "rounds": [
            {"round": 1, "summary": "looked at auth", "findings": 2, "confirmed": 1, "potential": 0, "dismissed": 1, "unresolved": 1},
            {"round": 2, "summary": null, "findings": 0, "confirmed": 0, "potential": 0, "dismissed": 0, "unresolved": 0}
        ],
        "gaps": ["test evidence is not bound to a commit"]
    }))
    .unwrap()
}

fn risk(severity: &str) -> serde_json::Value {
    json!({"severity": severity, "file": "src/a.rs", "line": 4, "claim": "token logged"})
}

fn save(roots: &StorageRoots, run: &str, project: &str, feature: &str, head: &str, body: Vec<u8>) {
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, run, project, feature, head, &body).unwrap();
}

#[test]
fn the_inbox_lists_every_project_with_its_status_risk_and_staleness() {
    let roots = roots("rows");
    save(&roots, "run-1", OURS, "COM-01", HEAD_A, report("current", json!([risk("low"), risk("high")])));
    save(&roots, "run-2", THEIRS, "APP-07", HEAD_A, report("incomplete", json!([])));
    let store = Store::open(&roots.database()).unwrap();
    let mut open = ReviewLifecycle::associate("github", "7", "https://example.test/7", HEAD_A);
    open = open.apply(Ok(Observation { state: ProviderState::Open, head_sha: HEAD_B.into() }));
    store.save_lifecycle("run-1", &open).unwrap();

    let rows = inbox(&roots, OURS, "orq-cli").unwrap();
    assert_eq!(rows.len(), 2);
    let ours = rows.iter().find(|r| r.run_id == "run-1").unwrap();
    assert_eq!((ours.feature.as_str(), ours.project.as_str(), ours.risk.as_str(), ours.stale), ("COM-01", "orq-cli", "high", true));
    assert_eq!(ours.status, "open");
    let theirs = rows.iter().find(|r| r.run_id == "run-2").unwrap();
    assert_eq!((theirs.project.as_str(), theirs.status.as_str(), theirs.risk.as_str(), theirs.stale), ("other-app", "draft", "-", false));
}

#[test]
fn a_run_with_several_reports_shows_only_the_latest() {
    let roots = roots("latest");
    save(&roots, "run-1", OURS, "COM-01", HEAD_A, report("incomplete", json!([risk("high")])));
    save(&roots, "run-1", OURS, "COM-01", HEAD_B, report("current", json!([])));
    let rows = inbox(&roots, OURS, "orq-cli").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].status.as_str(), rows[0].risk.as_str()), ("ready", "-"));
}

#[test]
fn no_database_means_an_empty_inbox_and_none_is_created() {
    let roots = roots("none");
    assert!(inbox(&roots, OURS, "orq-cli").unwrap().is_empty());
    assert!(!roots.database().exists());
}

#[test]
fn an_unreadable_report_still_lists_the_review_and_the_detail_says_why() {
    let roots = roots("broken");
    save(&roots, "run-1", OURS, "COM-01", HEAD_A, report("current", json!([])));
    let path: PathBuf = roots.data.join(format!("runs/run-1/review/{HEAD_A}/review.json"));
    std::fs::write(&path, b"{ not json").unwrap();
    let rows = inbox(&roots, OURS, "orq-cli").unwrap();
    assert_eq!((rows[0].status.as_str(), rows[0].risk.as_str()), ("unreadable", "-"));
    let err = detail(&roots, "run-1").unwrap_err();
    assert!(err.contains("run-1") && err.contains("unreadable"), "{err}");
}

#[test]
fn the_detail_has_a_section_for_each_part_of_the_report() {
    let roots = roots("detail");
    save(&roots, "run-1", OURS, "COM-01", HEAD_A, report("incomplete", json!([risk("high")])));
    let d = detail(&roots, "run-1").unwrap();
    let all = |lines: &Vec<String>| lines.join("\n");
    assert!(all(&d.summary).contains("2 files changed") && all(&d.summary).contains("+10 -3"), "{:?}", d.summary);
    assert!(all(&d.summary).contains("test evidence is not bound to a commit"), "{:?}", d.summary);
    assert!(all(&d.risks).contains("high") && all(&d.risks).contains("src/a.rs:4") && all(&d.risks).contains("token logged"), "{:?}", d.risks);
    assert!(all(&d.files).contains("src/a.rs  +10 -3") && all(&d.files).contains("logo.png  binary"), "{:?}", d.files);
    assert!(all(&d.tests).contains("passed") && all(&d.tests).contains("cargo test"), "{:?}", d.tests);
    assert!(all(&d.rounds).contains("round 1: looked at auth") && all(&d.rounds).contains("round 2: no summary recorded"), "{:?}", d.rounds);
}

#[test]
fn a_run_with_no_report_is_named_in_the_error() {
    let roots = roots("missing");
    assert!(detail(&roots, "run-9").unwrap_err().contains("run-9"));
}
