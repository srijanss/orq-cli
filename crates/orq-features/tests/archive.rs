use std::fs;
use std::path::{Path, PathBuf};

use orq_features::archive::{archive_ledger, archive_path, read_archive, ArchiveError};
use serde_json::{json, Value};

fn dir(name: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-archive-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn entry(name: &str, status: &str, depends_on: &[&str]) -> Value {
    json!({"featureName": name, "status": status, "dependsOn": depends_on, "extra": "kept"})
}

fn write_ledger(dir: &Path, entries: &[Value]) -> PathBuf {
    let path = dir.join(".tdd-features.json");
    fs::write(&path, serde_json::to_string_pretty(&Value::Array(entries.to_vec())).unwrap()).unwrap();
    path
}

fn ledger_names(path: &Path) -> Vec<String> {
    let value: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    value.as_array().unwrap().iter().map(|e| e["featureName"].as_str().unwrap().to_string()).collect()
}

/// c1..c5 completed, p1 and p2 pending; p1 depends on c1 and c5, p2 on c2 and p1.
fn sample() -> Vec<Value> {
    vec![
        entry("c1", "completed", &[]),
        entry("c2", "completed", &["c1"]),
        entry("p1", "pending", &["c1", "c5"]),
        entry("c3", "completed", &[]),
        entry("c4", "completed", &[]),
        entry("p2", "pending", &["c2", "p1"]),
        entry("c5", "completed", &[]),
    ]
}

#[test]
fn archive_moves_older_completed_entries_and_strips_their_names_from_depends_on() {
    let dir = dir("moves");
    let data = dir.join("data");
    let ledger = write_ledger(&dir, &sample());

    let plan = archive_ledger(&ledger, &data, "proj", 2, false, 0, "2026-11-20T00:00:00Z").unwrap();

    assert_eq!(plan.archived, vec!["c1", "c2", "c3"]);
    // The two newest completed (c4, c5) stay, with every pending entry, in order.
    assert_eq!(ledger_names(&ledger), vec!["p1", "c4", "p2", "c5"]);
    let remaining: Value = serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
    assert_eq!(remaining[0]["dependsOn"], json!(["c5"]));
    assert_eq!(remaining[2]["dependsOn"], json!(["p1"]));
    assert_eq!(remaining[0]["extra"], "kept");
    // The archive holds the moved entries with archivedAt, and the original lists of the stripped ones.
    let archived = read_archive(&archive_path(&data, "proj")).unwrap();
    let names: Vec<&str> = archived.iter().map(|e| e["featureName"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["c1", "c2", "c3", "p1", "p2"]);
    assert_eq!(archived[0]["archivedAt"], "2026-11-20T00:00:00Z");
    let p1 = &archived[3];
    assert_eq!(p1["archivedDependsOn"], json!(["c1"]));
    assert!(p1.get("status").is_none());
    assert_eq!(archived[4]["archivedDependsOn"], json!(["c2"]));
}

#[test]
fn dry_run_reports_the_plan_and_writes_nothing() {
    let dir = dir("dry");
    let data = dir.join("data");
    let ledger = write_ledger(&dir, &sample());
    let before = fs::read_to_string(&ledger).unwrap();

    let plan = archive_ledger(&ledger, &data, "proj", 2, true, 0, "t").unwrap();

    assert_eq!(plan.archived, vec!["c1", "c2", "c3"]);
    assert_eq!(plan.stripped, vec![("p1".to_string(), vec!["c1".to_string()]), ("p2".to_string(), vec!["c2".to_string()])]);
    assert_eq!(fs::read_to_string(&ledger).unwrap(), before);
    assert!(!data.exists());
}

#[test]
fn a_second_archive_appends_to_the_first() {
    let dir = dir("append");
    let data = dir.join("data");
    let ledger = write_ledger(&dir, &sample());
    archive_ledger(&ledger, &data, "proj", 3, false, 0, "t1").unwrap();
    let first = read_archive(&archive_path(&data, "proj")).unwrap().len();
    archive_ledger(&ledger, &data, "proj", 1, false, 0, "t2").unwrap();

    let archived = read_archive(&archive_path(&data, "proj")).unwrap();
    assert!(archived.len() > first);
    assert_eq!(archived[0]["featureName"], "c1");
    assert_eq!(archived[0]["archivedAt"], "t1");
    assert!(archived.iter().any(|e| e["featureName"] == "c4" && e["archivedAt"] == "t2"));
}

#[test]
fn an_active_run_refuses_and_changes_nothing() {
    let dir = dir("active");
    let data = dir.join("data");
    let ledger = write_ledger(&dir, &sample());
    let before = fs::read_to_string(&ledger).unwrap();

    let err = archive_ledger(&ledger, &data, "proj", 2, false, 1, "t").unwrap_err();

    assert!(matches!(err, ArchiveError::ActiveRun));
    assert_eq!(fs::read_to_string(&ledger).unwrap(), before);
    assert!(!data.exists());
}

#[test]
fn a_ledger_that_does_not_parse_is_refused() {
    let dir = dir("broken");
    let ledger = dir.join(".tdd-features.json");
    fs::write(&ledger, "{\"not\": \"an array\"}").unwrap();

    let err = archive_ledger(&ledger, &dir.join("data"), "proj", 2, false, 0, "t").unwrap_err();

    assert!(matches!(err, ArchiveError::Ledger(_)));
}

#[test]
fn abandoned_entries_move_unless_a_remaining_feature_depends_on_them() {
    let dir = dir("abandoned");
    let data = dir.join("data");
    let ledger = write_ledger(
        &dir,
        &[entry("a1", "abandoned", &[]), entry("a2", "abandoned", &[]), entry("p", "pending", &["a2"])],
    );

    let plan = archive_ledger(&ledger, &data, "proj", 100, false, 0, "t").unwrap();

    assert_eq!(plan.archived, vec!["a1"]);
    assert_eq!(ledger_names(&ledger), vec!["a2", "p"]);
}

#[test]
fn after_archiving_every_remaining_depends_on_name_exists_in_the_ledger() {
    let dir = dir("closed");
    let ledger = write_ledger(&dir, &sample());

    archive_ledger(&ledger, &dir.join("data"), "proj", 2, false, 0, "t").unwrap();

    let value: Value = serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
    let names = ledger_names(&ledger);
    for e in value.as_array().unwrap() {
        for dep in e["dependsOn"].as_array().unwrap() {
            assert!(names.contains(&dep.as_str().unwrap().to_string()), "{dep}");
        }
    }
}

fn parsed(entries: &[Value]) -> Vec<orq_features::FeatureEntry> {
    orq_features::parse_ledger(&serde_json::to_string(entries).unwrap()).unwrap()
}

fn names(items: &[&str]) -> std::collections::HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_pending_feature_depending_on_an_archived_one_is_eligible() {
    let ledger = parsed(&[entry("p", "pending", &["old"]), entry("q", "pending", &["gone"])]);
    let archived = names(&["old"]);

    assert!(orq_features::check_run_eligibility_with_archive(&ledger, "p", &archived).is_ok());
    assert!(orq_features::check_run_eligibility(&ledger, "p").is_err());
    assert!(orq_features::check_run_eligibility_with_archive(&ledger, "q", &archived).is_err());
    let ids = vec!["p".to_string()];
    assert!(orq_features::order_run_group_with_archive(&ledger, &ids, &archived).is_ok());
    assert!(orq_features::check_resume_eligibility_with_archive(&ledger, "p", &ids, &archived).is_ok());
    assert!(orq_features::order_resume_group_with_archive(&ledger, &ids, &ids, &archived).is_ok());
}

#[test]
fn validation_accepts_an_archived_dependency_and_names_it_for_dropping() {
    let ledger = parsed(&[entry("p", "pending", &["old", "q"]), entry("q", "pending", &[])]);
    let archived = names(&["old"]);

    assert!(orq_features::validate_entries(&ledger).is_err());
    assert_eq!(orq_features::validate_entries_with_archive(&ledger, &archived), Ok(()));
    let notes = orq_features::archived_dependency_notes(&ledger, &archived);
    assert_eq!(notes, vec![("p".to_string(), "old is archived and completed: not needed".to_string())]);
    let mut ledger = ledger;
    orq_features::drop_archived_dependencies(&mut ledger, &archived);
    assert_eq!(ledger[0].depends_on, vec!["q"]);
    assert_eq!(orq_features::validate_entries(&ledger), Ok(()));
}

/// Another writer holding the ledger's lock keeps the archive out: nothing is written to either file.
#[test]
fn archiving_waits_for_the_ledger_lock_and_writes_nothing_when_it_stays_held() {
    let dir = dir("locked");
    let data = dir.join("data");
    let ledger = write_ledger(&dir, &sample());
    let before = fs::read_to_string(&ledger).unwrap();
    let lock = fs::OpenOptions::new().create(true).write(true).truncate(false).open(dir.join(".tdd-features.json.lock")).unwrap();
    lock.lock().unwrap();

    let err = archive_ledger(&ledger, &data, "proj", 2, false, 0, "2026-11-20T00:00:00Z").unwrap_err();

    assert!(err.to_string().contains("locked"), "{err}");
    assert_eq!(fs::read_to_string(&ledger).unwrap(), before);
    assert!(!archive_path(&data, "proj").exists());
}

/// A batch that adds a feature named like an archived one makes that name live: a draft depending on it keeps it.
#[test]
fn a_batch_reintroducing_an_archived_name_keeps_the_dependency_on_it() {
    let dir = dir("batch-live");
    let ledger = write_ledger(&dir, &[entry("p", "pending", &[])]);
    let loaded = orq_features::LoadedLedger::load(&ledger).unwrap().with_archive(names(&["old"]));
    let mut again = orq_features::FeatureDraft::new("old");
    again.description = Some("back again".into());
    let mut needs = orq_features::FeatureDraft::new("needs-old");
    needs.depends_on = Some(vec!["old".into()]);

    let saved = loaded.append_many(&[again, needs]).unwrap();

    assert_eq!(saved[1].depends_on, vec!["old"]);
}

/// A long chain of abandoned features a pending one waits on stays whole, and an unrelated abandoned one moves.
#[test]
fn a_long_abandoned_dependency_chain_stays_and_an_unrelated_abandoned_feature_moves() {
    let dir = dir("abandoned-chain");
    let data = dir.join("data");
    let mut entries: Vec<Value> = Vec::new();
    entries.push(entry("stray", "abandoned", &[]));
    for i in (0..300).rev() {
        let deps = if i + 1 < 300 { vec![format!("a{}", i + 1)] } else { Vec::new() };
        let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
        entries.push(entry(&format!("a{i}"), "abandoned", &deps));
    }
    entries.push(entry("p", "pending", &["a0"]));
    let ledger = write_ledger(&dir, &entries);

    let plan = archive_ledger(&ledger, &data, "proj", 100, false, 0, "2026-11-20T00:00:00Z").unwrap();

    assert_eq!(plan.archived, vec!["stray"]);
    assert_eq!(ledger_names(&ledger).len(), 301);
}

#[test]
fn saving_drops_archived_dependencies_and_says_so_while_a_live_name_wins() {
    let dir = dir("save-drops");
    let ledger = write_ledger(&dir, &[entry("p", "pending", &[]), entry("live", "completed", &[])]);
    let archived = names(&["old"]);
    let loaded = orq_features::LoadedLedger::load(&ledger).unwrap().with_archive(archived.clone());

    assert!(loaded.is_archived("old") && !loaded.is_archived("live") && !loaded.is_archived("other"));
    let wanted = vec!["old".to_string(), "live".to_string(), "other".to_string()];
    assert_eq!(loaded.archived_dependency_notes(&wanted), vec!["old is archived and completed: not needed".to_string()]);

    let edits = orq_features::FieldEdits { depends_on: Some(vec!["old".into(), "live".into()]), ..Default::default() };
    let saved = loaded.edit_fields("p", &edits).unwrap();
    assert_eq!(saved.depends_on, vec!["live"]);

    let loaded = orq_features::LoadedLedger::load(&ledger).unwrap().with_archive(archived);
    let mut draft = orq_features::FeatureDraft::new("n");
    draft.depends_on = Some(vec!["old".into()]);
    let added = loaded.append(&draft).unwrap();
    assert!(added.depends_on.is_empty(), "{:?}", added.depends_on);
    let text = fs::read_to_string(&ledger).unwrap();
    assert!(!text.contains("\"old\""), "{text}");
}
