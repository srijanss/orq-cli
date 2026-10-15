use std::time::{Duration, Instant};

use orq_features::{extract_candidates, review_candidates, Verdict};

#[test]
fn many_distinct_unknown_dependencies_are_deduplicated_quickly() {
    let deps: Vec<String> = (0..40_000).map(|i| format!("\"ghost-{}\"", i % 20_000)).collect();
    let output = format!(r#"[{{"featureName":"a","dependsOn":[{}]}}]"#, deps.join(","));
    let started = Instant::now();

    let reviewed = review_candidates(&[], extract_candidates(&output).unwrap());

    let Verdict::Invalid(problems) = &reviewed[0].verdict else { panic!("{:?}", reviewed[0].verdict) };
    assert_eq!(problems.len(), 20_000);
    assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
}
