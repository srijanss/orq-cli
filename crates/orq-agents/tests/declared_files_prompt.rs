//! The implement session is told which files its features declare and what to do with a change
//! outside them: write_code refusing a file is never worked around silently, and before start_review
//! every committed file outside them is checked needed and tested and named in the exit marker. A
//! resumed or review-again session is also told the files the run already changed outside them.

use orq_agents::{adapter_for, ImplementRequest, ModelSpec};

const REFUSAL_RULE: &str = "when write_code refuses a file because it is outside the feature's files, drill_down into it if the change is behaviour that needs its own test; otherwise make the change with Edit and name the file and why in implement.done. Never edit around a refusal silently.";

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request<'a>(implementer: &str, declared_files: &'a [String], undeclared_files: &'a [String]) -> ImplementRequest<'a> {
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
        leave_full_suite_to_gate: false,
        allowed_commands: &[],
        declared_files,
        undeclared_files,
    }
}

fn prompt(request: &ImplementRequest<'_>) -> String {
    adapter_for(&request.implementer.harness).unwrap().implement_argv(request).unwrap().last().unwrap().clone()
}

#[test]
fn the_session_is_told_its_declared_files_and_never_to_edit_around_a_refusal_silently() {
    let declared = vec!["src/feature.rs".to_string(), "tests/feature_test.rs".to_string()];
    for harness in ["claude", "pi"] {
        let prompt = prompt(&request(harness, &declared, &[]));
        assert!(prompt.contains(REFUSAL_RULE), "{harness}: {prompt}");
        assert!(prompt.contains("The features declare these files: src/feature.rs, tests/feature_test.rs."), "{harness}: {prompt}");
        assert!(
            prompt.contains("Before start_review, check each committed file outside them is needed and tested, and name it and why in implement.done."),
            "{harness}: {prompt}"
        );
        assert!(!prompt.contains("These files are outside the feature's declared files"), "{harness}: {prompt}");
    }
}

#[test]
fn a_session_after_files_were_changed_outside_the_features_is_told_to_check_them() {
    let declared = vec!["src/feature.rs".to_string()];
    let undeclared = vec!["src/other.rs".to_string(), "fixed.txt".to_string()];
    for harness in ["claude", "pi"] {
        let prompt = prompt(&request(harness, &declared, &undeclared));
        assert!(
            prompt.contains("These files are outside the feature's declared files: src/other.rs, fixed.txt; check each change is needed and tested."),
            "{harness}: {prompt}"
        );
    }
}

#[test]
fn a_session_without_declared_files_still_gets_the_refusal_rule_but_no_file_list() {
    for harness in ["claude", "pi"] {
        let prompt = prompt(&request(harness, &[], &[]));
        assert!(prompt.contains(REFUSAL_RULE), "{harness}: {prompt}");
        assert!(!prompt.contains("The features declare these files"), "{harness}: {prompt}");
    }
}

#[test]
fn a_declared_file_name_that_could_break_the_prompt_is_refused() {
    let declared = vec!["src/feature.rs\n/do-something-else".to_string()];
    let request = request("claude", &declared, &[]);
    assert!(adapter_for("claude").unwrap().implement_argv(&request).is_err());
}
