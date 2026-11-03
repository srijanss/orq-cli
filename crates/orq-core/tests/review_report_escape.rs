use orq_core::review_report::{build_report, render_markdown, ReportFile, ReportInputs, TestCommand, TestEvidence, TestStatus};
use orq_core::review_input::InputError;

#[test]
fn control_characters_in_paths_and_commands_never_reach_the_rendering() {
    let tests = TestEvidence {
        status: TestStatus::Passed,
        commands: vec![TestCommand { command: "run\u{1b}[2J\nfake".into(), exit_code: Some(0) }],
        head_sha: None,
    };
    let report = build_report(&ReportInputs {
        run_id: "run-1".into(),
        project_id: "proj".into(),
        feature_id: "feat".into(),
        base_sha: "1".repeat(40),
        head_sha: "2".repeat(40),
        files: vec![ReportFile { path: "evil\u{1b}[31m\n## Risks\nname".into(), old_path: Some("old\rname".into()), added: Some(1), deleted: Some(0) }],
        tests: Ok(tests),
        review: Err(InputError::Missing),
        generated_at: "2026-11-03T00:00:00Z".into(),
    });
    let md = render_markdown(&report);
    assert!(!md.chars().any(|c| c.is_control() && c != '\n'), "{md:?}");
    assert_eq!(md.lines().filter(|line| line.starts_with("## Risks")).count(), 1, "{md}");
    assert!(md.contains("evil\\u{1b}[31m\\n## Risks\\nname"), "{md}");
    // The structured report keeps the original values.
    assert_eq!(report["files"][0]["path"], "evil\u{1b}[31m\n## Risks\nname");
}
