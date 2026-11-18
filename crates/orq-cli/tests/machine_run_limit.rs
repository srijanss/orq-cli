//! Every project shares one machine. `[runs]` in the user's `config.toml` limits how many runs
//! are active on it at once, across all projects, and how much free disk a run needs to start.
//! `orq run` refuses a start over either limit before it creates anything, exit 3.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orq_core::project_settings::{ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

const EXIT_LIMIT: i32 = 3;

const PROFILES: &str = "[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[roles]\nimplementer = \"opus\"\n";

/// A temporary home whose `config.toml` has the model profiles and `runs`, a `[runs]` table's body.
fn home(tag: &str, runs: Option<&str>) -> PathBuf {
    let home = orq_test_support::process_temp_dir().join(format!("orq-machine-run-limit-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    let runs = runs.map_or_else(String::new, |body| format!("\n[runs]\n{body}\n"));
    std::fs::write(home.join("config/orq/config.toml"), format!("{PROFILES}{runs}")).unwrap();
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

fn start_with(repo: &Path, home: &Path, feature: &str, extra: &[&str], env: &[(&str, &str)]) -> Output {
    let mut args = vec!["run", "--remote", "origin", "--target", "main", "--feature", feature];
    args.extend_from_slice(extra);
    orq(repo, home, &args, env)
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

fn record(repo: &Path, run_id: &str, feature: &str) -> RunRecord {
    RunRecord {
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
    }
}

/// A run of `feature` in the project at `repo`, started and not ended.
fn seed_active(home: &Path, repo: &Path, run_id: &str, feature: &str) {
    runs(home).start_run(&record(repo, run_id, feature)).unwrap();
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

#[test]
fn a_start_in_another_project_on_a_full_machine_exits_3_naming_the_machine_and_creates_nothing() {
    let a = repo();
    let b = repo();
    let home = home("full", Some("max_active = 1"));
    seed_active(&home, a.root(), "run-a", "COM-01");
    let name_a = project(a.root()).name;

    for extra in [&["--check"][..], &[][..], &["--detach"][..]] {
        let refused = start(b.root(), &home, "COM-01", extra);
        assert_eq!(refused.status.code(), Some(EXIT_LIMIT), "{extra:?}: {}", text(&refused.stderr));
        let expected = format!("this machine already has 1 active run ({name_a} run-a): wait for it to finish, or raise [runs] max_active");
        assert!(text(&refused.stderr).contains(&expected), "{extra:?}: {}", text(&refused.stderr));
        assert_eq!(text(&refused.stdout), "", "{extra:?}: no run id was printed");
    }
    assert_eq!(runs(&home).run_ids().unwrap(), vec!["run-a".to_string()], "no run was recorded");
    assert!(!b.root().join(".claude/worktrees").exists(), "no worktree was made");

    append(&home, a.root(), "run-a", "COM-01", "run.completed", serde_json::json!({"ending": "review_ready"}));
    let ready = start(b.root(), &home, "COM-01", &["--check"]);
    assert_eq!(ready.status.code(), Some(0), "{}", text(&ready.stderr));
}

#[test]
fn a_malformed_runs_value_is_refused_at_start_naming_the_file_and_the_key() {
    let repo = repo();
    for (tag, body, key) in [("zero", "max_active = 0", "max_active"), ("negative", "max_active = -2", "max_active"), ("text", "min_free_disk_gb = \"lots\"", "min_free_disk_gb")] {
        let home = home(&format!("malformed-{tag}"), Some(body));
        let refused = start(repo.root(), &home, "COM-01", &["--check"]);
        assert_ne!(refused.status.code(), Some(0), "{tag}: {}", text(&refused.stderr));
        let file = home.join("config/orq/config.toml");
        let expected = format!("{}: [runs] {key} must be empty or a whole number of at least 1", file.display());
        assert!(text(&refused.stderr).contains(&expected), "{tag}: {}", text(&refused.stderr));
        assert_eq!(text(&refused.stdout), "", "{tag}");
    }
}

#[test]
fn the_project_limit_still_applies_under_a_higher_machine_limit_and_without_runs_settings_runs_start_as_today() {
    let a = repo();
    let b = repo();
    let both = home("both", Some("max_active = 5"));
    save(&both, a.root(), &ProjectSettings { max_active_runs: Some(1), ..ProjectSettings::default() });
    seed_active(&both, a.root(), "run-a", "COM-01");
    let name_a = project(a.root()).name;

    let refused = start(a.root(), &both, "COM-02", &["--check"]);
    assert_eq!(refused.status.code(), Some(EXIT_LIMIT), "{}", text(&refused.stderr));
    assert!(text(&refused.stderr).contains(&format!("{name_a} already has 1 active run (run-a, COM-01)")), "{}", text(&refused.stderr));
    let other = start(b.root(), &both, "COM-01", &["--check"]);
    assert_eq!(other.status.code(), Some(0), "the machine has room: {}", text(&other.stderr));

    let unset = home("unset", None);
    seed_active(&unset, a.root(), "run-a", "COM-01");
    seed_active(&unset, b.root(), "run-b", "COM-01");
    let free = start(b.root(), &unset, "COM-02", &["--check"]);
    assert_eq!(free.status.code(), Some(0), "no [runs]: {}", text(&free.stderr));
    assert_eq!(text(&free.stdout), "state: ready\n");
}

#[test]
fn a_paused_run_counts_toward_the_machine_but_its_resume_is_not_refused_and_a_blocked_runs_resume_is() {
    let a = repo();
    let b = repo();
    let home = home("resume", Some("max_active = 1"));
    seed_active(&home, a.root(), "run-paused", "COM-01");
    append(&home, a.root(), "run-paused", "COM-01", "run.pause_requested", serde_json::json!({}));
    append(&home, a.root(), "run-paused", "COM-01", "run.paused", serde_json::json!({}));

    let other = start(b.root(), &home, "COM-01", &["--check"]);
    assert_eq!(other.status.code(), Some(EXIT_LIMIT), "a paused run is active: {}", text(&other.stderr));
    let resumed = start(a.root(), &home, "COM-01", &["--check", "--resume"]);
    assert_ne!(resumed.status.code(), Some(EXIT_LIMIT), "{}", text(&resumed.stderr));
    assert!(!text(&resumed.stderr).contains("[runs] max_active"), "{}", text(&resumed.stderr));

    seed_active(&home, b.root(), "run-blocked", "COM-02");
    append(&home, b.root(), "run-blocked", "COM-02", "run.completed", serde_json::json!({"ending": "blocked"}));
    runs(&home).finish_run(&orq_git::project_id(&project(b.root())), "COM-02", "run-blocked").unwrap();
    let blocked = start(b.root(), &home, "COM-02", &["--check", "--resume"]);
    assert_eq!(blocked.status.code(), Some(EXIT_LIMIT), "{}", text(&blocked.stderr));
    assert!(text(&blocked.stderr).contains("this machine already has 1 active run"), "{}", text(&blocked.stderr));
}

#[test]
fn the_machine_disk_floor_refuses_a_start_and_a_resume_naming_the_machine() {
    let repo = repo();
    let home = home("disk", Some("min_free_disk_gb = 15"));
    let low = [("ORQ_TEST_FREE_DISK_BYTES", "9800000000")];

    for extra in [&["--check"][..], &[][..], &["--detach"][..], &["--check", "--resume"][..]] {
        let refused = start_with(repo.root(), &home, "COM-01", extra, &low);
        assert_eq!(refused.status.code(), Some(EXIT_LIMIT), "{extra:?}: {}", text(&refused.stderr));
        let stderr = text(&refused.stderr);
        let needed = "(this machine needs 15 GB to start a run): free some space, or lower [runs] min_free_disk_gb";
        assert!(stderr.contains("only 9.8 GB free on ") && stderr.contains(needed), "{extra:?}: {stderr}");
        assert_eq!(text(&refused.stdout), "", "{extra:?}");
    }
    assert!(runs(&home).run_ids().unwrap_or_default().is_empty(), "no run was recorded");

    // A paused run's resume is exempt from the count, not from the floor.
    seed_active(&home, repo.root(), "run-paused", "COM-02");
    append(&home, repo.root(), "run-paused", "COM-02", "run.pause_requested", serde_json::json!({}));
    append(&home, repo.root(), "run-paused", "COM-02", "run.paused", serde_json::json!({}));
    runs(&home).finish_run(&orq_git::project_id(&project(repo.root())), "COM-02", "run-paused").unwrap();
    let paused = start_with(repo.root(), &home, "COM-02", &["--check", "--resume"], &low);
    assert_eq!(paused.status.code(), Some(EXIT_LIMIT), "{}", text(&paused.stderr));

    let enough = start_with(repo.root(), &home, "COM-01", &["--check"], &[("ORQ_TEST_FREE_DISK_BYTES", "15000000000")]);
    assert_eq!(enough.status.code(), Some(0), "{}", text(&enough.stderr));
}
