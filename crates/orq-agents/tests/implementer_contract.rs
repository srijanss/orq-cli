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
        attended: false,
        remote_control: None,
        handoff: None,
        review_again: None,
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
fn an_attended_session_is_told_to_ask_and_wait_and_to_leave_the_marker_only_with_nothing_left_to_ask() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();
        let features = ["COM-01"];
        let marked = ImplementRequest { exit_marker: Some("/logs/implement.done"), ..request(harness, model, &features) };
        let attended = ImplementRequest { attended: true, ..marked.clone() };

        let prompt = adapter.implement_argv(&attended).unwrap().last().unwrap().clone();
        assert!(prompt.contains("watching this session"), "{harness}: {prompt}");
        assert!(prompt.contains("wait for the answer"), "{harness}: {prompt}");
        assert!(prompt.contains("nothing is left to do or ask"), "{harness}: {prompt}");
        assert_eq!(prompt.contains("AskUserQuestion"), harness == "claude", "{harness}: {prompt}");
        assert!(prompt.find("watching this session") < prompt.find("/logs/implement.done"), "{harness}: {prompt}");

        let unattended = adapter.implement_argv(&marked).unwrap().last().unwrap().clone();
        assert!(!unattended.contains("watching this session"), "{harness}: {unattended}");
        assert!(unattended.contains("As your very last step, once everything is committed, create the empty file"), "{harness}: {unattended}");

        // Without a marker the session is still told it can ask.
        let no_marker = ImplementRequest { attended: true, ..request(harness, model, &features) };
        let prompt = adapter.implement_argv(&no_marker).unwrap().last().unwrap().clone();
        assert!(prompt.contains("watching this session"), "{harness}: {prompt}");
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
            review_again: None,
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

fn fix_request<'a>(harness: &str, model: &str, exit_marker: Option<&'a str>, attended: bool) -> orq_agents::FixRequest<'a> {
    orq_agents::FixRequest { implementer: spec(harness, model), task: "The gate failed. Fix it.", logs_dir: "/state/logs/run-1", exit_marker, attended }
}

#[test]
fn an_attended_fix_session_is_interactive_says_someone_is_watching_and_how_to_end_it() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();

        let argv = adapter.fix_argv(&fix_request(harness, model, Some("/state/logs/run-1/gate-fix.done"), true)).unwrap();

        assert!(!argv.iter().any(|a| a == "-p"), "{harness}: {argv:?}");
        let prompt = argv.last().unwrap();
        assert!(prompt.starts_with("The gate failed. Fix it."), "{harness}: {prompt}");
        assert!(prompt.contains("A person is watching this session"), "{harness}: {prompt}");
        assert!(prompt.contains("create the empty file /state/logs/run-1/gate-fix.done and then stop."), "{harness}: {prompt}");
        assert!(prompt.contains("only after the fix is committed"), "{harness}: {prompt}");
    });
}

#[test]
fn an_unattended_fix_session_is_headless_and_talks_to_nobody() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();

        let argv = adapter.fix_argv(&fix_request(harness, model, None, false)).unwrap();

        assert_eq!(argv.first().map(String::as_str), Some("-p"), "{harness}: {argv:?}");
        assert_eq!(argv.last().map(String::as_str), Some("The gate failed. Fix it."), "{harness}: {argv:?}");
    });
}

#[test]
fn a_fix_session_refuses_what_would_not_stay_one_safe_argument() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();

        assert!(adapter.fix_argv(&fix_request(harness, "--bad", None, false)).is_err(), "{harness}: model");
        assert!(adapter.fix_argv(&fix_request(harness, model, Some("relative/marker"), true)).is_err(), "{harness}: marker");
        let controlled = orq_agents::FixRequest { task: "fix\u{1b}[2J", ..fix_request(harness, model, None, false) };
        assert!(adapter.fix_argv(&controlled).is_err(), "{harness}: task");
    });
    let other = adapter_for("claude").unwrap().fix_argv(&fix_request("pi", "team/model", None, false));
    assert!(matches!(other, Err(AdapterError::WrongHarness { .. })), "{other:?}");
}

#[test]
fn a_fix_session_refuses_a_task_its_program_would_read_as_an_option() {
    for_each_harness(|harness, model| {
        let adapter = adapter_for(harness).unwrap();

        for task in ["--help", "  --help", "-p"] {
            let request = orq_agents::FixRequest { task, ..fix_request(harness, model, None, false) };
            assert!(adapter.fix_argv(&request).is_err(), "{harness}: {task}");
        }
    });
}

#[test]
fn a_claude_fix_session_may_read_the_logs_without_its_prompt_becoming_another_directory() {
    let argv = adapter_for("claude").unwrap().fix_argv(&fix_request("claude", "opus", None, false)).unwrap();

    // `--add-dir` takes any number of values, so what follows it must be another option.
    let at = argv.iter().position(|a| a == "--add-dir").unwrap();
    assert_eq!(argv[at + 1], "/state/logs/run-1");
    assert!(argv[at + 2].starts_with('-'), "{argv:?}");

    let leading = fix_request("claude", "opus", None, false);
    assert!(adapter_for("claude").unwrap().fix_argv(&orq_agents::FixRequest { logs_dir: "-rf", ..leading }).is_err());
}

#[test]
fn a_session_with_a_marker_is_told_to_write_down_what_it_could_not_do() {
    for_each_harness(|harness, model| {
        let features = ["COM-01"];
        let adapter = adapter_for(harness).unwrap();
        for attended in [true, false] {
            let marked = ImplementRequest {
                attended,
                exit_marker: Some("/state/logs/run-1/implement.done"),
                ..request(harness, model, &features)
            };
            let prompt = adapter.implement_argv(&marked).unwrap().last().unwrap().clone();
            assert!(prompt.contains("create the empty file /state/logs/run-1/implement.done and then stop."), "{harness}: {prompt}");
            assert!(prompt.contains("could not be done"), "{harness}: {prompt}");
            assert!(prompt.contains("write what went wrong into that file"), "{harness}: {prompt}");
        }

        let unmarked = adapter.implement_argv(&request(harness, model, &features)).unwrap().last().unwrap().clone();
        assert!(!unmarked.contains("write what went wrong"), "{harness}: {unmarked}");
    });
}
