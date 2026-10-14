use std::time::{Duration, Instant};

use orq_tui::{matching_rows, FeatureRow};

fn fixture(count: usize) -> (Vec<FeatureRow>, Vec<(String, Option<String>)>) {
    let rows = (0..count)
        .map(|i| FeatureRow { name: format!("feature-{i}"), status: if i % 2 == 0 { "pending" } else { "done" }.into() })
        .collect();
    let items = (0..count).map(|i| (format!("feature-{i}"), None)).collect();
    (rows, items)
}

#[test]
fn matches_keep_their_order_whether_text_or_status_matched() {
    let (rows, items) = fixture(6);

    assert_eq!(matching_rows(&rows, &items, "PENDING"), vec![0, 2, 4]);
    assert_eq!(matching_rows(&rows, &items, "feature-3"), vec![3]);
    // "e" is in every name and every status: all rows, once each, in order.
    assert_eq!(matching_rows(&rows, &items, "e"), vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn filtering_a_large_list_where_most_rows_match_stays_fast() {
    let (rows, items) = fixture(60_000);
    let started = Instant::now();

    let matches = matching_rows(&rows, &items, "feature");

    assert_eq!(matches.len(), 60_000);
    assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
}
