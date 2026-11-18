//! A project's own settings live in orq.db on this machine, keyed by the path of the project's main
//! checkout: an optional implementer and reviewer (harness, model, effort) and what a run does when it
//! finishes. Each save names the revision it was based on, so a save made from a stale read is
//! refused rather than overwriting a newer one.

use std::path::Path;

use orq_core::project_settings::{Delivery, ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> (Store, std::path::PathBuf) {
    let base = orq_test_support::process_temp_dir().join(format!("orq-project-settings-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let db = roots.database();
    (Store::open(&db).unwrap(), db)
}

fn agent(harness: &str, model: &str, effort: Option<&str>) -> ProjectAgent {
    ProjectAgent { harness: harness.to_string(), model: model.to_string(), effort: effort.map(str::to_string) }
}

#[test]
fn a_project_s_settings_are_saved_per_project_and_a_stale_save_is_refused() {
    let (mut store, db) = open("round-trip");
    let alpha = Path::new("/work/alpha");
    let beta = Path::new("/work/beta");

    let fresh = store.project_settings(alpha).unwrap();
    assert_eq!(fresh.settings, ProjectSettings::default(), "a project with no record has no settings of its own");
    assert_eq!(fresh.settings.delivery, Delivery::Review);
    assert_eq!(fresh.revision, 0);

    let chosen = ProjectSettings {
        implementer: Some(agent("claude", "sonnet", Some("high"))),
        reviewer: None,
        delivery: Delivery::MergeLocal,
        ..ProjectSettings::default()
    };
    assert_eq!(store.save_project_settings(alpha, 0, &chosen).unwrap(), SaveOutcome::Saved { revision: 1 });
    drop(store);

    let mut store = Store::open(&db).unwrap();
    let read = store.project_settings(alpha).unwrap();
    assert_eq!((read.settings.clone(), read.revision), (chosen.clone(), 1), "kept across reopening");
    assert_eq!(store.project_settings(beta).unwrap().revision, 0, "another project is untouched");

    let other = ProjectSettings::default();
    assert_eq!(store.save_project_settings(alpha, 0, &other).unwrap(), SaveOutcome::Conflict, "based on revision 0, but it is at 1");
    assert_eq!(store.project_settings(alpha).unwrap().settings, chosen, "a refused save writes nothing");

    assert_eq!(store.save_project_settings(alpha, 1, &other).unwrap(), SaveOutcome::Saved { revision: 2 });
    let cleared = store.project_settings(alpha).unwrap();
    assert_eq!(cleared.settings.implementer, None, "a role saved as unset falls back again");
    assert_eq!(cleared.settings.delivery, Delivery::Review);
}

#[test]
fn a_blank_or_multi_line_value_is_refused_and_nothing_is_written() {
    let (mut store, _) = open("invalid");
    let root = Path::new("/work/alpha");
    for bad in [agent(" ", "sonnet", None), agent("claude", "son\nnet", None), agent("claude", "sonnet", Some(""))] {
        let settings = ProjectSettings { implementer: Some(bad.clone()), ..ProjectSettings::default() };
        assert!(store.save_project_settings(root, 0, &settings).is_err(), "{bad:?}");
    }
    assert_eq!(store.project_settings(root).unwrap().revision, 0);
    assert_eq!(Delivery::parse("merge_local"), Some(Delivery::MergeLocal));
    assert_eq!(Delivery::MergeLocal.as_str(), "merge_local");
    assert_eq!(Delivery::parse("ship-it"), None);
}

#[test]
fn the_removed_push_delivery_is_never_saved_but_a_record_that_names_it_still_reads() {
    let (mut store, db) = open("push");
    let root = Path::new("/work/alpha");
    let push = ProjectSettings { delivery: Delivery::Push, ..ProjectSettings::default() };
    let refused = store.save_project_settings(root, 0, &push).unwrap_err();
    assert_eq!(refused.to_string(), orq_core::project_settings::PUSH_REMOVED);
    assert_eq!(store.project_settings(root).unwrap().revision, 0, "nothing was written");

    store.save_project_settings(root, 0, &ProjectSettings::default()).unwrap();
    rusqlite::Connection::open(&db).unwrap().execute("UPDATE project_settings SET delivery = 'push'", []).unwrap();
    assert_eq!(store.project_settings(root).unwrap().settings.delivery, Delivery::Push, "saved before push was removed");
}

#[test]
fn the_same_checkout_spelled_differently_shares_one_record() {
    let (mut store, _) = open("aliases");
    let chosen = ProjectSettings { delivery: Delivery::MergeLocal, ..ProjectSettings::default() };
    assert_eq!(store.save_project_settings(Path::new("/work/alpha/."), 0, &chosen).unwrap(), SaveOutcome::Saved { revision: 1 });
    for spelling in ["/work/alpha", "/work/alpha/", "/work//alpha/./"] {
        let read = store.project_settings(Path::new(spelling)).unwrap();
        assert_eq!((read.settings.delivery, read.revision), (Delivery::MergeLocal, 1), "{spelling}");
    }
    assert_eq!(store.save_project_settings(Path::new("/work/alpha/"), 0, &chosen).unwrap(), SaveOutcome::Conflict, "one revision history");
    assert!(store.project_settings(Path::new("/work/beta/../alpha")).is_err(), "a parent step could name another checkout, so it is refused");
    assert!(store.project_settings(Path::new("work/alpha")).is_err(), "a relative path names no checkout");
}

#[test]
fn editors_racing_from_the_same_read_save_once_and_the_rest_conflict() {
    let (store, db) = open("race");
    drop(store);
    let editors = 8;
    let start = std::sync::Arc::new(std::sync::Barrier::new(editors));
    let outcomes: Vec<SaveOutcome> = (0..editors)
        .map(|editor| {
            let (db, start) = (db.clone(), start.clone());
            std::thread::spawn(move || {
                let mut store = Store::open(&db).unwrap();
                let read = store.project_settings(Path::new("/work/alpha")).unwrap();
                let model = format!("model-{editor}");
                let settings = ProjectSettings { implementer: Some(agent("claude", &model, None)), ..ProjectSettings::default() };
                start.wait();
                store.save_project_settings(Path::new("/work/alpha"), read.revision, &settings).unwrap()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|editor| editor.join().unwrap())
        .collect();
    let saved = outcomes.iter().filter(|outcome| **outcome == SaveOutcome::Saved { revision: 1 }).count();
    assert_eq!(saved, 1, "{outcomes:?}");
    assert_eq!(outcomes.iter().filter(|outcome| **outcome == SaveOutcome::Conflict).count(), editors - 1);
    assert_eq!(Store::open(&db).unwrap().project_settings(Path::new("/work/alpha")).unwrap().revision, 1);
}

#[test]
fn a_read_only_lookup_writes_nothing_and_reads_an_older_database_as_having_no_settings() {
    let (mut store, db) = open("read-only");
    let chosen = ProjectSettings { delivery: Delivery::MergeLocal, ..ProjectSettings::default() };
    store.save_project_settings(Path::new("/work/alpha"), 0, &chosen).unwrap();
    drop(store);

    let read = Store::read_project_settings(&db, Path::new("/work/alpha")).unwrap();
    assert_eq!((read.settings, read.revision), (chosen, 1));

    let version = |db: &Path| -> u32 { rusqlite::Connection::open(db).unwrap().pragma_query_value(None, "user_version", |row| row.get(0)).unwrap() };
    // Schema version 6 added project_settings; later versions keep it.
    let before_settings = 5;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE project_settings RENAME TO project_settings_old;").unwrap();
    conn.pragma_update(None, "user_version", before_settings).unwrap();
    drop(conn);
    let older = Store::read_project_settings(&db, Path::new("/work/alpha")).unwrap();
    assert_eq!((older.settings, older.revision), (ProjectSettings::default(), 0));
    assert_eq!(version(&db), before_settings, "left as it was");

    rusqlite::Connection::open(&db).unwrap().pragma_update(None, "user_version", Store::LATEST_VERSION + 1).unwrap();
    assert!(Store::read_project_settings(&db, Path::new("/work/alpha")).is_err(), "a newer schema is not guessed at");
}

#[test]
fn the_run_limit_and_the_disk_floor_are_saved_and_out_of_range_values_are_refused() {
    let (mut store, db) = open("limits");
    let alpha = Path::new("/work/alpha");
    assert_eq!((ProjectSettings::default().max_active_runs, ProjectSettings::default().min_free_disk_gb), (None, None), "no limit unless set");

    let limited = ProjectSettings { max_active_runs: Some(1), min_free_disk_gb: Some(15), ..ProjectSettings::default() };
    assert_eq!(store.save_project_settings(alpha, 0, &limited).unwrap(), SaveOutcome::Saved { revision: 1 });
    assert_eq!(store.project_settings(alpha).unwrap().settings, limited);
    assert_eq!(Store::read_project_settings(&db, alpha).unwrap().settings, limited, "read without opening for writing too");

    for bad in [
        ProjectSettings { max_active_runs: Some(0), ..ProjectSettings::default() },
        ProjectSettings { max_active_runs: Some(9), ..ProjectSettings::default() },
        ProjectSettings { min_free_disk_gb: Some(0), ..ProjectSettings::default() },
        ProjectSettings { min_free_disk_gb: Some(501), ..ProjectSettings::default() },
    ] {
        let err = store.save_project_settings(alpha, 1, &bad).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
    }
    assert_eq!(store.project_settings(alpha).unwrap().settings, limited, "a refused save writes nothing");

    let edges = ProjectSettings { max_active_runs: Some(8), min_free_disk_gb: Some(500), ..ProjectSettings::default() };
    assert_eq!(store.save_project_settings(alpha, 1, &edges).unwrap(), SaveOutcome::Saved { revision: 2 });
    let cleared = ProjectSettings::default();
    assert_eq!(store.save_project_settings(alpha, 2, &cleared).unwrap(), SaveOutcome::Saved { revision: 3 });
    assert_eq!(store.project_settings(alpha).unwrap().settings, cleared, "both cleared back to no limit");
}

#[test]
fn a_database_from_before_the_limits_reads_its_settings_with_no_limits() {
    let (mut store, db) = open("pre-limits");
    let alpha = Path::new("/work/alpha");
    let chosen = ProjectSettings { delivery: Delivery::MergeLocal, ..ProjectSettings::default() };
    store.save_project_settings(alpha, 0, &chosen).unwrap();
    drop(store);
    // Schema version 7 had project_settings without the two limit columns.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE project_settings DROP COLUMN max_active_runs; ALTER TABLE project_settings DROP COLUMN min_free_disk_gb;").unwrap();
    conn.pragma_update(None, "user_version", 7).unwrap();
    drop(conn);
    let read = Store::read_project_settings(&db, alpha).unwrap();
    assert_eq!((read.settings, read.revision), (chosen, 1));
}

#[test]
fn waiting_for_each_run_s_review_before_the_next_is_on_by_default_and_saved_off() {
    let (mut store, db) = open("wait-for-review");
    let alpha = Path::new("/work/alpha");
    assert!(ProjectSettings::default().queue_wait_for_review, "on by default, for every delivery");
    assert!(store.project_settings(alpha).unwrap().settings.queue_wait_for_review);

    let off = ProjectSettings { delivery: Delivery::MergeLocal, queue_wait_for_review: false, ..ProjectSettings::default() };
    store.save_project_settings(alpha, 0, &off).unwrap();
    assert!(!store.project_settings(alpha).unwrap().settings.queue_wait_for_review);
    store.save_project_settings(Path::new("/work/beta"), 0, &ProjectSettings::default()).unwrap();
    drop(store);

    // Schema version 9 had no such column: a project saved then waits, as the default does.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE project_settings DROP COLUMN queue_wait_for_review;").unwrap();
    conn.pragma_update(None, "user_version", 9).unwrap();
    drop(conn);
    assert!(Store::read_project_settings(&db, Path::new("/work/beta")).unwrap().settings.queue_wait_for_review);
}
