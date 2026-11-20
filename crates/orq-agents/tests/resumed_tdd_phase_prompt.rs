//! A resumed run whose worktree has the feature open in the TDD state is told its phase and depth and
//! to carry on from there; and every implement prompt allows the review loop's own init_feature.

use orq_agents::{adapter_for, Continuing, ImplementRequest, ModelSpec, OpenFeature};

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request<'a>(continuing: Option<Continuing<'a>>, attended: bool) -> ImplementRequest<'a> {
    ImplementRequest {
        features: &["COM-01"],
        implementer: spec("claude", "opus"),
        reviewer: spec("pi", "kimi-k3"),
        context: None,
        exit_marker: Some("/logs/implement.done"),
        attended,
        unattended: !attended,
        remote_control: None,
        handoff: None,
        continuing,
        review_again: None,
        leave_full_suite_to_gate: false,
        allowed_commands: &[],
        declared_files: &[],
        answer: None,
        undeclared_files: &[],
        feature_specs: &[],
    }
}

fn prompt(continuing: Option<Continuing<'_>>, attended: bool) -> String {
    adapter_for("claude").unwrap().implement_argv(&request(continuing, attended)).unwrap().pop().unwrap()
}

fn continuing(open_feature: Option<OpenFeature<'static>>) -> Continuing<'static> {
    Continuing { run: "run-1-2-3", base: "0123abc", log: "abc1234 wip", open_feature }
}

#[test]
fn a_resumed_prompt_with_an_open_tdd_state_names_the_phase_and_depth_and_says_to_carry_on() {
    let text = prompt(Some(continuing(Some(OpenFeature { name: "COM-01", phase: "implement", depth: 2 }))), false);

    assert!(text.contains("The feature COM-01 is still open in the TDD state: phase implement, depth 2."), "{text}");
    assert!(text.contains("Carry on from there: finish the open cycles, and only then complete the feature."), "{text}");
    assert!(text.contains("Never complete a feature to get a review started."), "{text}");
}

/// The open state belongs to the feature it names, which need not be the run's own: a session that
/// stopped inside a review-finding feature is told that one is open, never that the run's feature is.
#[test]
fn a_resumed_prompt_names_the_feature_that_is_open_not_the_runs_feature() {
    let text = prompt(Some(continuing(Some(OpenFeature { name: "fix-the-finding", phase: "red", depth: 1 }))), false);

    assert!(text.contains("The feature fix-the-finding is still open in the TDD state: phase red, depth 1."), "{text}");
    assert!(!text.contains("The feature COM-01 is still open"), "{text}");
}

#[test]
fn a_resumed_prompt_without_an_open_state_says_nothing_of_it() {
    let text = prompt(Some(continuing(None)), false);

    assert!(!text.contains("still open in the TDD state"), "{text}");
    assert!(!text.contains("to get a review started"), "{text}");
}

#[test]
fn both_prompts_allow_the_review_loops_init_feature_and_still_forbid_ledger_workarounds() {
    for attended in [false, true] {
        let text = prompt(None, attended);

        assert!(text.contains("never get around it by changing the TDD ledger"), "{text}");
        assert!(
            text.contains("Starting a fix for a review finding with init_feature(reviewFindingId=...), under the finding's own feature name, is the normal review loop and allowed"),
            "{text}"
        );
        assert!(text.contains("never get around"), "{text}");
    }
}
