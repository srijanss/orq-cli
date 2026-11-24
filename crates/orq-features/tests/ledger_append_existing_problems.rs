//! A problem already in the ledger (an old entry depending on a name removed by hand) never refuses an append
//! or an edit of another entry; only a problem the appended or edited entry brings does.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use orq_features::{parse_ledger, validate_changed_entries, FeatureDraft, FeatureEntry, FieldEdits, LoadedLedger};

fn ledger(label: &str, text: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-append-existing-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".tdd-features.json");
    fs::write(&path, text).unwrap();
    path
}

/// An old completed entry whose dependency was removed from the ledger by hand.
const DANGLING: &str = "[\n  {\"featureName\": \"old\", \"status\": \"completed\", \"dependsOn\": [\"gone\"]},\n  {\"featureName\":\"other\",\"status\":\"draft\"}\n]\n";

#[test]
fn an_unrelated_draft_is_appended_past_an_existing_dangling_dependency() {
    let path = ledger("unrelated", DANGLING);
    let loaded = LoadedLedger::load(&path).unwrap();

    let saved = loaded.append(&FeatureDraft::new("new-one")).unwrap();

    assert_eq!(saved.feature_name, "new-one");
    let after = fs::read_to_string(&path).unwrap();
    let prefix = DANGLING.trim_end().strip_suffix(']').unwrap().trim_end();
    assert!(after.starts_with(prefix), "existing bytes kept: {after}");
    let names: Vec<_> = LoadedLedger::load(&path).unwrap().entries().iter().map(|e| e.feature_name.clone()).collect();
    assert_eq!(names, ["old", "other", "new-one"]);
}

#[test]
fn a_new_entrys_own_problems_still_refuse_the_append_and_write_nothing() {
    let path = ledger("refused", DANGLING);
    let loaded = LoadedLedger::load(&path).unwrap();
    let mut missing = FeatureDraft::new("new-one");
    missing.depends_on = Some(vec!["nowhere".into()]);
    let mut own = FeatureDraft::new("new-one");
    own.depends_on = Some(vec!["new-one".into()]);
    let mut first = FeatureDraft::new("x");
    first.depends_on = Some(vec!["y".into()]);
    let mut second = FeatureDraft::new("y");
    second.depends_on = Some(vec!["x".into()]);

    let refusals = [
        loaded.append(&missing).unwrap_err().to_string(),
        loaded.append(&FeatureDraft::new("other")).unwrap_err().to_string(),
        loaded.append(&own).unwrap_err().to_string(),
        loaded.append_many(&[first, second]).unwrap_err().to_string(),
        loaded.append(&FeatureDraft::new(" ")).unwrap_err().to_string(),
    ];

    assert_eq!(
        refusals,
        [
            "feature \"new-one\" depends on \"nowhere\", which does not exist",
            "featureName \"other\" is repeated",
            "feature \"new-one\" depends on itself",
            "dependency cycle among: x, y",
            "featureName must not be blank",
        ]
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), DANGLING, "nothing was written");
}

#[test]
fn an_existing_self_dependency_or_cycle_does_not_refuse_the_append() {
    let text = "[\n  {\"featureName\": \"me\", \"status\": \"draft\", \"dependsOn\": [\"me\"]},\n  {\"featureName\": \"a\", \"status\": \"draft\", \"dependsOn\": [\"b\"]},\n  {\"featureName\": \"b\", \"status\": \"draft\", \"dependsOn\": [\"a\"]}\n]\n";
    let path = ledger("self-cycle", text);
    let loaded = LoadedLedger::load(&path).unwrap();

    let appended = loaded.append_many_noting(&[FeatureDraft::new("new-one")]).unwrap();

    let shown: Vec<String> = appended.existing_problems.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "the project's ledger has a problem in an existing entry: feature \"me\" depends on itself",
            "the project's ledger has a problem in an existing entry: dependency cycle among: a, b",
        ]
    );
    let mut into_cycle = FeatureDraft::new("c");
    into_cycle.depends_on = Some(vec!["a".into()]);
    let loaded = LoadedLedger::load(&path).unwrap();
    assert!(loaded.append(&into_cycle).is_ok(), "leading into an existing cycle is not a new cycle");
}

#[test]
fn a_new_entry_closing_a_cycle_through_existing_entries_is_refused_beside_an_existing_cycle() {
    // An existing cycle x <-> y comes first, then `old` waiting for a `new` that is not there yet.
    let text = "[\n  {\"featureName\": \"x\", \"status\": \"draft\", \"dependsOn\": [\"y\"]},\n  {\"featureName\": \"y\", \"status\": \"draft\", \"dependsOn\": [\"x\"]},\n  {\"featureName\": \"old\", \"status\": \"draft\", \"dependsOn\": [\"new\"]}\n]\n";
    let path = ledger("closing-cycle", text);
    let mut closing = FeatureDraft::new("new");
    closing.depends_on = Some(vec!["old".into()]);

    let refused = LoadedLedger::load(&path).unwrap().append(&closing).unwrap_err();

    assert_eq!(refused.to_string(), "dependency cycle among: new, old");
    assert_eq!(fs::read_to_string(&path).unwrap(), text, "nothing was written");
}

#[test]
fn an_existing_dependency_on_an_archived_feature_is_not_a_problem_and_a_live_name_wins_over_the_archive() {
    let text = "[\n  {\"featureName\": \"old\", \"status\": \"completed\", \"dependsOn\": [\"archived-one\"]},\n  {\"featureName\": \"both\", \"status\": \"completed\"}\n]\n";
    let path = ledger("archived", text);
    let archive: HashSet<String> = ["archived-one".to_string(), "both".to_string()].into();
    let loaded = LoadedLedger::load(&path).unwrap().with_archive(archive);
    let mut draft = FeatureDraft::new("new-one");
    draft.depends_on = Some(vec!["both".into()]);

    let appended = loaded.append_many_noting(&[draft]).unwrap();

    assert_eq!(appended.warning(), None);
    assert_eq!(appended.entries[0].depends_on, ["both"], "a name both live and archived is kept");
}

/// Entries as given, each parsed alone, so a repeated name (which a whole ledger never loads with) can be built.
fn entries(items: &[&str]) -> Vec<FeatureEntry> {
    items.iter().flat_map(|item| parse_ledger(&format!("[{item}]")).unwrap()).collect()
}

const A_TO_B: &str = "{\"featureName\": \"a\", \"status\": \"draft\", \"dependsOn\": [\"b\"]}";
const A: &str = "{\"featureName\": \"a\", \"status\": \"draft\"}";
const B_TO_A: &str = "{\"featureName\": \"b\", \"status\": \"draft\", \"dependsOn\": [\"a\"]}";

#[test]
fn a_cycle_through_either_holder_of_an_existing_repeated_name_refuses_the_new_entry() {
    for order in [[A_TO_B, A, B_TO_A], [A, A_TO_B, B_TO_A]] {
        let list = entries(&order);

        let refused = validate_changed_entries(&list, &HashSet::new(), &|index| index == 2).unwrap_err();

        assert_eq!(refused.to_string(), "dependency cycle among: a, b", "{order:?}");
    }
}

#[test]
fn an_existing_cycle_through_the_first_holder_of_a_repeated_name_is_reported() {
    let list = entries(&[A_TO_B, A, B_TO_A, "{\"featureName\": \"new-one\", \"status\": \"draft\"}"]);

    let existing = validate_changed_entries(&list, &HashSet::new(), &|index| index == 3).unwrap();

    let shown: Vec<String> = existing.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "the project's ledger has a problem in an existing entry: featureName \"a\" is repeated",
            "the project's ledger has a problem in an existing entry: dependency cycle among: a, b",
        ]
    );
}

#[test]
fn an_edit_of_an_unrelated_feature_succeeds_but_a_missing_dependency_of_the_edited_one_is_refused() {
    let path = ledger("edit", DANGLING);
    let loaded = LoadedLedger::load(&path).unwrap();
    let described = FieldEdits { description: Some("now described".into()), ..FieldEdits::default() };

    let saved = loaded.edit_fields("other", &described).unwrap();

    assert_eq!(saved.description.as_deref(), Some("now described"));
    let loaded = LoadedLedger::load(&path).unwrap();
    let dangling = FieldEdits { depends_on: Some(vec!["nowhere".into()]), ..FieldEdits::default() };
    let refused = loaded.edit_fields("other", &dangling).unwrap_err();
    assert_eq!(refused.to_string(), "feature \"other\" depends on \"nowhere\", which does not exist");
    let refused_old = loaded.edit_fields("old", &described).unwrap_err();
    assert_eq!(refused_old.to_string(), "feature \"old\" depends on \"gone\", which does not exist", "the edited entry's own problem still refuses");
}

#[test]
fn the_append_reports_the_existing_problem_as_in_the_ledger_naming_the_entry() {
    let path = ledger("reported", DANGLING);
    let loaded = LoadedLedger::load(&path).unwrap();

    let appended = loaded.append_many_noting(&[FeatureDraft::new("new-one")]).unwrap();

    assert_eq!(appended.entries.len(), 1);
    let shown: Vec<String> = appended.existing_problems.iter().map(ToString::to_string).collect();
    assert_eq!(shown, ["the project's ledger has a problem in an existing entry: feature \"old\" depends on \"gone\", which does not exist"]);
}

#[test]
fn the_existing_problems_read_as_one_warning_and_none_without_them() {
    let path = ledger("warning", DANGLING);
    let loaded = LoadedLedger::load(&path).unwrap();
    let mut also = FeatureDraft::new("second");
    also.depends_on = Some(vec!["new-one".into()]);

    let appended = loaded.append_many_noting(&[FeatureDraft::new("new-one"), also]).unwrap();

    assert_eq!(
        appended.warning().as_deref(),
        Some("existing ledger problems: the project's ledger has a problem in an existing entry: feature \"old\" depends on \"gone\", which does not exist")
    );
    let clean = ledger("no-warning", "[]");
    let appended = LoadedLedger::load(&clean).unwrap().append_many_noting(&[FeatureDraft::new("new-one")]).unwrap();
    assert_eq!(appended.warning(), None);
}
