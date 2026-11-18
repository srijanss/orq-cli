//! A project's run queue lives in orq.db, keyed by the project's main checkout like its settings:
//! each entry is a feature with when and from where it was added and the launch profiles it starts
//! with, in the order added until reordered, plus the run it started and whether that run failed.
//! The queue can be paused with a reason and resumed.

use std::path::Path;

use orq_core::storage::StorageRoots;
use orq_core::store::run_queue::{QueueAddition, QueueProfiles};
use orq_core::store::Store;

fn open(tag: &str) -> Store {
    Store::open(&database(tag)).unwrap()
}

/// A fresh orq.db's path for test `tag`.
fn database(tag: &str) -> std::path::PathBuf {
    let base = orq_test_support::process_temp_dir().join(format!("orq-run-queue-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots.database()
}

fn plain(feature: &str) -> QueueAddition {
    QueueAddition { feature: feature.to_string(), profiles: None }
}

fn features(store: &Store, root: &Path) -> Vec<String> {
    store.queue(root).unwrap().entries.into_iter().map(|e| e.feature).collect()
}

#[test]
fn a_queue_keeps_its_entries_per_project_in_the_order_added_and_skips_a_feature_already_queued() {
    let store = open("add");
    let alpha = Path::new("/work/alpha");
    let beta = Path::new("/work/beta");
    assert!(store.queue(alpha).unwrap().entries.is_empty());

    let added = store.queue_add(alpha, &[plain("A"), plain("B")], "web", 100).unwrap();
    assert_eq!(added, ["A", "B"]);
    let profiles = QueueProfiles { implementer: "claude-opus".into(), reviewer: "pi-gpt".into() };
    let added = store.queue_add(alpha, &[plain("B"), QueueAddition { feature: "C".into(), profiles: Some(profiles.clone()) }], "cli", 200).unwrap();
    assert_eq!(added, ["C"], "B is queued already");
    store.queue_add(beta, &[plain("Z")], "web", 300).unwrap();

    let queue = store.queue(alpha).unwrap();
    assert_eq!(queue.entries.iter().map(|e| e.feature.as_str()).collect::<Vec<_>>(), ["A", "B", "C"]);
    let c = &queue.entries[2];
    assert_eq!((c.added_at, c.added_by.as_str(), c.profiles.as_ref()), (200, "cli", Some(&profiles)));
    assert_eq!((c.run.as_deref(), c.failed), (None, false));
    assert_eq!(queue.paused, None);
    assert_eq!(features(&store, beta), ["Z"]);
    assert_eq!(store.queued_projects().unwrap(), ["/work/alpha", "/work/beta"]);
}

#[test]
fn entries_are_reordered_removed_and_marked_with_their_run_and_its_failure() {
    let store = open("change");
    let alpha = Path::new("/work/alpha");
    store.queue_add(alpha, &[plain("A"), plain("B"), plain("C")], "web", 1).unwrap();

    store.queue_reorder(alpha, &["C".to_string(), "A".to_string(), "B".to_string()]).unwrap();
    assert_eq!(features(&store, alpha), ["C", "A", "B"]);
    assert!(store.queue_reorder(alpha, &["C".to_string(), "A".to_string()]).is_err(), "an order must name every entry once");
    assert!(store.queue_reorder(alpha, &["C".to_string(), "A".to_string(), "A".to_string()]).is_err());

    store.queue_started(alpha, "A", "run-1").unwrap();
    store.queue_failed(alpha, "A").unwrap();
    let a = store.queue(alpha).unwrap().entries.into_iter().find(|e| e.feature == "A").unwrap();
    assert_eq!((a.run.as_deref(), a.failed), (Some("run-1"), true));

    assert!(store.queue_remove(alpha, "A").unwrap());
    assert!(!store.queue_remove(alpha, "A").unwrap(), "removing an entry that is gone says so");
    assert_eq!(features(&store, alpha), ["C", "B"]);
    // A feature queued again after removal goes to the end.
    store.queue_add(alpha, &[plain("A")], "web", 2).unwrap();
    assert_eq!(features(&store, alpha), ["C", "B", "A"]);
}

#[test]
fn a_queue_is_paused_with_its_reason_and_resumed() {
    let store = open("pause");
    let alpha = Path::new("/work/alpha");
    store.queue_pause(alpha, "free disk is below the floor").unwrap();
    assert_eq!(store.queue(alpha).unwrap().paused.as_deref(), Some("free disk is below the floor"));
    store.queue_resume(alpha).unwrap();
    assert_eq!(store.queue(alpha).unwrap().paused, None);
    assert!(store.queue(Path::new("relative")).is_err(), "a project is keyed by an absolute path");
}

#[test]
fn a_project_s_queue_hours_and_stop_on_failure_are_saved_with_its_settings_and_bad_hours_are_refused() {
    use orq_core::project_settings::{within_queue_hours, ProjectSettings, SaveOutcome};
    let mut store = open("settings");
    let alpha = Path::new("/work/alpha");
    let fresh = store.project_settings(alpha).unwrap().settings;
    assert_eq!((fresh.queue_hours, fresh.queue_stop_on_failure), (None, false), "no window and stop-on-failure off by default");

    let chosen = ProjectSettings { queue_hours: Some("22:00-07:00".into()), queue_stop_on_failure: true, ..ProjectSettings::default() };
    assert_eq!(store.save_project_settings(alpha, 0, &chosen).unwrap(), SaveOutcome::Saved { revision: 1 });
    assert_eq!(store.project_settings(alpha).unwrap().settings, chosen);

    for bad in ["22:00", "24:00-07:00", "22:60-07:00", "7-8", " 22:00-07:00", "22:00-22:00"] {
        let settings = ProjectSettings { queue_hours: Some(bad.into()), ..ProjectSettings::default() };
        assert!(store.save_project_settings(alpha, 1, &settings).is_err(), "{bad:?} is refused");
    }

    // A window that wraps midnight holds the late evening and the early morning, not the day.
    let night = "22:00-07:00";
    assert!(within_queue_hours(night, 23 * 60));
    assert!(within_queue_hours(night, 6 * 60 + 59));
    assert!(!within_queue_hours(night, 7 * 60), "the end is outside");
    assert!(!within_queue_hours(night, 12 * 60));
    assert!(within_queue_hours("09:00-17:30", 9 * 60));
    assert!(!within_queue_hours("09:00-17:30", 17 * 60 + 30));
}

#[test]
fn the_queue_limit_is_checked_in_the_same_write_as_the_addition_so_two_adders_cannot_pass_it_together() {
    let database = database("limit");
    let store = Store::open(&database).unwrap();
    let alpha = Path::new("/work/alpha");
    let first: Vec<QueueAddition> = (1..orq_core::store::run_queue::QUEUE_LIMIT).map(|i| plain(&format!("F{i:03}"))).collect();
    store.queue_add(alpha, &first, "web", 1).unwrap();

    let refused = store.queue_add(alpha, &[plain("X"), plain("Y")], "web", 2).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(refused.to_string(), "the queue holds at most 200 features; 199 are queued");
    assert_eq!(store.queue_add(alpha, &[plain("F001")], "web", 2).unwrap(), Vec::<String>::new(), "one already queued fits");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let adders: Vec<_> = ["X", "Y"]
        .map(|feature| {
            let (database, barrier) = (database.clone(), barrier.clone());
            std::thread::spawn(move || {
                let store = Store::open(&database).unwrap();
                barrier.wait();
                store.queue_add(Path::new("/work/alpha"), &[plain(feature)], "cli", 3)
            })
        })
        .into_iter()
        .collect();
    let (added, refused): (Vec<_>, Vec<_>) = adders.into_iter().map(|adder| adder.join().unwrap()).partition(|result| result.is_ok());
    assert_eq!(added.len(), 1, "exactly one adder gets in: {added:?} {refused:?}");
    let refused = refused.into_iter().next().unwrap().unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput, "the other is refused by the limit, not a lock: {refused}");
    assert_eq!(refused.to_string(), "the queue holds at most 200 features; 200 are queued");
    assert_eq!(store.queue(alpha).unwrap().entries.len(), 200);
}
