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
        features: &["COM-02"],
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "openai/gpt-6-astra", Some("medium")),
        context: None,
        exit_marker: None,
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
    };
    let argv = claude.implement_argv(&request).unwrap();
    assert_eq!(
        argv,
        vec![
            "--model".to_string(),
            "opus".to_string(),
            "--permission-mode".to_string(),
            "dontAsk".to_string(),
            "/implement-and-review COM-02 --reviewer pi --model openai/gpt-6-astra --thinking medium".to_string(),
        ]
    );

    // A reviewer without a thinking level passes none.
    let plain = ImplementRequest { reviewer: spec("pi", "kimi-k3", None), ..request };
    let argv = claude.implement_argv(&plain).unwrap();
    assert_eq!(argv[4], "/implement-and-review COM-02 --reviewer pi --model kimi-k3");
}

#[test]
fn an_implement_request_the_adapter_cannot_serve_is_a_typed_error() {
    let claude = adapter_for("claude").unwrap();
    let pi = adapter_for("pi").unwrap();
    let mut request = ImplementRequest {
        features: &["COM-02"],
        implementer: spec("pi", "kimi-k3", None),
        reviewer: spec("pi", "kimi-k3", None),
        context: None,
        exit_marker: None,
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
    };
    // The implementer model must belong to the adapter's own harness.
    assert!(matches!(claude.implement_argv(&request), Err(AdapterError::WrongHarness { .. })));
    // pi serves its own models, and refuses another harness's.
    assert!(pi.implement_argv(&request).is_ok());
    assert!(matches!(pi.implement_argv(&ImplementRequest { implementer: spec("claude", "opus", None), ..request.clone() }), Err(AdapterError::WrongHarness { .. })));

    // Nothing from the feature id may turn into extra arguments or a second command.
    request.implementer = spec("claude", "opus", None);
    for bad in ["", "a b", "--help", "x;rm", "a\nb", "$(id)"] {
        let one = [bad];
        let attempt = ImplementRequest { features: &one, ..request.clone() };
        assert!(
            matches!(claude.implement_argv(&attempt), Err(AdapterError::InvalidValue { .. })),
            "{bad:?}"
        );
    }
}

#[test]
fn the_implementers_thinking_level_is_passed_to_claude_as_its_effort() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        features: &["COM-02"],
        implementer: spec("claude", "sonnet", Some("high")),
        reviewer: spec("pi", "kimi-k3", None),
        context: None,
        exit_marker: None,
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
    };
    let argv = claude.implement_argv(&request).unwrap();
    assert_eq!(&argv[..4], ["--model", "sonnet", "--effort", "high"]);
    assert_eq!(argv.len(), 7);

    // A level that is not a plain word is refused, not passed on.
    let bad = ImplementRequest { implementer: spec("claude", "sonnet", Some("--x y")), ..request };
    assert!(matches!(claude.implement_argv(&bad), Err(AdapterError::InvalidValue { .. })));
}

#[test]
fn a_reviewer_harness_no_adapter_exists_for_is_refused_before_any_argv_is_built() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        features: &["COM-02"],
        implementer: spec("claude", "opus", None),
        reviewer: spec("gemini", "flash", None),
        context: None,
        exit_marker: None,
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
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
        features: &["COM-02"],
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "kimi-k3", None),
        context: Some("keep the API stable\nand mention it in the MR"),
        exit_marker: None,
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
    };
    let argv = claude.implement_argv(&request).unwrap();
    assert_eq!(argv.len(), 5, "the context stays inside the one prompt argument");
    assert_eq!(
        argv[4],
        "/implement-and-review COM-02 --reviewer pi --model kimi-k3\n\nExtra context:\nkeep the API stable\nand mention it in the MR"
    );

    // Blank context is no context.
    request.context = Some("  \n ");
    assert_eq!(claude.implement_argv(&request).unwrap()[4], "/implement-and-review COM-02 --reviewer pi --model kimi-k3");

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
        features: &["COM-02"],
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "kimi-k3", None),
        context: Some("keep the API stable"),
        exit_marker: Some("/repo/.git/orq/exit-run-1"),
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
    };

    let argv = claude.implement_argv(&request).unwrap();

    let prompt = argv.last().unwrap();
    assert!(prompt.starts_with("/implement-and-review COM-02 --reviewer pi --model kimi-k3"), "{prompt}");
    assert!(prompt.contains("Extra context:\nkeep the API stable"), "{prompt}");
    assert!(prompt.contains("As your very last step, once everything is committed, create the empty file /repo/.git/orq/exit-run-1 and then stop."), "{prompt}");
}

#[test]
fn an_exit_marker_with_spaces_is_quoted_and_one_that_cannot_be_named_safely_is_refused() {
    let claude = adapter_for("claude").unwrap();
    let ask = |marker: &str| {
        claude.implement_argv(&ImplementRequest {
            features: &["COM-02"],
            implementer: spec("claude", "opus", None),
            reviewer: spec("pi", "kimi-k3", None),
            context: None,
            exit_marker: Some(marker),
            attended: false,
            remote_control: None,
            handoff: None,
            review_again: None,
        })
    };
    let argv = ask("/my repo/.git/orq/exit").unwrap();
    assert!(argv.last().unwrap().contains("create the empty file \"/my repo/.git/orq/exit\" and then stop."), "{argv:?}");
    for bad in ["-x", "relative/path", "/a\nb", "", "/a\"b", "/a`b", "/a\\b"] {
        assert!(ask(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_remote_control_name_makes_claude_start_with_remote_control_before_the_prompt() {
    let claude = adapter_for("claude").unwrap();
    let ask = |name: Option<&str>| {
        claude.implement_argv(&ImplementRequest {
            features: &["COM-02"],
            implementer: spec("claude", "opus", None),
            reviewer: spec("pi", "kimi-k3", None),
            context: None,
            exit_marker: None,
            attended: false,
            remote_control: name,
            handoff: None,
            review_again: None,
        })
    };
    let argv = ask(Some("orq/com-02")).unwrap();
    assert_eq!(&argv[..4], ["--model", "opus", "--remote-control", "orq/com-02"]);
    assert_eq!(argv.len(), 7);
    assert!(!ask(None).unwrap().iter().any(|a| a == "--remote-control"));
    for bad in ["", "-x", "a b", "a;b"] {
        assert!(ask(Some(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn the_failure_explainer_is_a_remote_controllable_claude_session_told_the_failure_and_the_logs() {
    use orq_agents::explain_failure_argv;
    let argv = explain_failure_argv("opus", Some("orq/com-02"), "the gate failed at step test", "/repo/.git/orq/logs/run-1").unwrap();
    assert_eq!(&argv[..4], ["--model", "opus", "--remote-control", "orq/com-02"]);
    let prompt = argv.last().unwrap();
    assert!(prompt.contains("the gate failed at step test") && prompt.contains("/repo/.git/orq/logs/run-1"), "{prompt}");
    assert!(explain_failure_argv("-x", Some("orq/com-02"), "d", "/l").is_err());
    assert!(explain_failure_argv("opus", Some("a b"), "d", "/l").is_err());
    assert!(explain_failure_argv("opus", Some("orq/com-02"), "bell\u{7}", "/l").is_err());

    // With Remote Control opted out the explainer is not reachable from the app either.
    let local = explain_failure_argv("opus", None, "d", "/l").unwrap();
    assert!(!local.iter().any(|a| a == "--remote-control"), "{local:?}");
}

#[test]
fn push_session_is_a_connected_interactive_session_with_only_the_push_tool_and_takes_one_line() {
    use orq_agents::push_notification_argv;
    let marker = std::path::Path::new("/tmp/orq-push/sent");
    let argv = push_notification_argv("orq: COM-02 failed", "orq-push-7", marker).unwrap();
    assert!(!argv.contains(&"-p".to_string()), "a print session can never deliver: {argv:?}");
    assert_eq!(&argv[..6], ["--model", "haiku", "--allowedTools", "PushNotification", "--tools", "PushNotification"]);
    assert!(argv.contains(&"--disable-slash-commands".to_string()), "{argv:?}");
    assert!(argv.contains(&"--strict-mcp-config".to_string()), "no connected tools: {argv:?}");
    assert!(argv.contains(&"--remote-control=orq-push-7".to_string()), "{argv:?}");
    let at = argv.iter().position(|a| a == "--name").unwrap();
    assert_eq!(argv[at + 1], "orq-push-7");
    let at = argv.iter().position(|a| a == "--setting-sources").expect("project and local settings (hooks) are not loaded");
    assert_eq!(argv[at + 1], "user");
    assert!(argv.last().unwrap().contains("orq: COM-02 failed"));
    for bad in ["", "  ", "two\nlines", "bell\u{7}"] {
        assert!(push_notification_argv(bad, "n", marker).is_err(), "{bad:?}");
    }
}

#[test]
fn push_session_marks_when_the_tool_has_run_wherever_the_marker_lives() {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-push-it's $(x) {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("sent");
    let argv = orq_agents::push_notification_argv("orq: COM-02 failed", "n", &marker).unwrap();

    let at = argv.iter().position(|a| a == "--settings").unwrap();
    let settings: serde_json::Value = serde_json::from_str(&argv[at + 1]).unwrap();
    let hook = &settings["hooks"]["PostToolUse"][0];
    assert_eq!(hook["matcher"], "PushNotification");
    let status = std::process::Command::new("sh").args(["-c", hook["hooks"][0]["command"].as_str().unwrap()]).status().unwrap();

    assert!(status.success());
    assert!(marker.exists());
}

#[test]
fn push_session_keeps_the_notification_short_enough_for_the_app() {
    let long = "é".repeat(500);
    let argv = orq_agents::push_notification_argv(&long, "n", std::path::Path::new("/m")).unwrap();
    let prompt = argv.last().unwrap();
    assert_eq!(prompt.matches('é').count(), 190, "{prompt}");
}

#[test]
fn a_group_implement_request_lists_the_features_in_order_and_asks_for_a_commit_per_feature() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        features: &["COM-01", "COM-02", "COM-03"],
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "kimi-k3", None),
        context: None,
        exit_marker: None,
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
    };
    let argv = claude.implement_argv(&request).unwrap();
    let prompt = &argv[4];
    assert!(
        prompt.starts_with("/implement-and-review COM-01 COM-02 COM-03 --reviewer pi --model kimi-k3"),
        "{prompt}"
    );
    assert!(prompt.contains("one commit per feature"), "{prompt}");

    // A single feature asks for nothing extra, so its prompt is unchanged.
    let single = ImplementRequest { features: &["COM-01"], ..request.clone() };
    let argv = claude.implement_argv(&single).unwrap();
    assert_eq!(argv[4], "/implement-and-review COM-01 --reviewer pi --model kimi-k3");

    // Every id must stay a single plain argument, and there must be at least one.
    let bad = ImplementRequest { features: &["COM-01", "x y"], ..request.clone() };
    assert!(matches!(claude.implement_argv(&bad), Err(AdapterError::InvalidValue { .. })));
    let none = ImplementRequest { features: &[], ..request };
    assert!(matches!(claude.implement_argv(&none), Err(AdapterError::InvalidValue { .. })));
}

#[test]
fn a_group_implement_request_asks_for_every_feature_to_be_completed_the_usual_way() {
    let claude = adapter_for("claude").unwrap();
    let request = ImplementRequest {
        features: &["COM-01", "COM-02"],
        implementer: spec("claude", "opus", None),
        reviewer: spec("pi", "kimi-k3", None),
        context: None,
        exit_marker: None,
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
    };
    let prompt = claude.implement_argv(&request).unwrap().pop().unwrap();
    assert!(prompt.contains("Complete every one of these features"), "{prompt}");
}
