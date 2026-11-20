use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox {
    home: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-snapshots-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let status = Command::new("git").args(["init", "-q"]).arg(&repo).status().unwrap();
        assert!(status.success());
        Sandbox { home, repo }
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

    fn repo_arg(&self) -> &str {
        self.repo.to_str().unwrap()
    }

    fn add_feature(&self, name: &str) {
        let out = self.orq(&["feature", "add", "--repo", self.repo_arg(), "--name", name]);
        assert!(out.status.success(), "{}", text(&out));
    }

    /// `orq serve` on a free port, started in this sandbox; killed when dropped.
    fn serve(&self) -> ServeGuard {
        // orq.db is made the first time a command opens it; `storage status` does.
        let status = self.orq(&["storage", "status"]);
        assert!(status.status.success() && self.home.join("data/orq/orq.db").exists(), "{}", text(&status));
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(["serve", "--repo", self.repo_arg(), "--listen", "127.0.0.1:0"]);
        for (name, dir) in [("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"), ("XDG_CONFIG_HOME", "config"), ("XDG_CACHE_HOME", "cache")] {
            command.env(name, self.home.join(dir));
        }
        command.env("HOME", &self.home).env_remove("INVOCATION_ID").env_remove("NOTIFY_SOCKET").env_remove("HERDR_ACTIVE_PANE_CWD");
        ServeGuard(command.stdout(Stdio::null()).stderr(Stdio::inherit()).spawn().unwrap())
    }

    /// The database snapshots under the data folder, by name.
    fn db_snapshots(&self) -> Vec<PathBuf> {
        let Ok(dir) = std::fs::read_dir(self.home.join("data/orq/snapshots")) else { return Vec::new() };
        let mut found: Vec<PathBuf> = dir.flatten().map(|e| e.path()).filter(|p| name_of(p).starts_with("orq-db-")).collect();
        found.sort();
        found
    }

    /// The ledger snapshot files of every project, oldest name first.
    fn ledger_snapshots(&self) -> Vec<PathBuf> {
        let root = self.home.join("data/orq/snapshots");
        let mut found = Vec::new();
        let Ok(projects) = std::fs::read_dir(&root) else { return found };
        for project in projects.flatten().filter(|p| p.path().is_dir()) {
            for file in std::fs::read_dir(project.path()).unwrap().flatten() {
                found.push(file.path());
            }
        }
        found.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
        found
    }
}

struct ServeGuard(std::process::Child);

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Polls `check` for up to twenty seconds.
fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !check() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn name_of(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn storage_prune_leaves_every_snapshot_alone() {
    let s = Sandbox::new("prune-keeps");
    s.add_feature("first");
    s.add_feature("second");
    let dir = s.home.join("data/orq/snapshots");
    // An ancient database snapshot, far past any run retention.
    std::fs::write(dir.join("orq-db-2020-01-01.sqlite"), "old").unwrap();
    let before: Vec<PathBuf> = s.ledger_snapshots();
    assert!(!before.is_empty());

    let pruned = s.orq(&["storage", "prune"]);

    assert!(pruned.status.success(), "{}", text(&pruned));
    assert_eq!(s.ledger_snapshots(), before);
    assert_eq!(s.db_snapshots().len(), 1, "{}", text(&pruned));
}

#[test]
fn list_shows_each_snapshot_with_its_time_size_and_feature_counts_by_status() {
    let s = Sandbox::new("list");
    s.add_feature("first");
    s.add_feature("second");
    let snapshot = &s.ledger_snapshots()[0];
    let size = std::fs::metadata(snapshot).unwrap().len();

    let out = s.orq(&["snapshots", "list", "--project", s.repo_arg()]);

    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains(&name_of(snapshot)), "{shown}");
    assert!(shown.contains(&format!("{size} bytes")), "{shown}");
    assert!(shown.contains("draft 1"), "{shown}");
    assert!(shown.contains("pending 0"), "{shown}");
}

#[test]
fn diff_names_features_added_removed_or_changed_since_the_snapshot() {
    let s = Sandbox::new("diff");
    s.add_feature("kept");
    s.add_feature("gone");
    s.add_feature("third");
    let snapshot = name_of(s.ledger_snapshots().last().unwrap());
    // Hand-edit the ledger: drop "gone", move "kept" to pending, add "fresh".
    let ledger = s.repo.join(".tdd-features.json");
    let mut entries: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
    entries.retain(|e| e["featureName"] != "gone");
    entries[0]["status"] = "pending".into();
    entries.push(serde_json::json!({ "featureName": "fresh", "status": "draft" }));
    std::fs::write(&ledger, serde_json::to_vec(&entries).unwrap()).unwrap();

    let out = s.orq(&["snapshots", "diff", &snapshot, "--repo", s.repo_arg()]);

    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("added: fresh"), "{shown}");
    assert!(shown.contains("added: third"), "{shown}");
    assert!(shown.contains("removed: gone"), "{shown}");
    assert!(shown.contains("changed: kept (status draft -> pending)"), "{shown}");
}

#[test]
fn diff_names_a_feature_whose_description_changed_though_its_status_did_not() {
    let s = Sandbox::new("diff-fields");
    s.add_feature("kept");
    s.add_feature("other");
    s.add_feature("latest");
    // The newest snapshot is of the ledger as it was before "latest" was added.
    let snapshot = name_of(s.ledger_snapshots().last().unwrap());
    let ledger = s.repo.join(".tdd-features.json");
    let mut entries: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
    entries[0]["description"] = "reworded".into();
    std::fs::write(&ledger, serde_json::to_vec(&entries).unwrap()).unwrap();

    let out = s.orq(&["snapshots", "diff", &snapshot, "--repo", s.repo_arg()]);

    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("changed: kept"), "{shown}");
    assert!(!shown.contains("other"), "{shown}");
    assert!(shown.contains("added: latest"), "{shown}");
}

#[test]
fn restore_without_confirmation_changes_nothing_and_with_yes_snapshots_then_replaces_the_ledger() {
    let s = Sandbox::new("restore");
    s.add_feature("first");
    s.add_feature("second");
    let snapshot = s.ledger_snapshots()[0].clone();
    let wanted = std::fs::read(&snapshot).unwrap();
    let ledger = s.repo.join(".tdd-features.json");
    let current = std::fs::read(&ledger).unwrap();

    let refused = s.orq(&["snapshots", "restore", &name_of(&snapshot), "--repo", s.repo_arg()]);

    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(text(&refused).contains("added: second"), "{}", text(&refused));
    assert_eq!(std::fs::read(&ledger).unwrap(), current);

    let done = s.orq(&["snapshots", "restore", &name_of(&snapshot), "--repo", s.repo_arg(), "--yes"]);

    assert!(done.status.success(), "{}", text(&done));
    assert_eq!(std::fs::read(&ledger).unwrap(), wanted);
    let held: Vec<Vec<u8>> = s.ledger_snapshots().iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert!(held.contains(&current), "the ledger the restore replaced was not snapshotted");
}

#[test]
fn serve_writes_a_private_database_snapshot_at_start_up() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new("dbsnap");
    let _serve = s.serve();

    wait_until("the database snapshot", || !s.db_snapshots().is_empty());

    let snapshot = &s.db_snapshots()[0];
    let name = name_of(snapshot);
    assert!(name.starts_with("orq-db-") && name.ends_with(".sqlite"), "{name}");
    assert!(std::fs::read(snapshot).unwrap().starts_with(b"SQLite format 3\0"));
    assert_eq!(std::fs::metadata(snapshot).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn restoring_the_database_is_refused_while_serve_runs_and_replaces_it_once_it_has_stopped() {
    let s = Sandbox::new("dbrestore");
    let serve = s.serve();
    wait_until("the database snapshot", || !s.db_snapshots().is_empty());
    let name = name_of(&s.db_snapshots()[0]);

    let refused = s.orq(&["snapshots", "restore", &name, "--yes"]);

    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(text(&refused).contains("orq serve is running"), "{}", text(&refused));
    assert!(text(&refused).contains("stop"), "{}", text(&refused));

    drop(serve);
    let database = s.home.join("data/orq/orq.db");
    std::fs::write(&database, b"damaged").unwrap();
    let done = s.orq(&["snapshots", "restore", &name, "--yes"]);

    assert!(done.status.success(), "{}", text(&done));
    assert!(std::fs::read(&database).unwrap().starts_with(b"SQLite format 3\0"));
}

#[test]
fn restoring_the_database_without_confirmation_changes_nothing_and_with_yes_replaces_it() {
    let s = Sandbox::new("dbconfirm");
    assert!(s.orq(&["storage", "status"]).status.success());
    let dir = s.home.join("data/orq/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("orq-db-2026-01-01.sqlite"), b"from the snapshot").unwrap();
    let database = s.home.join("data/orq/orq.db");
    std::fs::write(&database, b"current").unwrap();

    let declined = s.orq(&["snapshots", "restore", "orq-db-2026-01-01.sqlite"]);

    assert!(!declined.status.success(), "{}", text(&declined));
    assert!(text(&declined).contains("unchanged"), "{}", text(&declined));
    assert_eq!(std::fs::read(&database).unwrap(), b"current");

    let done = s.orq(&["snapshots", "restore", "orq-db-2026-01-01.sqlite", "--yes"]);

    assert!(done.status.success(), "{}", text(&done));
    assert_eq!(std::fs::read(&database).unwrap(), b"from the snapshot");
}

#[test]
fn list_without_a_project_shows_the_database_snapshots_and_every_projects_ledger_snapshots() {
    let s = Sandbox::new("listall");
    s.add_feature("first");
    s.add_feature("second");
    let serve = s.serve();
    wait_until("the database snapshot", || !s.db_snapshots().is_empty());
    drop(serve);

    let out = s.orq(&["snapshots", "list"]);

    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains(&name_of(&s.db_snapshots()[0])), "{shown}");
    assert!(shown.contains(&name_of(&s.ledger_snapshots()[0])), "{shown}");
}

#[test]
fn a_ledger_snapshot_is_private_and_an_unchanged_ledger_is_not_snapshotted_twice() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new("private");
    s.add_feature("first");
    let ledger = s.repo.join(".tdd-features.json");
    let first_only = std::fs::read(&ledger).unwrap();
    s.add_feature("second");
    let snapshots = s.ledger_snapshots();
    assert_eq!(std::fs::metadata(&snapshots[0]).unwrap().permissions().mode() & 0o777, 0o600);

    // The ledger goes back to what the newest snapshot holds, then changes again.
    std::fs::write(&ledger, &first_only).unwrap();
    s.add_feature("third");

    assert_eq!(s.ledger_snapshots().len(), 1, "{:?}", s.ledger_snapshots());
}

#[test]
fn old_ledger_snapshots_are_trimmed_to_the_newest_200_plus_one_per_day_for_60_days() {
    let s = Sandbox::new("retention");
    s.add_feature("first");
    s.add_feature("second");
    let dir = s.ledger_snapshots()[0].parent().unwrap().to_path_buf();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let day = 86_400;
    let seed = |secs: u64, hash: &str| std::fs::write(dir.join(format!("ledger-{}-{hash}.json", orq_core::storage::utc_stamp(secs))), "[]").unwrap();
    // 210 snapshots within one day, three days back; one from long ago.
    for i in 0..210u64 {
        // Midday, so the 210 seconds cannot straddle midnight.
        seed(now / day * day - 3 * day + 12 * 3600 + i, &format!("{i:08x}"));
    }
    seed(now - 100 * day, "deadbeef");

    s.add_feature("third");

    let kept = s.ledger_snapshots();
    assert_eq!(kept.len(), 201, "the newest 200 and the newest of the older day");
    assert!(kept.iter().all(|p| !name_of(p).ends_with("-deadbeef.json")), "a snapshot older than 60 days was kept");
}

#[test]
fn a_feature_save_snapshots_the_ledger_it_replaces() {
    let s = Sandbox::new("save");
    s.add_feature("first");
    let before = std::fs::read(s.repo.join(".tdd-features.json")).unwrap();

    s.add_feature("second");

    let snapshots = s.ledger_snapshots();
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
    let name = name_of(&snapshots[0]);
    assert!(name.starts_with("ledger-") && name.ends_with(".json"), "{name}");
    assert_eq!(std::fs::read(&snapshots[0]).unwrap(), before);
}
