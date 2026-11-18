//! A run whose review escalated is resumed as a review of what came after the escalated head,
//! not as a new implementation: the session reviews and fixes, told which findings were open.

use orq_agents::{adapter_for, ImplementRequest, ModelSpec, ReviewAgain};

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request<'a>(implementer: &str, review_again: Option<ReviewAgain<'a>>) -> ImplementRequest<'a> {
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
        review_again,
        leave_full_suite_to_gate: false,
        allowed_commands: &[],
        declared_files: &[],
        answer: None,
        undeclared_files: &[],
    }
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn a_review_again_asks_for_a_review_and_fix_of_the_commits_after_the_escalated_head_with_its_open_findings() {
    let again = ReviewAgain { from: HEAD, findings: "- leak (high, src/log.rs:7): the token reaches the log", failed: false };

    let claude = adapter_for("claude").unwrap().implement_argv(&request("claude", Some(again.clone()))).unwrap();
    let prompt = claude.last().unwrap();
    assert!(prompt.starts_with(&format!("/review-and-fix {HEAD}..HEAD --reviewer pi --model openai/gpt-6-astra\n")), "{prompt}");
    let findings_at = prompt.find("- leak (high, src/log.rs:7): the token reaches the log").expect("the findings");
    let marker_at = prompt.find("/logs/implement.done").expect("the marker");
    assert!(findings_at < marker_at, "{prompt}");

    let pi = adapter_for("pi").unwrap().implement_argv(&request("pi", Some(again))).unwrap();
    assert!(pi.iter().any(|a| a.starts_with(&format!("/skill:review-and-fix {HEAD}..HEAD --reviewer pi"))), "{pi:?}");
}

#[test]
fn a_review_again_from_something_other_than_a_commit_name_is_refused() {
    let claude = adapter_for("claude").unwrap();
    for from in ["--help", "HEAD~1", "", "abc"] {
        let again = ReviewAgain { from, findings: "", failed: false };
        assert!(claude.implement_argv(&request("claude", Some(again))).is_err(), "{from} was accepted");
    }
}

#[test]
fn a_failed_run_s_review_again_says_the_run_failed_after_its_review_rather_than_that_it_escalated() {
    let again = ReviewAgain { from: HEAD, findings: "- leak (high): the token reaches the log", failed: true };
    let claude = adapter_for("claude").unwrap().implement_argv(&request("claude", Some(again))).unwrap();
    let prompt = claude.last().unwrap();
    assert!(prompt.starts_with(&format!("/review-and-fix {HEAD}..HEAD ")), "{prompt}");
    assert!(prompt.contains("The run failed after its review and a human asked for another review of the commits since the head it last reviewed. These findings were open when its review ended:\n- leak"), "{prompt}");
    assert!(!prompt.contains("escalated"), "{prompt}");
}

#[test]
fn the_answer_of_the_person_who_resumed_the_run_is_put_in_the_session_s_prompt() {
    let claude = adapter_for("claude").unwrap();
    let answered = ImplementRequest { answer: Some("  use the v2 endpoint\nnot v1 "), ..request("claude", None) };
    let prompt = claude.implement_argv(&answered).unwrap().last().unwrap().clone();
    let answer_at = prompt.find("\n\nThe person who resumed this run answered:\nuse the v2 endpoint\nnot v1").expect(&prompt);
    assert!(answer_at < prompt.find("/logs/implement.done").unwrap(), "{prompt}");

    let blank = ImplementRequest { answer: Some("   "), ..request("claude", None) };
    assert!(!claude.implement_argv(&blank).unwrap().last().unwrap().contains("answered"), "a blank answer says nothing");
    let control = ImplementRequest { answer: Some("a\u{1b}[2Jb"), ..request("claude", None) };
    assert!(claude.implement_argv(&control).is_err(), "an answer with a control character is refused");
    let long = "x".repeat(8001);
    assert!(claude.implement_argv(&ImplementRequest { answer: Some(&long), ..request("claude", None) }).is_err(), "an answer over 8000 characters is refused");
}
