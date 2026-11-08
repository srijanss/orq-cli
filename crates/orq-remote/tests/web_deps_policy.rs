//! What the browser-behaviour tests do when node or the locked web dependencies are missing:
//! a machine without node skips them, a machine with node but without `npm ci` fails them,
//! and only an explicit opt-out turns that failure into a skip.

mod web_deps;

use std::io::ErrorKind;

use web_deps::{classify_node_probe, decide, Decision, NodeProbe};

#[test]
fn everything_present_means_run() {
    assert_eq!(decide(true, true, false), Decision::Run);
    assert_eq!(decide(true, true, true), Decision::Run);
}

#[test]
fn no_node_skips_as_before() {
    assert_eq!(decide(false, false, false), Decision::Skip);
    assert_eq!(decide(false, true, false), Decision::Skip);
}

#[test]
fn node_without_the_dependencies_fails_unless_opted_out() {
    match decide(true, false, false) {
        Decision::Fail(message) => assert!(message.contains("npm ci") && message.contains("ORQ_SKIP_WEB_TESTS"), "{message}"),
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(decide(true, false, true), Decision::Skip);
}

#[test]
fn ready_runs_when_this_machine_has_everything_or_opts_out() {
    // On a machine that fails the policy this test fails too, which is the point.
    let _ = web_deps::ready();
}

#[test]
fn only_a_missing_node_counts_as_no_node() {
    assert_eq!(classify_node_probe(Ok(true)), NodeProbe::Present);
    assert_eq!(classify_node_probe(Err(ErrorKind::NotFound)), NodeProbe::Absent);
}

#[test]
fn a_node_that_cannot_be_started_or_exits_badly_is_a_broken_environment_not_a_skip() {
    for probe in [classify_node_probe(Ok(false)), classify_node_probe(Err(ErrorKind::PermissionDenied)), classify_node_probe(Err(ErrorKind::OutOfMemory))] {
        match probe {
            NodeProbe::Broken(message) => assert!(message.contains("node"), "{message}"),
            other => panic!("expected a broken environment, got {other:?}"),
        }
    }
}
