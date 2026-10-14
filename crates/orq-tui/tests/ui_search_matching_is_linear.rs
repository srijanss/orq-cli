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

/// Best of three runs, so one descheduled run cannot distort the figure.
fn best_time(count: usize) -> Duration {
    let (rows, items) = fixture(count);
    (0..3)
        .map(|_| {
            let started = Instant::now();
            let matches = matching_rows(&rows, &items, "feature");
            let elapsed = started.elapsed();
            assert_eq!(matches.len(), count);
            elapsed
        })
        .min()
        .unwrap()
}

/// Four times the rows should cost about four times as much; a quadratic
/// filter costs sixteen times. The bound sits far from both, and no absolute
/// deadline is involved, so a slow machine does not fail it.
#[test]
#[ignore = "timing-sensitive; run with --ignored"]
fn quadrupling_the_rows_does_not_square_the_filtering_cost() {
    let small = best_time(15_000).max(Duration::from_micros(50));
    let large = best_time(60_000);

    assert!(large < small * 10, "15k rows took {small:?}, 60k rows took {large:?}");
}

/// Deterministic companion to the timing check above: a large list is
/// filtered completely and in order, with a status-only query as well.
#[test]
fn a_large_list_is_filtered_completely_and_in_order() {
    let (rows, items) = fixture(60_000);

    let all = matching_rows(&rows, &items, "feature");
    assert_eq!(all.len(), 60_000);
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]));

    let pending = matching_rows(&rows, &items, "pending");
    assert_eq!(pending.len(), 30_000);
    assert!(pending.iter().all(|i| i % 2 == 0));
}
