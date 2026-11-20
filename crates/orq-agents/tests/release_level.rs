//! The one-shot call that picks a release's level: its argv has no tools and carries the evidence as
//! delimited data, and its answer is `{"level": ..., "reason": ...}` or nothing.

use orq_agents::{parse_release_level, release_level_argv, ReleaseLevel};

#[test]
fn the_argv_has_no_tools_and_holds_the_evidence_as_data_after_the_definitions() {
    for (harness, thinking) in [("claude", Some("low")), ("pi", Some("medium")), ("claude", None)] {
        let argv = release_level_argv(harness, "some-model", thinking, "EVIDENCE-TEXT").unwrap();
        assert_eq!(argv[0], "-p");
        assert!(argv.windows(2).any(|w| w == ["--model", "some-model"]), "{argv:?}");
        let tools_off = argv.iter().any(|a| a == "--no-tools") || argv.windows(2).any(|w| w == ["--tools", ""]);
        assert!(tools_off, "{harness}: {argv:?}");
        let flag = if harness == "claude" { "--effort" } else { "--thinking" };
        assert_eq!(argv.contains(&flag.to_string()), thinking.is_some(), "{harness}: {argv:?}");
        let prompt = argv.last().unwrap();
        for word in ["major", "minor", "patch", "\"level\"", "\"reason\""] {
            assert!(prompt.contains(word), "{word}");
        }
        assert!(prompt.contains("<<<\nEVIDENCE-TEXT\n>>>"));
    }
    assert!(release_level_argv("claude", "bad model", None, "x").is_err());
    assert!(release_level_argv("claude", "m", None, "  ").is_err());
    assert!(release_level_argv("nobody", "m", None, "x").is_err());
}

#[test]
fn the_answer_is_read_from_json_even_inside_a_fence_and_anything_else_is_nothing() {
    let got = parse_release_level("```json\n{\"level\":\"minor\",\"reason\":\"adds a flag\"}\n```").unwrap();
    assert_eq!(got, ReleaseLevel { level: "minor".to_string(), reason: "adds a flag".to_string() });
    assert_eq!(parse_release_level("{\"level\":\"major\",\"reason\":\"r\"}").unwrap().level, "major");
    for bad in ["", "minor", "{\"level\":\"huge\",\"reason\":\"r\"}", "{\"level\":\"patch\"}", "{\"level\":\"patch\",\"reason\":\"\"}", "{not json}"] {
        assert_eq!(parse_release_level(bad), None, "{bad}");
    }
}
