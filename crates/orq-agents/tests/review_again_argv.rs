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
        review_again,
    }
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn a_review_again_asks_for_a_review_and_fix_of_the_commits_after_the_escalated_head_with_its_open_findings() {
    let again = ReviewAgain { from: HEAD, findings: "- leak (high, src/log.rs:7): the token reaches the log" };

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
        let again = ReviewAgain { from, findings: "" };
        assert!(claude.implement_argv(&request("claude", Some(again))).is_err(), "{from} was accepted");
    }
}
