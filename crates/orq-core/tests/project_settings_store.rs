//! A project's own settings live in orq.db on this machine, keyed by the path of the project's main
//! checkout: an optional implementer and reviewer (harness, model, effort) and what a run does when it
//! finishes. Each save names the revision it was based on, so a save made from a stale read is
//! refused rather than overwriting a newer one.

use std::path::Path;

use orq_core::project_settings::{Delivery, ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn open(tag: &str) -> (Store, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("orq-project-settings-{}-{tag}", std::process::id()));
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
    };
    assert_eq!(store.save_project_settings(alpha, 0, &chosen).unwrap(), SaveOutcome::Saved { revision: 1 });
    drop(store);

    let mut store = Store::open(&db).unwrap();
    let read = store.project_settings(alpha).unwrap();
    assert_eq!((read.settings.clone(), read.revision), (chosen.clone(), 1), "kept across reopening");
    assert_eq!(store.project_settings(beta).unwrap().revision, 0, "another project is untouched");

    let other = ProjectSettings { delivery: Delivery::Push, ..ProjectSettings::default() };
    assert_eq!(store.save_project_settings(alpha, 0, &other).unwrap(), SaveOutcome::Conflict, "based on revision 0, but it is at 1");
    assert_eq!(store.project_settings(alpha).unwrap().settings, chosen, "a refused save writes nothing");

    assert_eq!(store.save_project_settings(alpha, 1, &other).unwrap(), SaveOutcome::Saved { revision: 2 });
    let cleared = store.project_settings(alpha).unwrap();
    assert_eq!(cleared.settings.implementer, None, "a role saved as unset falls back again");
    assert_eq!(cleared.settings.delivery, Delivery::Push);
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
fn the_same_checkout_spelled_differently_shares_one_record() {
    let (mut store, _) = open("aliases");
    let chosen = ProjectSettings { delivery: Delivery::Push, ..ProjectSettings::default() };
    assert_eq!(store.save_project_settings(Path::new("/work/alpha/."), 0, &chosen).unwrap(), SaveOutcome::Saved { revision: 1 });
    for spelling in ["/work/alpha", "/work/alpha/", "/work//alpha/./"] {
        let read = store.project_settings(Path::new(spelling)).unwrap();
        assert_eq!((read.settings.delivery, read.revision), (Delivery::Push, 1), "{spelling}");
    }
    assert_eq!(store.save_project_settings(Path::new("/work/alpha/"), 0, &chosen).unwrap(), SaveOutcome::Conflict, "one revision history");
    assert!(store.project_settings(Path::new("/work/beta/../alpha")).is_err(), "a parent step could name another checkout, so it is refused");
    assert!(store.project_settings(Path::new("work/alpha")).is_err(), "a relative path names no checkout");
}
