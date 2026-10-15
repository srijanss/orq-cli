use orq_features::{
    extract_candidates, review_candidates, CandidateProblem, ReviewedCandidate, Verdict,
};

fn review(output: &str) -> Vec<ReviewedCandidate> {
    review_candidates(&[], extract_candidates(output).unwrap())
}

#[test]
fn dependents_of_an_invalid_candidate_are_invalid_too() {
    let reviewed = review(
        r#"[{"featureName":"a","dependsOn":["ghost"]},{"featureName":"b","dependsOn":["a"]},
            {"featureName":"c","dependsOn":["b"]},{"featureName":"ok"}]"#,
    );

    assert!(matches!(reviewed[0].verdict, Verdict::Invalid(_)));
    assert_eq!(
        reviewed[1].verdict,
        Verdict::Invalid(vec![CandidateProblem::UnknownDependency("a".into())])
    );
    assert_eq!(
        reviewed[2].verdict,
        Verdict::Invalid(vec![CandidateProblem::UnknownDependency("b".into())])
    );
    assert_eq!(reviewed[3].verdict, Verdict::New);
}
