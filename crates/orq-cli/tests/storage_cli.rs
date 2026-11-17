use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-storage-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        Sandbox { home }
    }

    fn orq(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.output().unwrap()
    }

    fn data_file(&self, rel: &str) -> PathBuf {
        self.home.join("data/orq").join(rel)
    }

    /// A finished run, ended long ago, with a log of `bytes` bytes.
    fn failed_run(&self, run_id: &str, ended: &str, bytes: usize) {
        let store = RunStore::new(&self.home.join("state/orq"));
        store
            .create(&RunRecord {
                delivery: Default::default(),
                run_id: run_id.to_string(),
                project_id: "p".to_string(),
                feature_id: "F".to_string(),
                remote: "origin".to_string(),
                provider: "github".to_string(),
                target_branch: "main".to_string(),
                base_sha: "a".repeat(40),
                branch: "b".to_string(),
                worktree: self.home.join("no-such-worktree"),
                repository: String::new(),
                profiles: BTreeMap::new(),
            })
            .unwrap();
        store
            .append_event(&Event {
                schema_version: 1,
                event_id: "e1".to_string(),
                run_id: run_id.to_string(),
                project_id: "p".to_string(),
                feature_id: "F".to_string(),
                kind: "run.failed".to_string(),
                stage: None,
                attempt: 1,
                occurred_at: ended.to_string(),
                payload: serde_json::json!({ "reason": "boom" }),
            })
            .unwrap();
        let log = self.data_file(&format!("runs/{run_id}/events.jsonl"));
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(log, vec![b'x'; bytes]).unwrap();
    }
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn status_and_list_describe_orqs_own_run_files() {
    let s = Sandbox::new("status");
    s.failed_run("old-run", "2020-01-01T00:00:00Z", 2048);
    let status = s.orq(&["storage", "status"]);
    assert!(status.status.success(), "{}", text(&status));
    let out = text(&status);
    assert!(out.contains("old-run") || out.contains("log"), "{out}");
    assert!(out.contains("2.0 KiB") || out.contains("2048"), "{out}");
    let list = s.orq(&["storage", "list"]);
    assert!(list.status.success(), "{}", text(&list));
    assert!(text(&list).contains("runs/old-run/events.jsonl"), "{}", text(&list));
}

#[test]
fn a_dry_run_shows_what_how_much_and_why_and_removes_nothing() {
    let s = Sandbox::new("dry");
    s.failed_run("old-run", "2020-01-01T00:00:00Z", 2048);
    let out = s.orq(&["storage", "prune", "--dry-run"]);
    assert!(out.status.success(), "{}", text(&out));
    let shown = text(&out);
    assert!(shown.contains("runs/old-run/events.jsonl"), "{shown}");
    assert!(shown.contains("KiB") || shown.contains("2048"), "{shown}");
    assert!(shown.to_lowercase().contains("retention") || shown.contains("days") || shown.contains("old"), "{shown}");
    assert!(s.data_file("runs/old-run/events.jsonl").exists());
}

#[test]
fn prune_removes_old_finished_runs_files_and_a_second_prune_finds_nothing() {
    let s = Sandbox::new("prune");
    s.failed_run("old-run", "2020-01-01T00:00:00Z", 2048);
    s.failed_run("fresh-run", "2999-01-01T00:00:00Z", 10);
    let out = s.orq(&["storage", "prune"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!s.data_file("runs/old-run/events.jsonl").exists());
    assert!(s.data_file("runs/fresh-run/events.jsonl").exists());
    assert!(text(&out).contains("2.0 KiB") || text(&out).contains("2048"), "{}", text(&out));
    let again = s.orq(&["storage", "prune"]);
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("Nothing to prune"), "{}", text(&again));
}

const PINS_REMOVED: &str = "run pins were removed: storage cleanup asks before removing anything";

#[test]
fn run_pin_and_unpin_are_gone_and_say_so() {
    let s = Sandbox::new("pin-removed");
    s.failed_run("old-run", "2020-01-01T00:00:00Z", 100);
    for command in ["pin", "unpin"] {
        let out = s.orq(&["run", command, "old-run"]);
        assert_eq!(out.status.code(), Some(2), "{command}: {}", text(&out));
        assert!(text(&out).contains(PINS_REMOVED), "{command}: {}", text(&out));
    }
    let usage = text(&s.orq(&["storage", "frobnicate"]));
    assert!(!usage.contains("run pin"), "the usage no longer offers pins: {usage}");
}

#[test]
fn storage_status_has_no_pinned_line_and_prune_removes_a_run_that_would_once_have_been_pinned() {
    let s = Sandbox::new("no-pins");
    s.failed_run("old-run", "2020-01-01T00:00:00Z", 100);
    let status = text(&s.orq(&["storage", "status"]));
    assert!(!status.to_lowercase().contains("pinned"), "{status}");
    let pruned = s.orq(&["storage", "prune"]);
    assert!(pruned.status.success(), "{}", text(&pruned));
    assert!(!text(&pruned).contains("pinned"), "{}", text(&pruned));
    assert!(!s.data_file("runs/old-run/events.jsonl").exists());
}

/// The database as an Orq from before pins were removed left it: at schema version 6, with a
/// `run_pins` table holding `pins`.
fn database_from_before_pins_were_removed(s: &Sandbox, pins: &[&str]) -> rusqlite::Connection {
    assert!(s.orq(&["storage", "status"]).status.success());
    let db = rusqlite::Connection::open(s.data_file("orq.db")).unwrap();
    db.execute_batch("CREATE TABLE run_pins (run_id TEXT NOT NULL PRIMARY KEY, pinned_at INTEGER NOT NULL); PRAGMA user_version = 6;").unwrap();
    for pin in pins {
        db.execute("INSERT INTO run_pins (run_id, pinned_at) VALUES (?1, 1)", [pin]).unwrap();
    }
    db
}

fn has_pins_table(db: &rusqlite::Connection) -> bool {
    db.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'run_pins')", [], |row| row.get(0)).unwrap()
}

#[test]
fn an_empty_pins_table_is_dropped_without_a_word() {
    let s = Sandbox::new("pins-empty");
    let db = database_from_before_pins_were_removed(&s, &[]);
    let status = s.orq(&["storage", "status"]);
    assert!(status.status.success(), "{}", text(&status));
    assert!(!text(&status).contains("pinned"), "{}", text(&status));
    assert!(!has_pins_table(&db), "the empty table is dropped");
}

#[test]
fn pins_left_from_before_are_mentioned_once_by_storage_status_and_then_forgotten() {
    let s = Sandbox::new("pins-kept");
    let db = database_from_before_pins_were_removed(&s, &["run-a", "run-b"]);
    s.failed_run("run-a", "2020-01-01T00:00:00Z", 100);
    assert!(s.orq(&["storage", "prune", "--dry-run"]).status.success());
    assert!(has_pins_table(&db), "a table with pins in it is kept until status has said so");

    let first = s.orq(&["storage", "status"]);
    assert!(first.status.success(), "{}", text(&first));
    assert!(text(&first).contains("2 runs were pinned before pins were removed: they're now pruned like any other run"), "{}", text(&first));
    let second = text(&s.orq(&["storage", "status"]));
    assert!(!second.contains("pinned"), "said once: {second}");
    assert!(!has_pins_table(&db));

    s.orq(&["storage", "prune"]);
    assert!(!s.data_file("runs/run-a/events.jsonl").exists(), "a run pinned before is pruned like any other");
}

#[test]
fn an_unknown_storage_command_is_a_usage_error() {
    let s = Sandbox::new("usage");
    let out = s.orq(&["storage", "frobnicate"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

#[test]
fn a_dry_run_leaves_an_interrupted_prunes_journal_untouched() {
    let s = Sandbox::new("dry-journal");
    // Opening the database through the CLI creates it; then plant an interrupted removal of a file that is gone.
    assert!(s.orq(&["storage", "status"]).status.success());
    let db = s.data_file("orq.db");
    let store = orq_core::store::Store::open(&db).unwrap();
    store.begin_removal("old-run", "runs/old-run/events.jsonl").unwrap();
    drop(store);

    let out = s.orq(&["storage", "prune", "--dry-run"]);
    assert!(out.status.success(), "{}", text(&out));
    let store = orq_core::store::Store::open(&db).unwrap();
    assert_eq!(store.pending_removals().unwrap().len(), 1, "a dry run must not settle the journal");

    let out = s.orq(&["storage", "prune"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(store.pending_removals().unwrap().is_empty(), "a real prune settles it");
}

#[test]
fn status_reports_the_last_real_cleanup_and_a_dry_run_does_not_count() {
    let s = Sandbox::new("last-cleanup");
    s.failed_run("old-run", "2020-01-01T00:00:00Z", 100);
    assert!(text(&s.orq(&["storage", "status"])).contains("last cleanup: never"));
    s.orq(&["storage", "prune", "--dry-run"]);
    assert!(text(&s.orq(&["storage", "status"])).contains("last cleanup: never"), "a preview is not a cleanup");
    s.orq(&["storage", "prune"]);
    let after = text(&s.orq(&["storage", "status"]));
    assert!(after.contains("last cleanup:") && !after.contains("last cleanup: never"), "{after}");
    assert!(after.contains("retention:") && after.contains("failed 30"), "the policy is shown: {after}");
}
