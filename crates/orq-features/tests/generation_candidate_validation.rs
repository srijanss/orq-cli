use orq_features::{
    extract_candidates, parse_ledger, review_candidates, CandidateProblem, ReviewedCandidate, Verdict,
};

fn review(ledger: &str, output: &str) -> Vec<ReviewedCandidate> {
    let existing = parse_ledger(ledger).unwrap();
    review_candidates(&existing, extract_candidates(output).unwrap())
}

#[test]
fn valid_new_candidates_may_depend_on_the_ledger_or_each_other() {
    let reviewed = review(
        r#"[{"featureName":"base","status":"completed"}]"#,
        r#"[{"featureName":"a","dependsOn":["base"]},{"featureName":"b","dependsOn":["a"]}]"#,
    );

    assert_eq!(reviewed[0].verdict, Verdict::New);
    assert_eq!(reviewed[1].verdict, Verdict::New);
}

#[test]
fn duplicates_are_marked_and_never_replace_existing_entries() {
    let existing = parse_ledger(r#"[{"featureName":"base","status":"completed","cyclesCompleted":2}]"#).unwrap();
    let before = existing.clone();
    let candidates =
        extract_candidates(r#"[{"featureName":"base","status":"pending"},{"featureName":"x"},{"featureName":"x"}]"#)
            .unwrap();

    let reviewed = review_candidates(&existing, candidates);

    assert_eq!(reviewed[0].verdict, Verdict::DuplicateOfExisting);
    assert_eq!(reviewed[1].verdict, Verdict::New);
    assert_eq!(reviewed[2].verdict, Verdict::DuplicateInBatch);
    assert_eq!(existing, before);
}

#[test]
fn unknown_dependencies_and_cycles_are_reported() {
    let reviewed = review(
        "[]",
        r#"[{"featureName":"lost","dependsOn":["ghost"]},
            {"featureName":"a","dependsOn":["b"]},{"featureName":"b","dependsOn":["a"]},
            {"featureName":"me","dependsOn":["me"]},{"featureName":"fine"}]"#,
    );

    assert_eq!(
        reviewed[0].verdict,
        Verdict::Invalid(vec![CandidateProblem::UnknownDependency("ghost".into())])
    );
    for index in [1, 2, 3] {
        assert_eq!(reviewed[index].verdict, Verdict::Invalid(vec![CandidateProblem::Cycle]), "{index}");
    }
    assert_eq!(reviewed[4].verdict, Verdict::New);
}

#[test]
fn ledger_owned_fields_are_stripped_and_parallelizable_is_only_a_proposal() {
    let reviewed = review(
        "[]",
        r#"[{"featureName":"a","status":"completed","recordedAt":"2026-01-01","cyclesCompleted":4,"parallelizable":true},
            {"featureName":"b","parallelizable":false},{"featureName":"c"}]"#,
    );

    assert_eq!(reviewed[0].candidate.draft.to_json(), r#"{"featureName":"a","status":"draft","parallelizable":true}"#);
    assert_eq!(reviewed[0].candidate.ignored_fields, ["status", "recordedAt", "cyclesCompleted"]);
    assert!(reviewed[0].suggests_parallel);
    assert!(!reviewed[1].suggests_parallel);
    assert!(!reviewed[2].suggests_parallel);
}
