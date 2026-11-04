use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Output};

use orq_orchestrator::{Event, RunRecord, RunStore};

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-maintain-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("config/orq")).unwrap();
        Sandbox { home }
    }

    fn config(&self, text: &str) {
        std::fs::write(self.home.join("config/orq/config.toml"), text).unwrap();
    }

    fn orq(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command
    }

    fn data_file(&self, rel: &str) -> PathBuf {
        self.home.join("data/orq").join(rel)
    }

    fn failed_run(&self, run_id: &str) {
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
                occurred_at: "2020-01-01T00:00:00Z".to_string(),
                payload: serde_json::json!({ "reason": "boom" }),
            })
            .unwrap();
        let log = self.data_file(&format!("runs/{run_id}/events.jsonl"));
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(log, vec![b'x'; 512]).unwrap();
    }
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

const ON: &str = "[storage_maintenance]\nenabled = true\ninterval_days = 1\n";

#[test]
fn maintenance_is_off_by_default_and_removes_nothing() {
    let s = Sandbox::new("off");
    s.failed_run("old-run");
    let out = s.orq(&["storage", "maintain"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("off"), "{}", text(&out));
    assert!(s.data_file("runs/old-run/events.jsonl").exists());
    assert!(!s.data_file("last-cleanup").exists());
}

#[test]
fn an_enabled_maintenance_prunes_once_per_interval_and_records_the_cleanup() {
    let s = Sandbox::new("on");
    s.config(ON);
    s.failed_run("old-run");
    let out = s.orq(&["storage", "maintain"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!s.data_file("runs/old-run/events.jsonl").exists(), "{}", text(&out));
    assert!(s.data_file("last-cleanup").exists());
    assert!(text(&s.orq(&["storage", "status"])).contains("last cleanup: 20"));

    s.failed_run("another-old-run");
    let again = s.orq(&["storage", "maintain"]);
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("not due"), "{}", text(&again));
    assert!(s.data_file("runs/another-old-run/events.jsonl").exists());
}

#[test]
fn maintenance_keeps_what_a_manual_prune_keeps() {
    let s = Sandbox::new("pinned");
    s.config(ON);
    s.failed_run("kept");
    s.failed_run("gone");
    assert!(s.orq(&["run", "pin", "kept"]).status.success());
    assert!(s.orq(&["storage", "maintain"]).status.success());
    assert!(s.data_file("runs/kept/events.jsonl").exists());
    assert!(!s.data_file("runs/gone/events.jsonl").exists());
}

#[test]
fn maintenance_never_runs_while_another_prune_holds_the_lock() {
    let s = Sandbox::new("locked");
    s.config(ON);
    s.failed_run("old-run");
    let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(s.data_file("prune.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    let out = s.orq(&["storage", "maintain"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("another prune"), "{}", text(&out));
    assert!(s.data_file("runs/old-run/events.jsonl").exists());
    assert!(!s.data_file("last-cleanup").exists());
    let manual = s.orq(&["storage", "prune"]);
    assert!(!manual.status.success(), "{}", text(&manual));
    assert!(s.data_file("runs/old-run/events.jsonl").exists());
    drop(held);
    assert!(s.orq(&["storage", "prune"]).status.success());
    assert!(!s.data_file("runs/old-run/events.jsonl").exists());
}

#[test]
fn an_invalid_maintenance_setting_is_refused_and_prunes_nothing() {
    let s = Sandbox::new("bad");
    s.failed_run("old-run");
    for bad in ["[storage_maintenance]\nenabled = true\n", "[storage_maintenance]\nenabled = true\ninterval_days = 0\n", "[storage_maintenance]\nenabled = true\ninterval_days = 1\nbogus = 1\n"] {
        s.config(bad);
        let out = s.orq(&["storage", "maintain"]);
        assert!(!out.status.success(), "{bad}: {}", text(&out));
        assert!(s.data_file("runs/old-run/events.jsonl").exists());
    }
}

#[test]
fn an_unreadable_or_malformed_cleanup_time_stops_maintenance_instead_of_repeating_it() {
    let s = Sandbox::new("stamp");
    s.config(ON);
    s.failed_run("old-run");
    std::fs::create_dir_all(s.data_file("last-cleanup")).unwrap();
    let out = s.orq(&["storage", "maintain"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(s.data_file("runs/old-run/events.jsonl").exists());

    std::fs::remove_dir_all(s.data_file("last-cleanup")).unwrap();
    std::fs::write(s.data_file("last-cleanup"), "not a time").unwrap();
    let out = s.orq(&["storage", "maintain"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(s.data_file("runs/old-run/events.jsonl").exists());
}

#[test]
fn a_fifo_planted_at_the_lock_path_cannot_hang_maintenance() {
    let s = Sandbox::new("fifo");
    s.config(ON);
    s.failed_run("old-run");
    let path = std::ffi::CString::new(s.data_file("prune.lock").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let mut child = s.command(&["storage", "maintain"]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > std::time::Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let status = status.expect("maintenance hung on a FIFO at the lock path");
    assert!(!status.success());
    assert!(s.data_file("runs/old-run/events.jsonl").exists());
}

#[test]
fn the_off_and_not_due_paths_do_not_open_or_create_the_database() {
    let s = Sandbox::new("nodb");
    assert!(s.orq(&["storage", "maintain"]).status.success());
    assert!(!s.data_file("orq.db").exists(), "disabled maintenance opened the database");

    s.config(ON);
    std::fs::write(s.data_file("last-cleanup"), "99999999999").unwrap();
    let out = s.orq(&["storage", "maintain"]);
    assert!(text(&out).contains("not due"), "{}", text(&out));
    assert!(!s.data_file("orq.db").exists(), "a not-due check opened the database");
}

#[test]
fn maintenance_runs_again_once_the_interval_has_passed() {
    let s = Sandbox::new("due");
    s.config(ON);
    s.failed_run("old-run");
    // A cleanup at the start of the epoch is long past a one-day interval.
    std::fs::write(s.data_file("last-cleanup"), "1").unwrap();
    let out = s.orq(&["storage", "maintain"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!s.data_file("runs/old-run/events.jsonl").exists(), "{}", text(&out));
    let stamp: u64 = std::fs::read_to_string(s.data_file("last-cleanup")).unwrap().trim().parse().unwrap();
    assert!(stamp > 1_000_000_000, "the cleanup time was not renewed: {stamp}");
}

#[test]
fn a_cleanup_time_that_cannot_be_recorded_stops_maintenance_before_it_deletes_anything() {
    use std::os::unix::process::CommandExt;
    let s = Sandbox::new("norecord");
    s.config(ON);
    s.failed_run("old-run");
    // Create the database and lock first, so that only writing the cleanup time can hit the limit.
    assert!(s.orq(&["storage", "status"]).status.success());
    std::fs::write(s.data_file("prune.lock"), "").unwrap();
    let mut command = s.command(&["storage", "maintain"]);
    // No file may grow: a new file's first write fails. SIGXFSZ is ignored so the write returns an error.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
        });
    }
    let out = command.output().unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("cannot record the cleanup time"), "{}", text(&out));
    assert!(s.data_file("runs/old-run/events.jsonl").exists(), "{}", text(&out));
    assert!(!s.data_file("last-cleanup").exists());
}
