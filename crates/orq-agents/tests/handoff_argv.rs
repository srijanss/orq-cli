use orq_agents::{adapter_for, ImplementRequest, ModelSpec};

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

#[test]
fn a_handoff_summary_goes_into_the_one_prompt_argument_after_the_request_and_before_the_exit_marker() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        features: &["COM-02"],
        implementer: spec("claude", "opus"),
        reviewer: spec("pi", "kimi-k3"),
        context: Some("keep the API stable"),
        exit_marker: Some("/logs/implement.done"),
        attended: false,
        remote_control: None,
        handoff: Some("Plan:\n- COM-02 (in_progress)\nRemaining: gate"),
    };
    let argv = claude.implement_argv(&request).unwrap();
    assert_eq!(argv.len(), 3, "the summary stays inside the one prompt argument: {argv:?}");
    let prompt = &argv[2];
    let request_at = prompt.find("/implement-and-review COM-02").expect("the request");
    let handoff_at = prompt.find("Handoff from the previous session:\nPlan:\n- COM-02 (in_progress)\nRemaining: gate").expect("the summary");
    let marker_at = prompt.find("/logs/implement.done").expect("the marker");
    assert!(request_at < handoff_at && handoff_at < marker_at, "{prompt}");

    // No handoff, no section; a blank one is no handoff.
    for none in [None, Some("  \n ")] {
        let argv = claude.implement_argv(&ImplementRequest { handoff: none, ..request.clone() }).unwrap();
        assert!(!argv[2].contains("Handoff"), "{}", argv[2]);
    }
}
