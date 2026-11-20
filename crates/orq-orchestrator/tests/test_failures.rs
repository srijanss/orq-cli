//! The tests a run of the suite failed, picked out of what it printed: nextest's FAIL lines and output blocks,
//! libtest's `failures:` list and `---- <test> stdout ----` blocks, and doc tests, each with the end of its own
//! output, redacted, so a failed Accept or gate can say which tests failed and why.

use orq_orchestrator::find_test_failures;

const FAILING: &str = "orq-tui::ui_status_filter_names unknown_statuses_are_listed";

/// A nextest run of 4389 tests in which one failed, as nextest prints it: the failure's STDOUT and STDERR blocks
/// right after its FAIL line, far before the end, and the summary repeating the FAIL line.
fn nextest_run() -> String {
    let mut out = String::from("    Starting 4389 tests across 120 binaries (2 tests skipped)\n");
    for n in 1..=4389 {
        if n == 4382 {
            out.push_str(&format!("        FAIL [   0.010s] ({n}/4389) {FAILING}\n"));
            out.push_str(&format!("──── STDOUT:             {FAILING}\n\nrunning 1 test\ntest unknown_statuses_are_listed ... FAILED\n\nfailures:\n\nfailures:\n    unknown_statuses_are_listed\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s\n\n"));
            out.push_str(&format!("──── STDERR:             {FAILING}\n\nthread 'unknown_statuses_are_listed' panicked at crates/orq-tui/tests/ui_status_filter_names.rs:12:5:\nassertion `left == right` failed\n  left: [\"waiting\"]\n right: [\"waiting\", \"accepting\"]\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n"));
        } else {
            out.push_str(&format!(
                "        PASS [   0.004s] ({n}/4389) orq-core::suite test_{n}\n"
            ));
        }
    }
    out.push_str(
        "────────────\n     Summary [  52.112s] 4389 tests run: 4388 passed, 1 failed, 2 skipped\n",
    );
    out.push_str(&format!(
        "        FAIL [   0.010s] (4382/4389) {FAILING}\nerror: test run failed\n"
    ));
    out
}

#[test]
fn a_nextest_run_gives_the_one_failing_test_with_its_panic_and_left_right() {
    let failures = find_test_failures(&nextest_run());
    let names: Vec<&str> = failures
        .tests
        .iter()
        .map(|test| test.name.as_str())
        .collect();
    assert_eq!(names, [FAILING]);
    assert_eq!(failures.more, 0);
    let output = &failures.tests[0].output;
    assert!(
        output.contains("panicked at crates/orq-tui/tests/ui_status_filter_names.rs:12:5"),
        "{output}"
    );
    assert!(output.contains("left: [\"waiting\"]"), "{output}");
    assert!(
        output.contains("right: [\"waiting\", \"accepting\"]"),
        "{output}"
    );
    assert!(!output.contains("PASS"), "only its own output: {output}");
    assert_eq!(
        failures.headline().as_deref(),
        Some(format!("1 test failed: {FAILING}").as_str())
    );
}

/// What `cargo test` prints when two tests of a binary fail.
const CARGO_TEST: &str = "\
running 3 tests
test store::loads ... ok
test store::saves ... FAILED
test queue::orders ... FAILED

failures:

---- store::saves stdout ----

thread 'store::saves' panicked at src/store.rs:40:9:
assertion `left == right` failed
  left: 1
 right: 2

---- queue::orders stdout ----
thread 'queue::orders' panicked at src/queue.rs:7:5:
the queue was empty


failures:
    store::saves
    queue::orders

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `--lib`
";

#[test]
fn a_cargo_test_failures_list_gives_each_name_and_its_block() {
    let failures = find_test_failures(CARGO_TEST);
    let names: Vec<&str> = failures
        .tests
        .iter()
        .map(|test| test.name.as_str())
        .collect();
    assert_eq!(names, ["store::saves", "queue::orders"]);
    assert!(
        failures.tests[0].output.contains("left: 1"),
        "{:?}",
        failures.tests[0]
    );
    assert!(
        !failures.tests[0].output.contains("queue.rs"),
        "{:?}",
        failures.tests[0]
    );
    assert!(
        failures.tests[1].output.contains("the queue was empty"),
        "{:?}",
        failures.tests[1]
    );
    assert!(
        !failures.tests[1].output.contains("test result"),
        "{:?}",
        failures.tests[1]
    );
    assert_eq!(
        failures.headline().as_deref(),
        Some("2 tests failed: store::saves, queue::orders")
    );
}

#[test]
fn sixty_failures_keep_fifty_and_count_the_rest() {
    let mut out = String::new();
    for n in 0..60 {
        out.push_str(&format!("        FAIL [   0.010s] ({n}/60) suite t{n}\n"));
    }
    let failures = find_test_failures(&out);
    assert_eq!(failures.tests.len(), 50);
    assert_eq!(failures.more, 10);
    assert_eq!(failures.tests[49].name, "suite t49");
    assert_eq!(
        failures.headline().as_deref(),
        Some("60 tests failed: suite t0, suite t1, …")
    );
}

#[test]
fn a_doc_test_failure_is_named() {
    let out = "running 2 tests\ntest src/lib.rs - add (line 3) ... FAILED\n\nfailures:\n\n---- src/lib.rs - add (line 3) stdout ----\nTest executable failed (exit status: 101).\n\nstderr:\nthread 'main' panicked at src/lib.rs:5:1:\nassertion failed: add(1, 1) == 3\n\n\nfailures:\n    src/lib.rs - add (line 3)\n\ntest result: FAILED. 1 passed; 1 failed\n";
    let failures = find_test_failures(out);
    let names: Vec<&str> = failures
        .tests
        .iter()
        .map(|test| test.name.as_str())
        .collect();
    assert_eq!(names, ["src/lib.rs - add (line 3)"]);
    assert!(
        failures.tests[0]
            .output
            .contains("assertion failed: add(1, 1) == 3"),
        "{:?}",
        failures.tests[0]
    );
}

#[test]
fn a_block_with_a_token_in_it_is_redacted() {
    let out = "failures:\n\n---- auth::login stdout ----\nsent GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789\nthread 'auth::login' panicked at src/auth.rs:9:5:\nlogin failed\n\nfailures:\n    auth::login\n\ntest result: FAILED. 0 passed; 1 failed\n";
    let failures = find_test_failures(out);
    let output = &failures.tests[0].output;
    assert!(
        !output.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "{output}"
    );
    assert!(output.contains("login failed"), "{output}");
}

#[test]
fn a_long_block_keeps_its_end_and_its_panic_with_left_right_within_4000_bytes() {
    let noise = "debug: still going\n".repeat(600);
    let out = format!("failures:\n\n---- big::one stdout ----\nthread 'big::one' panicked at src/big.rs:1:1:\nassertion `left == right` failed\n  left: 3\n right: 4\n{noise}the very last line\n\nfailures:\n    big::one\n\ntest result: FAILED. 0 passed; 1 failed\n");
    let failures = find_test_failures(&out);
    let output = &failures.tests[0].output;
    assert!(output.len() <= 4000, "{}", output.len());
    assert!(output.contains("panicked at src/big.rs:1:1"), "{output}");
    assert!(
        output.contains("left: 3") && output.contains("right: 4"),
        "{output}"
    );
    assert!(output.ends_with("the very last line"), "{output}");
}

#[test]
fn a_passing_tests_output_block_does_not_make_it_a_failure() {
    // `--success-output` prints blocks for passing tests too; only a FAIL status names a failure.
    let out = "        PASS [   0.004s] (1/3) suite ok_one\n\
               ──── STDOUT:             suite ok_one\nall fine\n\
               \x20  TRY 1 FAIL [   0.010s] (2/3) suite flaky\n\
               ──── TRY 1 STDERR:       suite flaky\nfirst try broke\n\
               \x20  TRY 2 PASS [   0.010s] (2/3) suite flaky\n\
               ──── TRY 2 STDOUT:       suite flaky\nsecond try fine\n\
               \x20       FAIL [   0.010s] (3/3) suite broken\n\
               ──── STDERR:             suite broken\nthread 'broken' panicked at src/a.rs:1:1:\nboom\n";
    let failures = find_test_failures(out);
    let names: Vec<&str> = failures
        .tests
        .iter()
        .map(|test| test.name.as_str())
        .collect();
    assert_eq!(names, ["suite broken"]);
    assert!(
        failures.tests[0].output.contains("boom"),
        "{:?}",
        failures.tests[0]
    );
}

#[test]
fn a_hundred_thousand_failures_with_output_are_parsed_in_linear_time() {
    let mut out = String::new();
    for n in 0..100_000 {
        out.push_str(&format!("        FAIL [   0.010s] ({n}/100000) suite t{n}\n──── STDERR:             suite t{n}\nbroke {n}\n"));
    }
    let started = std::time::Instant::now();
    let failures = find_test_failures(&out);
    let took = started.elapsed();
    assert_eq!(failures.tests.len(), 50);
    assert_eq!(failures.more, 99_950);
    assert_eq!(failures.tests[49].output, "broke 49");
    // Generous for a loaded machine; a quadratic parse of this takes over a minute.
    assert!(took < std::time::Duration::from_secs(20), "{took:?}");
}

#[test]
fn a_hundred_thousand_failures_that_pass_on_retry_are_parsed_in_linear_time() {
    let mut out = String::new();
    for n in 0..100_000 {
        out.push_str(&format!(
            "   TRY 1 FAIL [   0.010s] ({n}/100000) suite t{n}\n"
        ));
    }
    for n in 0..100_000 {
        out.push_str(&format!(
            "   TRY 2 PASS [   0.010s] ({n}/100000) suite t{n}\n"
        ));
    }
    // One that passed on retry and then failed again is listed once, with its last output.
    out.push_str("        FAIL [   0.010s] (7/100000) suite t7\n──── STDERR:             suite t7\nbroke again\n");
    let started = std::time::Instant::now();
    let failures = find_test_failures(&out);
    let took = started.elapsed();
    let names: Vec<&str> = failures
        .tests
        .iter()
        .map(|test| test.name.as_str())
        .collect();
    assert_eq!(names, ["suite t7"]);
    assert_eq!(failures.more, 0);
    assert_eq!(failures.tests[0].output, "broke again");
    assert!(took < std::time::Duration::from_secs(20), "{took:?}");
}

#[test]
fn a_build_error_names_no_test() {
    let out = "   Compiling orq-core v0.1.1\nerror[E0425]: cannot find value `x` in this scope\n --> src/lib.rs:3:5\n\nerror: could not compile `orq-core` (lib) due to 1 previous error\n";
    let failures = find_test_failures(out);
    assert!(failures.tests.is_empty(), "{failures:?}");
    assert_eq!(failures.headline(), None);
}
