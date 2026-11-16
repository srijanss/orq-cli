//! A run whose gate runs the full suite after the session tells the session to leave the full suite to
//! that gate: it runs only the tests for what it changed. A fix session is never told so.

use orq_agents::{adapter_for, FixRequest, ImplementRequest, ModelSpec, ReviewAgain};

const LEAVE_FULL_SUITE: &str = "orq's gate runs the full suite after review";

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request(implementer: &str, leave_full_suite_to_gate: bool) -> ImplementRequest<'static> {
    ImplementRequest {
        features: &["COM-01"],
        implementer: spec(implementer, "opus"),
        reviewer: spec("pi", "openai/gpt-6-astra"),
        context: None,
        exit_marker: Some("/logs/implement.done"),
        attended: false,
        remote_control: None,
        handoff: None,
        continuing: None,
        review_again: None,
        leave_full_suite_to_gate,
        allowed_commands: &[],
    }
}

fn prompt(request: &ImplementRequest<'_>) -> String {
    adapter_for(&request.implementer.harness).unwrap().implement_argv(request).unwrap().last().unwrap().clone()
}

#[test]
fn a_session_whose_gate_runs_the_full_suite_is_told_to_run_only_the_tests_for_what_it_changed() {
    for harness in ["claude", "pi"] {
        let prompt = prompt(&request(harness, true));
        assert!(prompt.contains(LEAVE_FULL_SUITE), "{harness}: {prompt}");
        assert!(prompt.contains("run only the tests for what you changed, through run_tests"), "{harness}: {prompt}");
    }
}

#[test]
fn a_session_not_asked_to_leave_the_full_suite_gets_todays_prompt() {
    for harness in ["claude", "pi"] {
        let prompt = prompt(&request(harness, false));
        assert!(!prompt.contains(LEAVE_FULL_SUITE), "{harness}: {prompt}");
    }
}

/// The review-and-fix session of a review asked for again runs inside the same gated run.
#[test]
fn a_review_again_session_is_told_to_leave_the_full_suite_to_the_gate_too() {
    let findings = "- leak (high, src/log.rs:7): the token reaches the log";
    let again = ImplementRequest {
        review_again: Some(ReviewAgain { from: "0123456789abcdef0123456789abcdef01234567", findings }),
        ..request("claude", true)
    };
    let prompt = prompt(&again);
    assert!(prompt.starts_with("/review-and-fix "), "{prompt}");
    assert!(prompt.contains(LEAVE_FULL_SUITE), "{prompt}");
}

/// A gate-fix session repairs full-suite failures and may need to run the full suite again.
#[test]
fn a_fix_session_is_never_told_to_leave_the_full_suite_to_the_gate() {
    for harness in ["claude", "pi"] {
        let fix = FixRequest {
            implementer: spec(harness, "opus"),
            task: "The gate failed. Fix it.",
            logs_dir: "/logs",
            exit_marker: Some("/logs/gate-fix.done"),
            attended: false,
            allowed_commands: &[],
        };
        let argv = adapter_for(harness).unwrap().fix_argv(&fix).unwrap();
        assert!(!argv.iter().any(|a| a.contains(LEAVE_FULL_SUITE)), "{harness}: {argv:?}");
    }
}
