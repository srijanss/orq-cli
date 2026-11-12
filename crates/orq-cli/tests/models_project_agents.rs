//! A project's own agents, saved in this machine's orq.db, come before every configured fallback:
//! the implementer before `[roles]` and the machine default, the reviewer before `.tdd-config.json`
//! `reviewers.default`. `reviewers.allowedModels` still applies to them. `orq models` prints the
//! defaults a run would use, so it shows the resolution `orq run` makes.

use std::path::Path;
use std::process::{Command, Output};

use orq_core::project_settings::{ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_test_support::GitFixture;

const MACHINE: &str = r#"
[models.opus]
harness = "claude"
model = "opus"

[roles]
implementer = "opus"

[reviewer]
harness = "pi"
model = "vendor/machine"
"#;

fn home(tag: &str) -> std::path::PathBuf {
    let home = orq_test_support::process_temp_dir().join(format!("orq-project-agents-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config/orq")).unwrap();
    std::fs::write(home.join("config/orq/config.toml"), MACHINE).unwrap();
    home
}

fn roots(home: &Path) -> StorageRoots {
    let base = home.to_path_buf();
    StorageRoots::resolve(&|name| match name {
        "XDG_DATA_HOME" => Some(base.join("data").display().to_string()),
        "XDG_CONFIG_HOME" => Some(base.join("config").display().to_string()),
        "XDG_STATE_HOME" => Some(base.join("state").display().to_string()),
        "XDG_CACHE_HOME" => Some(base.join("cache").display().to_string()),
        _ => None,
    })
    .unwrap()
}

fn save(home: &Path, repo: &Path, settings: &ProjectSettings) {
    let roots = roots(home);
    roots.create_all().unwrap();
    let mut store = Store::open(&roots.database()).unwrap();
    let root = std::fs::canonicalize(repo).unwrap();
    let revision = store.project_settings(&root).unwrap().revision;
    assert!(matches!(store.save_project_settings(&root, revision, settings).unwrap(), SaveOutcome::Saved { .. }));
}

fn models(repo: &Path, home: &Path) -> serde_json::Value {
    let output: Output = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["models", "--repo"])
        .arg(repo)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

fn agent(harness: &str, model: &str, effort: Option<&str>) -> ProjectAgent {
    ProjectAgent { harness: harness.to_string(), model: model.to_string(), effort: effort.map(str::to_string) }
}

#[test]
fn a_project_s_own_agents_come_before_roles_tdd_config_and_the_machine_defaults() {
    let repo = GitFixture::new();
    std::fs::write(repo.root().join(".tdd-config.json"), r#"{"reviewers":{"default":{"reviewer":"pi","model":"vendor/tdd"}}}"#).unwrap();
    let home = home("chosen");

    let before = models(repo.root(), &home);
    assert_eq!(before["defaults"]["implementer"]["model"], "opus", "no record: [roles] applies");
    assert_eq!(before["defaults"]["reviewer"]["model"], "vendor/tdd", "no record: .tdd-config.json applies");

    let chosen = ProjectSettings {
        implementer: Some(agent("claude", "sonnet", Some("low"))),
        reviewer: Some(agent("pi", "vendor/project", Some("high"))),
        ..ProjectSettings::default()
    };
    save(&home, repo.root(), &chosen);
    let after = models(repo.root(), &home);
    assert_eq!(after["defaults"]["implementer"], serde_json::json!({"harness": "claude", "model": "sonnet", "thinking": "low"}));
    assert_eq!(after["defaults"]["reviewer"], serde_json::json!({"harness": "pi", "model": "vendor/project", "thinking": "high"}));

    let worktree = repo.add_linked_worktree("side");
    assert_eq!(models(&worktree, &home)["defaults"], after["defaults"], "a linked worktree is the same project");
}

#[test]
fn a_project_reviewer_outside_allowed_models_is_refused() {
    let repo = GitFixture::new();
    std::fs::write(repo.root().join(".tdd-config.json"), r#"{"reviewers":{"allowedModels":["vendor/machine"]}}"#).unwrap();
    let home = home("allowed");
    save(&home, repo.root(), &ProjectSettings { reviewer: Some(agent("pi", "vendor/project", None)), ..ProjectSettings::default() });
    let json = models(repo.root(), &home);
    let error = json["defaultsError"].as_str().unwrap_or_default();
    assert!(error.contains("vendor/project"), "{json}");
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap();
}

fn schema_version(database: &Path) -> u32 {
    let conn = rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap()
}

#[test]
fn looking_up_a_project_s_agents_never_writes_the_database() {
    let repo = GitFixture::new();
    let home = home("read-only");
    save(&home, repo.root(), &ProjectSettings { implementer: Some(agent("claude", "sonnet", None)), ..ProjectSettings::default() });
    let roots = roots(&home);
    let database = roots.database();
    let current = schema_version(&database);

    set_mode(&database, 0o400);
    set_mode(&roots.data, 0o500);
    let read = models(repo.root(), &home);
    set_mode(&roots.data, 0o700);
    // SQLite gives an empty -wal the database's mode when it opens it, so restore every file.
    for file in ["orq.db", "orq.db-wal", "orq.db-shm"].map(|name| roots.data.join(name)).iter().filter(|file| file.exists()) {
        set_mode(file, 0o600);
    }
    assert_eq!(read["defaults"]["implementer"]["model"], "sonnet", "read from a database nobody may write: {read}");

    // A database from before project settings existed has none, and is left at its version.
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch("ALTER TABLE project_settings RENAME TO project_settings_old;").unwrap();
    conn.pragma_update(None, "user_version", current - 1).unwrap();
    drop(conn);
    let older = models(repo.root(), &home);
    assert_eq!(older["defaults"]["implementer"]["model"], "opus", "{older}");
    assert_eq!(schema_version(&database), current - 1, "not migrated by a lookup");
}
