use orq_agents::{adapter_for, AdapterError, Capability, ImplementRequest, ModelSpec};

fn spec(harness: &str, model: &str, thinking: Option<&str>) -> ModelSpec {
    ModelSpec {
        harness: harness.to_string(),
        model: model.to_string(),
        thinking: thinking.map(str::to_string),
    }
}

#[test]
fn claude_and_pi_declare_different_capabilities_and_unknown_harnesses_are_refused() {
    let claude = adapter_for("claude").unwrap();
    let pi = adapter_for("pi").unwrap();
    assert_eq!(claude.name(), "claude");
    assert_eq!(pi.name(), "pi");

    // Callers ask for capabilities; the adapters differ in what they declare.
    assert!(claude.supports(Capability::ResumeSession));
    assert!(claude.supports(Capability::PrintMode));
    assert!(!pi.supports(Capability::ResumeSession));
    assert!(pi.supports(Capability::PrintMode));
    assert!(!pi.supports(Capability::RemoteControl));

    assert!(matches!(adapter_for("gemini"), Err(AdapterError::UnknownHarness(name)) if name == "gemini"));
}

#[test]
fn the_claude_implement_argv_runs_implement_and_review_with_the_runs_reviewer_as_arguments() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        feature: "COM-02",
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "openai/gpt-6-astra", Some("medium")),
    };
    let argv = claude.implement_argv(&request).unwrap();
    assert_eq!(
        argv,
        vec![
            "--model".to_string(),
            "opus".to_string(),
            "/implement-and-review COM-02 --reviewer pi --model openai/gpt-6-astra --thinking medium".to_string(),
        ]
    );

    // A reviewer without a thinking level passes none.
    let plain = ImplementRequest { reviewer: spec("pi", "kimi-k3", None), ..request };
    let argv = claude.implement_argv(&plain).unwrap();
    assert_eq!(argv[2], "/implement-and-review COM-02 --reviewer pi --model kimi-k3");
}

#[test]
fn an_implement_request_the_adapter_cannot_serve_is_a_typed_error() {
    let claude = adapter_for("claude").unwrap();
    let pi = adapter_for("pi").unwrap();
    let mut request = ImplementRequest {
        feature: "COM-02",
        implementer: spec("pi", "kimi-k3", None),
        reviewer: spec("pi", "kimi-k3", None),
    };
    // The implementer model must belong to the adapter's own harness.
    assert!(matches!(claude.implement_argv(&request), Err(AdapterError::WrongHarness { .. })));
    // pi does not implement features yet.
    assert!(matches!(pi.implement_argv(&request), Err(AdapterError::Unsupported { .. })));

    // Nothing from the feature id may turn into extra arguments or a second command.
    request.implementer = spec("claude", "opus", None);
    for bad in ["", "a b", "--help", "x;rm", "a\nb", "$(id)"] {
        request.feature = bad;
        assert!(
            matches!(claude.implement_argv(&request), Err(AdapterError::InvalidValue { .. })),
            "{bad:?}"
        );
    }
}

#[test]
fn the_implementers_thinking_level_is_passed_to_claude_as_its_effort() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        feature: "COM-02",
        implementer: spec("claude", "sonnet", Some("high")),
        reviewer: spec("pi", "kimi-k3", None),
    };
    let argv = claude.implement_argv(&request).unwrap();
    assert_eq!(&argv[..4], ["--model", "sonnet", "--effort", "high"]);
    assert_eq!(argv.len(), 5);

    // A level that is not a plain word is refused, not passed on.
    let bad = ImplementRequest { implementer: spec("claude", "sonnet", Some("--x y")), ..request };
    assert!(matches!(claude.implement_argv(&bad), Err(AdapterError::InvalidValue { .. })));
}

#[test]
fn a_reviewer_harness_no_adapter_exists_for_is_refused_before_any_argv_is_built() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        feature: "COM-02",
        implementer: spec("claude", "opus", None),
        reviewer: spec("gemini", "flash", None),
    };
    assert!(matches!(
        claude.implement_argv(&request),
        Err(AdapterError::UnknownHarness(name)) if name == "gemini"
    ));
}
