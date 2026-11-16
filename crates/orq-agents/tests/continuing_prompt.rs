//! A resumed run's session continues the feature the run it resumes started: it is told which run
//! that was, which commits are already on the branch, and to go on under the same feature name.

use orq_agents::{adapter_for, AdapterError, Continuing, ImplementRequest, ModelSpec};

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request<'a>(continuing: Option<Continuing<'a>>) -> ImplementRequest<'a> {
    ImplementRequest {
        features: &["COM-01"],
        implementer: spec("claude", "opus"),
        reviewer: spec("pi", "kimi-k3"),
        context: None,
        exit_marker: Some("/logs/implement.done"),
        attended: false,
        remote_control: None,
        handoff: None,
        continuing,
        review_again: None,
        leave_full_suite_to_gate: false,
        allowed_commands: &[],
    }
}

#[test]
fn a_continuing_session_is_told_the_run_its_commits_and_to_keep_the_feature_name() {
    let claude = adapter_for("claude").unwrap();
    let continuing = Continuing { run: "run-1-2-3", base: "0123456789abcdef0123456789abcdef01234567", log: "abc1234 wip\ndef5678 more\0x" };

    let argv = claude.implement_argv(&request(Some(continuing))).unwrap();

    let prompt = argv.last().unwrap();
    let expected = "This run continues run-1-2-3, which started COM-01 and did not finish it. Its commits are already on this branch (0123456789abcdef0123456789abcdef01234567..HEAD):\nabc1234 wip\ndef5678 more x\nContinue COM-01 under the same featureName: call init_feature with \"COM-01\" and never create a new feature for this work.";
    let at = prompt.find(expected).unwrap_or_else(|| panic!("{prompt}"));
    assert!(at < prompt.find("/logs/implement.done").unwrap(), "{prompt}");

    let fresh = claude.implement_argv(&request(None)).unwrap();
    assert!(!fresh.last().unwrap().contains("This run continues"), "{fresh:?}");
}

#[test]
fn a_continuing_run_or_base_that_is_not_plain_is_refused() {
    let claude = adapter_for("claude").unwrap();
    let bad_base = Continuing { run: "run-1", base: "main; echo", log: "" };
    assert!(matches!(claude.implement_argv(&request(Some(bad_base))), Err(AdapterError::InvalidValue { .. })));
    let bad_run = Continuing { run: "run 1\nx", base: "0123abc", log: "" };
    assert!(matches!(claude.implement_argv(&request(Some(bad_run))), Err(AdapterError::InvalidValue { .. })));
}
