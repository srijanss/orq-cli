use std::time::{Duration, Instant};

use orq_features::{extract_candidates, review_candidates, Verdict};

#[test]
fn a_long_reverse_ordered_chain_ending_in_an_unknown_dependency_is_propagated_quickly() {
    let n = 8_000;
    let items: Vec<String> = (0..n)
        .map(|i| {
            let dep = if i + 1 == n { "ghost".to_string() } else { format!("f-{}", i + 1) };
            format!(r#"{{"featureName":"f-{i}","status":"draft","dependsOn":["{dep}"]}}"#)
        })
        .collect();
    let output = format!("[{}]", items.join(","));
    let candidates = extract_candidates(&output).unwrap();
    let started = Instant::now();

    let reviewed = review_candidates(&[], candidates);

    assert!(reviewed.iter().all(|r| matches!(r.verdict, Verdict::Invalid(_))));
    assert!(started.elapsed() < Duration::from_millis(500), "took {:?}", started.elapsed());
}
