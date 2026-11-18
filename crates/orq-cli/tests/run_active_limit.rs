//! A project may limit how many of its runs are active at once and how much free disk a run needs
//! to start. `orq run` refuses a start over either limit before it creates anything, exit 3.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::project_settings::{ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

const EXIT_LIMIT: i32 = 3;

fn home(tag: &str) -> PathBuf {
    let home = orq_test_support::process_temp_dir().join(format!("orq-run-active-limit-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    std::fs::write(home.join("config/orq/config.toml"), "[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[roles]\nimplementer = \"opus\"\n").unwrap();
    home
}

fn save(home: &Path, repo: &Path, settings: &ProjectSettings) {
    let base = home.to_path_buf();
    let roots = StorageRoots::resolve(&|name| match name {
        "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
        "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
        "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
        "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
        _ => None,
    })
    .unwrap();
    roots.create_all().unwrap();
    let mut store = Store::open(&roots.database()).unwrap();
    let root = std::fs::canonicalize(repo).unwrap();
    let revision = store.project_settings(&root).unwrap().revision;
    assert!(matches!(store.save_project_settings(&root, revision, settings).unwrap(), SaveOutcome::Saved { .. }));
}

fn orq(repo: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command
        .args(args)
        .arg("--repo")
        .arg(repo)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("ORQ_TEST_FREE_DISK_BYTES");
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn start(repo: &Path, home: &Path, feature: &str, extra: &[&str]) -> Output {
    start_with(repo, home, feature, extra, &[])
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A repository with pending features and an `origin` a run could open its review on.
fn repo() -> GitFixture {
    let repo = GitFixture::new();
    let added = Command::new("git").arg("-C").arg(repo.root()).args(["remote", "add", "origin", "git@github.com:o/r.git"]).status().unwrap();
    assert!(added.success());
    write_ledger(repo.root(), &[LedgerEntry::new("COM-01", "pending"), LedgerEntry::new("COM-02", "pending"), LedgerEntry::new("COM-03", "pending")]);
    repo
}

fn project(repo: &Path) -> orq_git::Project {
    orq_git::resolve_project(repo).unwrap()
}

fn runs(home: &Path) -> RunStore {
    RunStore::new(&home.join("state/orq"))
}

/// A run of `feature` in the project at `repo`, started and not ended.
fn seed_active(home: &Path, repo: &Path, run_id: &str, feature: &str) {
    let store = runs(home);
    store
        .start_run(&RunRecord {
            delivery: Default::default(),
            run_id: run_id.into(),
            project_id: orq_git::project_id(&project(repo)),
            feature_id: feature.into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: format!("orq/{run_id}"),
            worktree: repo.join(".claude/worktrees").join(run_id),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    append(home, repo, run_id, feature, "stage.started", serde_json::json!({}));
}

fn append(home: &Path, repo: &Path, run_id: &str, feature: &str, kind: &str, payload: serde_json::Value) {
    let store = runs(home);
    let n = store.events(run_id).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{run_id}-e{n}"),
            run_id: run_id.into(),
            project_id: orq_git::project_id(&project(repo)),
            feature_id: feature.into(),
            kind: kind.into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-18T10:00:00Z".into(),
            payload,
        })
        .unwrap();
}

fn limited(max_active_runs: Option<u8>, min_free_disk_gb: Option<u32>) -> ProjectSettings {
    ProjectSettings { max_active_runs, min_free_disk_gb, ..ProjectSettings::default() }
}

#[test]
fn a_start_at_the_limit_exits_3_with_the_reason_and_creates_nothing_and_starts_once_the_run_ended() {
    let repo = repo();
    let home = home("limit");
    save(&home, repo.root(), &limited(Some(1), None));
    seed_active(&home, repo.root(), "run-seed", "COM-02");
    let name = project(repo.root()).name;

    let refused = start(repo.root(), &home, "COM-01", &[]);
    assert_eq!(refused.status.code(), Some(EXIT_LIMIT), "{}", text(&refused.stderr));
    let expected = format!("{name} already has 1 active run (run-seed, COM-02): wait for it to finish, or raise the limit in project settings");
    assert!(text(&refused.stderr).contains(&expected), "{}", text(&refused.stderr));
    assert_eq!(text(&refused.stdout), "", "no run id was printed");
    assert_eq!(runs(&home).run_ids().unwrap(), vec!["run-seed".to_string()], "no run was recorded");
    assert!(!repo.root().join(".claude/worktrees").exists(), "no worktree was made");

    append(&home, repo.root(), "run-seed", "COM-02", "run.completed", serde_json::json!({"ending": "review_ready"}));
    let ready = start(repo.root(), &home, "COM-01", &["--check"]);
    assert_eq!(ready.status.code(), Some(0), "{}", text(&ready.stderr));
    assert_eq!(text(&ready.stdout), "state: ready\n");
}

fn start_with(repo: &Path, home: &Path, feature: &str, extra: &[&str], env: &[(&str, &str)]) -> Output {
    let mut args = vec!["run", "--remote", "origin", "--target", "main", "--feature", feature];
    args.extend_from_slice(extra);
    orq(repo, home, &args, env)
}

#[test]
fn a_paused_run_counts_toward_the_limit_but_its_resume_is_not_refused_by_it() {
    let repo = repo();
    let home = home("resume");
    save(&home, repo.root(), &limited(Some(1), None));
    seed_active(&home, repo.root(), "run-paused", "COM-01");
    append(&home, repo.root(), "run-paused", "COM-01", "run.pause_requested", serde_json::json!({}));
    append(&home, repo.root(), "run-paused", "COM-01", "run.paused", serde_json::json!({}));

    let other = start(repo.root(), &home, "COM-02", &["--check"]);
    assert_eq!(other.status.code(), Some(EXIT_LIMIT), "a paused run is active: {}", text(&other.stderr));
    assert!(text(&other.stderr).contains("already has 1 active run (run-paused, COM-01)"), "{}", text(&other.stderr));

    let resumed = start(repo.root(), &home, "COM-01", &["--check", "--resume"]);
    assert_ne!(resumed.status.code(), Some(EXIT_LIMIT), "{}", text(&resumed.stderr));
    assert!(!text(&resumed.stderr).contains("raise the limit"), "{}", text(&resumed.stderr));
}

#[test]
fn a_start_or_a_resume_under_the_disk_floor_exits_3_with_what_is_free_and_what_is_needed() {
    let repo = repo();
    let home = home("disk");
    save(&home, repo.root(), &limited(None, Some(15)));
    let name = project(repo.root()).name;
    let low = [("ORQ_TEST_FREE_DISK_BYTES", "9800000000")];

    for extra in [&["--check"][..], &[][..], &["--detach"][..], &["--check", "--resume"][..]] {
        let refused = start_with(repo.root(), &home, "COM-01", extra, &low);
        assert_eq!(refused.status.code(), Some(EXIT_LIMIT), "{extra:?}: {}", text(&refused.stderr));
        let stderr = text(&refused.stderr);
        let needed = format!("({name} needs 15 GB to start a run): free some space, or lower the floor in project settings");
        assert!(stderr.contains("only 9.8 GB free on ") && stderr.contains(&needed), "{extra:?}: {stderr}");
        assert_eq!(text(&refused.stdout), "", "{extra:?}");
    }
    assert!(runs(&home).run_ids().unwrap_or_default().is_empty(), "no run was recorded");
    assert!(!repo.root().join(".claude/worktrees").exists(), "no worktree was made");

    let enough = start_with(repo.root(), &home, "COM-01", &["--check"], &[("ORQ_TEST_FREE_DISK_BYTES", "15000000000")]);
    assert_eq!(enough.status.code(), Some(0), "{}", text(&enough.stderr));
}

#[test]
fn a_detached_start_at_the_limit_is_refused_before_it_launches_and_without_a_limit_runs_do_not_count() {
    let repo = repo();
    let home = home("detach");
    save(&home, repo.root(), &limited(Some(1), None));
    seed_active(&home, repo.root(), "run-seed", "COM-02");

    let refused = start(repo.root(), &home, "COM-01", &["--detach"]);
    assert_eq!(refused.status.code(), Some(EXIT_LIMIT), "{}", text(&refused.stderr));
    assert!(text(&refused.stderr).contains("already has 1 active run (run-seed, COM-02)"), "{}", text(&refused.stderr));
    assert_eq!(text(&refused.stdout), "", "nothing was launched");
    assert_eq!(runs(&home).run_ids().unwrap(), vec!["run-seed".to_string()]);

    save(&home, repo.root(), &limited(None, None));
    let unlimited = start(repo.root(), &home, "COM-01", &["--check"]);
    assert_eq!(unlimited.status.code(), Some(0), "{}", text(&unlimited.stderr));
}

#[test]
fn the_disk_floor_is_measured_on_the_filesystem_the_worktrees_directory_is_on() {
    let elsewhere = std::path::PathBuf::from("/dev/shm");
    let device = |path: &Path| std::os::unix::fs::MetadataExt::dev(&std::fs::metadata(path).unwrap());
    let repo = repo();
    if !elsewhere.is_dir() || device(&elsewhere) == device(repo.root()) {
        return;
    }
    let home = home("disk-elsewhere");
    save(&home, repo.root(), &limited(None, Some(15)));
    let target = elsewhere.join(format!("orq-run-active-limit-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&target);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::create_dir_all(repo.root().join(".claude")).unwrap();
    std::os::unix::fs::symlink(&target, repo.root().join(".claude/worktrees")).unwrap();

    let refused = start_with(repo.root(), &home, "COM-01", &["--check"], &[("ORQ_TEST_FREE_DISK_BYTES", "9800000000")]);
    let _ = std::fs::remove_dir_all(&target);
    assert_eq!(refused.status.code(), Some(EXIT_LIMIT), "{}", text(&refused.stderr));
    assert!(text(&refused.stderr).contains("only 9.8 GB free on /dev/shm ("), "{}", text(&refused.stderr));
}

#[test]
fn a_resume_measures_the_disk_its_recorded_worktree_is_on_and_a_new_start_the_worktrees_directory() {
    let elsewhere = PathBuf::from("/dev/shm");
    let device = |path: &Path| std::os::unix::fs::MetadataExt::dev(&std::fs::metadata(path).unwrap());
    let repo = repo();
    if !elsewhere.is_dir() || device(&elsewhere) == device(repo.root()) {
        return;
    }
    let home = home("disk-resumed");
    save(&home, repo.root(), &limited(None, Some(15)));
    let worktree = elsewhere.join(format!("orq-run-active-limit-resumed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&worktree);
    std::fs::create_dir_all(&worktree).unwrap();
    runs(&home)
        .start_run(&RunRecord {
            delivery: Default::default(),
            run_id: "run-moved".into(),
            project_id: orq_git::project_id(&project(repo.root())),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: "orq/run-moved".into(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    append(&home, repo.root(), "run-moved", "COM-01", "stage.started", serde_json::json!({}));
    append(&home, repo.root(), "run-moved", "COM-01", "run.completed", serde_json::json!({"ending": "blocked"}));
    // An ended run gives its feature back.
    runs(&home).finish_run(&orq_git::project_id(&project(repo.root())), "COM-01", "run-moved").unwrap();
    let low = [("ORQ_TEST_FREE_DISK_BYTES", "9800000000")];

    let resumed = start_with(repo.root(), &home, "COM-01", &["--check", "--resume"], &low);
    let fresh = start_with(repo.root(), &home, "COM-02", &["--check"], &low);
    let _ = std::fs::remove_dir_all(&worktree);
    assert_eq!(resumed.status.code(), Some(EXIT_LIMIT), "{}", text(&resumed.stderr));
    assert!(text(&resumed.stderr).contains("only 9.8 GB free on /dev/shm ("), "{}", text(&resumed.stderr));
    assert_eq!(fresh.status.code(), Some(EXIT_LIMIT), "{}", text(&fresh.stderr));
    assert!(!text(&fresh.stderr).contains("/dev/shm"), "{}", text(&fresh.stderr));
}

/// A run of `feature` that blocked and gave its feature back, as an ended run does.
fn seed_blocked(home: &Path, repo: &Path, run_id: &str, feature: &str) {
    seed_active(home, repo, run_id, feature);
    append(home, repo, run_id, feature, "run.completed", serde_json::json!({"ending": "blocked"}));
    runs(home).finish_run(&orq_git::project_id(&project(repo)), feature, run_id).unwrap();
}

#[test]
fn a_blocked_run_holds_no_slot_so_a_start_passes_but_its_own_resume_is_held_to_the_limit_and_a_paused_resume_is_not() {
    let repo = repo();
    let home = home("blocked");
    save(&home, repo.root(), &limited(Some(1), None));
    seed_blocked(&home, repo.root(), "run-blocked", "COM-01");

    let fresh = start(repo.root(), &home, "COM-02", &["--check"]);
    assert_eq!(fresh.status.code(), Some(0), "a blocked run is not active: {}", text(&fresh.stderr));

    seed_active(&home, repo.root(), "run-running", "COM-02");
    let resumed = start(repo.root(), &home, "COM-01", &["--check", "--resume"]);
    assert_eq!(resumed.status.code(), Some(EXIT_LIMIT), "{}", text(&resumed.stderr));
    let name = project(repo.root()).name;
    let expected = format!("{name} already has 1 active run (run-running, COM-02): wait for it to finish, or raise the limit in project settings");
    assert!(text(&resumed.stderr).contains(&expected), "{}", text(&resumed.stderr));

    seed_active(&home, repo.root(), "run-paused", "COM-03");
    append(&home, repo.root(), "run-paused", "COM-03", "run.pause_requested", serde_json::json!({}));
    append(&home, repo.root(), "run-paused", "COM-03", "run.paused", serde_json::json!({}));
    let paused = start(repo.root(), &home, "COM-03", &["--check", "--resume"]);
    assert_ne!(paused.status.code(), Some(EXIT_LIMIT), "{}", text(&paused.stderr));
    assert!(!text(&paused.stderr).contains("raise the limit"), "{}", text(&paused.stderr));
}
