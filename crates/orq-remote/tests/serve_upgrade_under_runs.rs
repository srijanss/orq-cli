//! orq serve does not apply a breaking orq.db migration under runs whose Orq could no longer use it, unless told to.

mod web_deps;

use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use serde_json::{json, Value};

fn roots(label: &str) -> StorageRoots {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

/// A database an Orq that understands up to `version` left behind.
fn database_at(roots: &StorageRoots, version: u32) {
    drop(Store::open_understanding(&roots.database(), version).unwrap());
}

fn version(roots: &StorageRoots) -> u32 {
    rusqlite::Connection::open(roots.database()).unwrap().pragma_query_value(None, "user_version", |row| row.get(0)).unwrap()
}

fn event(store: &RunStore, run_id: &str, kind: &str, payload: Value) {
    let n = store.events(run_id).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{run_id}-{n}"),
            run_id: run_id.into(),
            project_id: "alpha-1".into(),
            feature_id: format!("feat-{run_id}"),
            kind: kind.into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-18T05:44:00Z".into(),
            payload,
        })
        .unwrap();
}

/// A running run launched by an Orq that understands schema `schema`; `None` for one launched before runs said.
fn running_run(roots: &StorageRoots, run_id: &str, schema: Option<u32>) -> RunStore {
    let store = RunStore::new(&roots.state);
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: run_id.into(),
            project_id: "alpha-1".into(),
            feature_id: format!("feat-{run_id}"),
            remote: "origin".into(),
            provider: "gitlab".into(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: format!("orq/{run_id}"),
            worktree: roots.state.join("worktrees").join(run_id),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();
    let mut launched = json!({"pid": 1});
    if let Some(schema) = schema {
        launched["schema"] = json!(schema);
    }
    event(&store, run_id, "run.launched", launched);
    event(&store, run_id, "stage.started", json!({}));
    store
}

#[test]
fn a_launched_run_records_the_schema_its_orq_understands() {
    let roots = roots("upgrade-launch-schema");
    let store = running_run(&roots, "run-new", None);
    orq_orchestrator::record_launch(&store, "run-new", 4242, &roots.state.join("run.log")).unwrap();
    let launched = store.events("run-new").unwrap().into_iter().rev().find(|e| e.kind == "run.launched").unwrap();
    assert_eq!(launched.payload["schema"], json!(Store::LATEST_VERSION), "{}", launched.payload);
}

#[test]
fn serve_refuses_a_breaking_upgrade_while_runs_of_an_older_orq_are_active_and_names_them() {
    let roots = roots("upgrade-refused");
    // Version 7 drops what Orq at version 6 reads, so the upgrade to the latest schema is breaking for it.
    database_at(&roots, 6);
    running_run(&roots, "run-1794980659-2086195-1", Some(6));
    running_run(&roots, "run-unsaid", None);
    running_run(&roots, "run-current", Some(Store::LATEST_VERSION));

    let refused = orq_remote::projects::check_upgrade(&roots, false).expect_err("the breaking upgrade waits");
    assert!(refused.contains("run-1794980659-2086195-1"), "{refused}");
    assert!(refused.contains("run-unsaid"), "a run launched before runs said their schema has the file's: {refused}");
    assert!(!refused.contains("run-current"), "a run of this Orq can use the upgraded database: {refused}");
    assert!(refused.contains("--upgrade-now"), "{refused}");
    assert_eq!(version(&roots), 6, "nothing was migrated");
}

#[test]
fn upgrade_now_migrates_and_records_which_runs_it_upgraded_under() {
    let roots = roots("upgrade-now");
    database_at(&roots, 6);
    let store = running_run(&roots, "run-old", Some(6));

    orq_remote::projects::check_upgrade(&roots, true).unwrap();
    assert_eq!(version(&roots), Store::LATEST_VERSION);
    let recorded = store.events("run-old").unwrap().into_iter().find(|e| e.kind == "store.upgraded_under_runs").expect("the upgrade is recorded on the run");
    assert_eq!(recorded.payload["run_ids"], json!(["run-old"]), "{}", recorded.payload);
    assert_eq!(recorded.payload["to"], json!(Store::LATEST_VERSION), "{}", recorded.payload);
}

#[test]
fn serve_starts_when_the_pending_migrations_only_add() {
    let roots = roots("upgrade-additive");
    // From version 7 on, every migration only adds: an Orq at 7 keeps using the upgraded database.
    database_at(&roots, 7);
    let store = running_run(&roots, "run-old", Some(7));

    orq_remote::projects::check_upgrade(&roots, false).unwrap();
    assert!(store.events("run-old").unwrap().iter().all(|e| e.kind != "store.upgraded_under_runs"));
}

#[test]
fn serve_starts_with_no_database_yet() {
    let roots = roots("upgrade-no-database");
    running_run(&roots, "run-old", Some(6));
    orq_remote::projects::check_upgrade(&roots, false).unwrap();
}
