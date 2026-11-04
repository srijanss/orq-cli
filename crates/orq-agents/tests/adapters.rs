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
        context: None,
        exit_marker: None,
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
        context: None,
        exit_marker: None,
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
        context: None,
        exit_marker: None,
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
        context: None,
        exit_marker: None,
    };
    assert!(matches!(
        claude.implement_argv(&request),
        Err(AdapterError::UnknownHarness(name)) if name == "gemini"
    ));
}

#[test]
fn extra_context_is_appended_to_the_request_as_one_argument_and_bad_text_is_refused() {
    let claude = adapter_for("claude").unwrap();
    let mut request = ImplementRequest {
        feature: "COM-02",
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "kimi-k3", None),
        context: Some("keep the API stable\nand mention it in the MR"),
        exit_marker: None,
    };
    let argv = claude.implement_argv(&request).unwrap();
    assert_eq!(argv.len(), 3, "the context stays inside the one prompt argument");
    assert_eq!(
        argv[2],
        "/implement-and-review COM-02 --reviewer pi --model kimi-k3\n\nExtra context:\nkeep the API stable\nand mention it in the MR"
    );

    // Blank context is no context.
    request.context = Some("  \n ");
    assert_eq!(claude.implement_argv(&request).unwrap()[2], "/implement-and-review COM-02 --reviewer pi --model kimi-k3");

    // Control characters other than newline and tab, and over-long text, are refused.
    let long = "x".repeat(4001);
    for bad in ["bell\u{7}", "esc\u{1b}[31m", "nul\0", long.as_str()] {
        request.context = Some(bad);
        assert!(matches!(claude.implement_argv(&request), Err(AdapterError::InvalidValue { field: "context", .. })), "{bad:?}");
    }
}

#[test]
fn context_limits_count_characters_and_allow_tabs_and_newlines() {
    use orq_agents::{checked_context, MAX_CONTEXT_CHARS};

    let exact = "x".repeat(MAX_CONTEXT_CHARS);
    assert_eq!(checked_context(&exact).unwrap(), Some(exact.as_str()));

    // Multibyte characters count once each, not once per byte.
    let wide = "é".repeat(MAX_CONTEXT_CHARS);
    assert!(wide.len() > MAX_CONTEXT_CHARS);
    assert_eq!(checked_context(&wide).unwrap(), Some(wide.as_str()));
    let too_wide = "é".repeat(MAX_CONTEXT_CHARS + 1);
    assert!(matches!(checked_context(&too_wide), Err(AdapterError::InvalidValue { field: "context", .. })));

    assert_eq!(checked_context("a\tb\nc").unwrap(), Some("a\tb\nc"));
    assert_eq!(checked_context(" \n ").unwrap(), None);
}

#[test]
fn an_exit_marker_makes_the_claude_prompt_ask_for_the_file_as_the_last_step() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        feature: "COM-02",
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "kimi-k3", None),
        context: Some("keep the API stable"),
        exit_marker: Some("/repo/.git/orq/exit-run-1"),
    };

    let argv = claude.implement_argv(&request).unwrap();

    let prompt = argv.last().unwrap();
    assert!(prompt.starts_with("/implement-and-review COM-02 --reviewer pi --model kimi-k3"), "{prompt}");
    assert!(prompt.contains("Extra context:\nkeep the API stable"), "{prompt}");
    assert!(prompt.ends_with("As your very last step, once everything is committed, create the empty file /repo/.git/orq/exit-run-1 and then stop."), "{prompt}");
}

#[test]
fn an_exit_marker_with_whitespace_or_a_leading_dash_is_refused() {
    let claude = adapter_for("claude").unwrap();
    for bad in ["/a b", "-x", "/a\nb", ""] {
        let request = ImplementRequest {
            feature: "COM-02",
            implementer: spec("claude", "opus", None),
            reviewer: spec("pi", "kimi-k3", None),
            context: None,
            exit_marker: Some(bad),
        };
        assert!(claude.implement_argv(&request).is_err(), "{bad:?}");
    }
}
