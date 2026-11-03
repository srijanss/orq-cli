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

#[test]
fn a_pinned_run_survives_prune_until_it_is_unpinned() {
    let s = Sandbox::new("pin");
    s.failed_run("old-run", "2020-01-01T00:00:00Z", 100);
    let pinned = s.orq(&["run", "pin", "old-run"]);
    assert!(pinned.status.success(), "{}", text(&pinned));
    let pruned = s.orq(&["storage", "prune"]);
    assert!(pruned.status.success(), "{}", text(&pruned));
    assert!(s.data_file("runs/old-run/events.jsonl").exists());
    assert!(text(&pruned).contains("old-run"), "the preview names what is kept: {}", text(&pruned));
    let unpinned = s.orq(&["run", "unpin", "old-run"]);
    assert!(unpinned.status.success(), "{}", text(&unpinned));
    s.orq(&["storage", "prune"]);
    assert!(!s.data_file("runs/old-run/events.jsonl").exists());
}

#[test]
fn an_unsafe_run_id_cannot_be_pinned() {
    let s = Sandbox::new("unsafe");
    let out = s.orq(&["run", "pin", "../etc"]);
    assert!(!out.status.success());
}

#[test]
fn run_without_pin_keeps_its_usage_error() {
    let s = Sandbox::new("usage");
    let out = s.orq(&["storage", "frobnicate"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}
