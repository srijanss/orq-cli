use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use orq_core::redact::redact_bytes;

struct Counting;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
        PEAK.fetch_max(now, Ordering::SeqCst);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Peak extra bytes allocated while redacting `input`.
fn peak_extra(input: &[u8]) -> usize {
    let base = CURRENT.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let out = redact_bytes(input);
    let peak = PEAK.load(Ordering::SeqCst) - base;
    drop(out);
    peak
}

#[test]
fn a_plain_log_of_many_empty_lines_needs_memory_proportional_to_its_size() {
    let input = vec![b'\n'; 400_000];
    let extra = peak_extra(&input);
    assert!(extra < input.len() * 4, "peak extra {extra} for {} bytes", input.len());
}

#[test]
fn a_jsonl_log_is_not_held_as_parsed_records_all_at_once() {
    let input = "[1]\n".repeat(100_000).into_bytes();
    let extra = peak_extra(&input);
    assert!(extra < input.len() * 4, "peak extra {extra} for {} bytes", input.len());
}

#[test]
fn streaming_keeps_mixed_output_correct() {
    let out = String::from_utf8(redact_bytes(b"a\n\n{\"k\":\"x\"}\npassword=hunter2hunter2\n")).unwrap();
    assert_eq!(out, "a\n\n{\"k\":\"x\"}\npassword=[REDACTED]\n");
}
