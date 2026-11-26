//! An unattended session (one the queue started) has nobody to answer it: it runs in print mode,
//! without Remote Control, and is told never to ask, and what to do instead.

use orq_agents::{adapter_for, ImplementRequest, ModelSpec};

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request(implementer: &str, unattended: bool) -> ImplementRequest<'static> {
    ImplementRequest {
        features: &["COM-01"],
        implementer: spec(implementer, "opus"),
        reviewer: spec("pi", "openai/gpt-6-astra"),
        context: None,
        exit_marker: Some("/logs/implement.done"),
        attended: false,
        unattended,
        remote_control: Some("orq/COM-01"),
        handoff: None,
        continuing: None,
        review_again: None,
        leave_full_suite_to_gate: false,
        allowed_commands: &[],
        declared_files: &[],
        answer: None,
        undeclared_files: &[],
        reopened_findings: &[],
        frontend: false,
        feature_specs: &[],
    }
}

fn argv(request: &ImplementRequest<'_>) -> Vec<String> {
    adapter_for(&request.implementer.harness).unwrap().implement_argv(request).unwrap()
}

#[test]
fn an_unattended_claude_session_runs_in_print_mode_without_remote_control_and_is_told_never_to_ask() {
    let argv = argv(&request("claude", true));
    let prompt = argv.last().unwrap();

    assert_eq!(argv.first().map(String::as_str), Some("-p"), "{argv:?}");
    assert!(!argv.iter().any(|a| a == "--remote-control"), "{argv:?}");
    assert!(prompt.contains("Nobody is watching this session"), "{prompt}");
    assert!(prompt.contains("never ask"), "{prompt}");
    assert!(prompt.contains("review-followup-"), "{prompt}");
    assert!(prompt.contains("needs a decision:"), "{prompt}");
    assert!(!prompt.contains("AskUserQuestion"), "{prompt}");
}

/// The follow-up name the unattended paragraph gives, filled in with `feature`, `finding` and `hash`.
fn followup_pattern_filled(prompt: &str, feature: &str, finding: &str, hash: &str) -> String {
    let start = prompt.find("review-followup-").expect("the prompt names follow-ups");
    let pattern: String = prompt[start..].chars().take_while(|c| *c != ')').collect();
    pattern.replace("<feature>", feature).replace("<finding id>", finding).replace("<hash>", hash)
}

#[test]
fn an_unattended_session_names_its_followups_as_orq_writes_them_with_a_hash_suffix() {
    let argv = argv(&request("claude", true));
    let prompt = argv.last().unwrap();

    assert!(prompt.contains("(review-followup-<feature>-<finding id>-<hash>)"), "{prompt}");

    // orq's followup_name writes `review-followup-<feature>-<finding id>-<12 hex digits>`.
    let written = "review-followup-COM-01-off-by-one-0123456789ab";
    let filled = followup_pattern_filled(prompt, "COM-01", "off-by-one", "0123456789ab");
    assert_eq!(filled, written, "{prompt}");
    let hash = written.rsplit('-').next().unwrap();
    assert_eq!(hash.len(), 12);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn an_attended_session_has_no_unattended_paragraph() {
    let mut attended = request("claude", false);
    attended.attended = true;
    let prompt = argv(&attended).last().unwrap().clone();
    assert!(!prompt.contains("Nobody is watching this session"), "{prompt}");
    assert!(!prompt.contains("review-followup-"), "{prompt}");
}

#[test]
fn an_unattended_pi_session_runs_in_print_mode_and_an_attended_or_ordinary_one_does_not() {
    let unattended = argv(&request("pi", true));
    assert_eq!(unattended.first().map(String::as_str), Some("-p"), "{unattended:?}");
    assert!(unattended.last().unwrap().contains("Nobody is watching this session"), "{unattended:?}");

    let ordinary = argv(&request("pi", false));
    assert!(!ordinary.iter().any(|a| a == "-p"), "{ordinary:?}");
}
