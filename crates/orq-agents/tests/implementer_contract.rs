//! The contract every harness adapter must meet to start an implementer session. Each check runs for
//! every harness in `HARNESSES`; a new harness is added to the list and must pass all of it.

use orq_agents::{adapter_for, AdapterError, ImplementRequest, ModelSpec};

const HARNESSES: [(&str, &str); 2] = [("claude", "opus"), ("pi", "team/model")];

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request<'a>(harness: &str, model: &str, features: &'a [&'a str]) -> ImplementRequest<'a> {
    ImplementRequest {
        features,
        implementer: spec(harness, model),
        reviewer: spec("pi", "kimi-k3"),
        context: None,
        exit_marker: None,
        remote_control: None,
        handoff: None,
    }
}

fn for_each_harness(check: impl Fn(&str, &str)) {
    for (harness, model) in HARNESSES {
        check(harness, model);
    }
}

#[test]
fn every_harness_carries_the_model_the_features_and_the_prompt_in_the_argv() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();
        let argv = adapter.implement_argv(&request(harness, model, &["COM-01", "COM-02"])).unwrap_or_else(|e| panic!("{harness}: {e:?}"));
        assert!(argv.windows(2).any(|w| w[0] == "--model" && w[1].ends_with(model)), "{harness}: {argv:?}");
        let prompt = argv.last().unwrap();
        assert!(prompt.contains("COM-01") && prompt.contains("COM-02"), "{harness}: {prompt}");
        assert!(prompt.find("COM-01") < prompt.find("COM-02"), "{harness}: features keep their order");
        assert_eq!(argv.iter().filter(|a| a.contains("COM-01")).count(), 1, "{harness}: the features appear in the prompt only: {argv:?}");
    });
}

#[test]
fn every_harness_refuses_another_harnesss_model_no_features_and_unsafe_values() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();
        let other = if harness == "claude" { "pi" } else { "claude" };
        assert!(matches!(adapter.implement_argv(&request(other, "x", &["COM-01"])), Err(AdapterError::WrongHarness { .. })), "{harness}");
        assert!(adapter.implement_argv(&request(harness, model, &[])).is_err(), "{harness}: no features");
        for bad in ["--evil", "COM 01", "COM-01\nrm", ""] {
            assert!(adapter.implement_argv(&request(harness, model, &[bad])).is_err(), "{harness}: feature {bad:?}");
        }
        for bad in ["--evil", "a b", ""] {
            assert!(adapter.implement_argv(&request(harness, bad, &["COM-01"])).is_err(), "{harness}: model {bad:?}");
        }
    });
}

#[test]
fn every_harness_keeps_context_handoff_and_exit_marker_inside_the_one_prompt_argument() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();
        let features = ["COM-01"];
        let bare = adapter.implement_argv(&request(harness, model, &features)).unwrap();
        let full = ImplementRequest {
            context: Some("keep the API stable"),
            handoff: Some("Plan:\n- COM-01 (pending)\nbad\0byte"),
            exit_marker: Some("/logs/implement.done"),
            ..request(harness, model, &features)
        };
        let argv = adapter.implement_argv(&full).unwrap();
        assert_eq!(argv.len(), bare.len(), "{harness}: extras add no arguments: {argv:?}");
        let prompt = argv.last().unwrap();
        let at = |needle: &str| prompt.find(needle).unwrap_or_else(|| panic!("{harness}: {needle:?} missing from {prompt}"));
        assert!(at("keep the API stable") < at("Plan:\n- COM-01 (pending)") && at("Plan:\n- COM-01 (pending)") < at("/logs/implement.done"), "{harness}: {prompt}");
        assert!(!argv.iter().any(|a| a.contains('\0')), "{harness}: a NUL cannot be passed to a process");

        let bad_context = ImplementRequest { context: Some("a\u{7}b"), ..request(harness, model, &features) };
        assert!(adapter.implement_argv(&bad_context).is_err(), "{harness}: control characters in context");
        let bad_marker = ImplementRequest { exit_marker: Some("relative/marker"), ..request(harness, model, &features) };
        assert!(adapter.implement_argv(&bad_marker).is_err(), "{harness}: a marker must be an absolute path");
    });
}

#[test]
fn each_harness_names_the_skill_and_the_thinking_flag_its_own_way_and_only_claude_gets_remote_control() {
    let features = ["COM-01"];
    let with = |harness: &str, model: &str| ImplementRequest {
        implementer: ModelSpec { thinking: Some("high".to_string()), ..spec(harness, model) },
        remote_control: Some("my-session"),
        ..request(harness, model, &features)
    };

    let pi = adapter_for("pi").unwrap().implement_argv(&with("pi", "team/model")).unwrap();
    assert_eq!(&pi[..4], ["--model", "team/model", "--thinking", "high"], "{pi:?}");
    assert_eq!(pi.len(), 5, "pi gets no remote control arguments: {pi:?}");
    assert!(pi[4].starts_with("/skill:implement-and-review COM-01"), "{}", pi[4]);

    let claude = adapter_for("claude").unwrap().implement_argv(&with("claude", "opus")).unwrap();
    assert_eq!(&claude[..6], ["--model", "opus", "--effort", "high", "--remote-control", "my-session"], "{claude:?}");
    assert!(claude[6].starts_with("/implement-and-review COM-01"), "{}", claude[6]);

    for harness in ["claude", "pi"] {
        let model = if harness == "claude" { "opus" } else { "team/model" };
        for bad in ["--evil", "a b", ""] {
            let request = ImplementRequest { implementer: ModelSpec { thinking: Some(bad.to_string()), ..spec(harness, model) }, ..request(harness, model, &features) };
            assert!(adapter_for(harness).unwrap().implement_argv(&request).is_err(), "{harness}: thinking {bad:?}");
        }
    }
}
