use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use orq_tui::{matching_rows, matching_rows_visiting, FeatureRow};

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

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Counts allocations per thread, so parallel tests do not disturb each
/// other and the figure is the same on every machine and run.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn allocations_for(count: usize) -> usize {
    let (rows, items) = fixture(count);
    let before = ALLOCATIONS.with(Cell::get);
    let matches = matching_rows(&rows, &items, "feature");
    let used = ALLOCATIONS.with(Cell::get) - before;
    assert_eq!(matches.len(), count);
    used
}

/// Deterministic scaling check, with no clock involved: the filter's work is
/// counted as allocations (each row's lowercase copies), which a linear pass
/// keeps at a fixed few per row. Re-scanning, re-collecting or re-filtering
/// per row multiplies that count, and quadrupling the rows then does more than
/// quadruple it.
#[test]
fn filtering_work_per_row_does_not_grow_with_the_list() {
    let small = allocations_for(15_000);
    let large = allocations_for(60_000);

    assert!(small <= 15_000 * 4, "{small} allocations for 15k rows");
    assert!(large <= small * 4 + 64, "15k rows used {small} allocations, 60k rows used {large}");
}

fn visits_for(count: usize, query: &str) -> (usize, usize) {
    let (rows, items) = fixture(count);
    let mut visits = 0;
    let matches = matching_rows_visiting(&rows, &items, query, |_| visits += 1);
    (visits, matches.len())
}

/// The filter reports every row it examines. A single pass examines each row
/// exactly once, so the count is the list length for any query; a filter that
/// re-scans per row examines more and the growth is no longer linear.
#[test]
fn each_row_is_examined_exactly_once_whatever_the_query() {
    for query in ["feature", "pending", "no-such-text", ""] {
        let (small, _) = visits_for(1_000, query);
        let (large, _) = visits_for(4_000, query);

        assert_eq!(small, 1_000, "query {query:?}");
        assert_eq!(large, 4_000, "query {query:?}");
    }
}

#[test]
fn the_visiting_variant_returns_what_matching_rows_returns() {
    let (rows, items) = fixture(10);

    for query in ["feature-3", "done", "zzz", ""] {
        assert_eq!(matching_rows_visiting(&rows, &items, query, |_| {}), matching_rows(&rows, &items, query));
    }
}
