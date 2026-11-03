use orq_core::review_input::{InputError, ReviewInput};
use orq_core::review_report::{build_report, load_test_evidence, render_markdown, ReportFile, ReportInputs, TestEvidence, TestStatus};

const HEAD: &str = "773a1660000000000000000000000000000000bb";
const BASE: &str = "1111111111111111111111111111111111111111";

fn inputs(tests: Result<TestEvidence, InputError>, review: Result<ReviewInput, InputError>, files: Vec<ReportFile>) -> ReportInputs {
    ReportInputs {
        run_id: "run-1".into(),
        project_id: "proj".into(),
        feature_id: "feat".into(),
        base_sha: BASE.into(),
        head_sha: HEAD.into(),
        files,
        tests,
        review,
        generated_at: "2026-11-03T00:00:00Z".into(),
    }
}

fn file(path: &str, added: Option<u64>, deleted: Option<u64>) -> ReportFile {
    ReportFile { path: path.into(), old_path: None, added, deleted }
}

fn passed() -> TestEvidence {
    TestEvidence { status: TestStatus::Passed, commands: vec![], head_sha: Some(HEAD.into()) }
}

#[test]
fn a_complete_report_pins_the_head_and_never_invents_findings() {
    let review = ReviewInput { rounds_observed: Some(2), outcome: Some("approved".into()), head_sha: Some(HEAD.into()) };
    let report = build_report(&inputs(Ok(passed()), Ok(review), vec![file("a.rs", Some(3), Some(1)), file("b.bin", None, None)]));
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["head_sha"], HEAD);
    assert_eq!(report["base_sha"], BASE);
    assert_eq!(report["status"], "current");
    assert_eq!(report["risks"], serde_json::json!([]));
    assert_eq!(report["summary"]["files_changed"], 2);
    assert_eq!(report["summary"]["added"], 3);
    assert_eq!(report["summary"]["deleted"], 1);
    assert_eq!(report["summary"]["binary_files"], 1);
    assert_eq!(report["tests"]["status"], "passed");
    assert_eq!(report["review"]["status"], "available");
    assert_eq!(report["review"]["rounds_observed"], 2);
    assert_eq!(report["generated_at"], "2026-11-03T00:00:00Z");
}

#[test]
fn missing_evidence_makes_the_report_incomplete_and_says_why() {
    let report = build_report(&inputs(Err(InputError::Missing), Err(InputError::Malformed("bad".into())), vec![]));
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["tests"]["status"], "unknown");
    assert_eq!(report["review"]["status"], "unavailable");
    assert!(report["review"]["reason"].as_str().unwrap().contains("bad"));
    let gaps = report["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert_eq!(report["summary"]["files_changed"], 0);
}

#[test]
fn an_unknown_round_count_stays_unknown() {
    let review = ReviewInput { rounds_observed: None, outcome: None, head_sha: Some(HEAD.into()) };
    let report = build_report(&inputs(Ok(passed()), Ok(review), vec![]));
    assert!(report["review"]["rounds_observed"].is_null());
}

#[test]
fn failed_tests_are_reported_as_failed_not_hidden() {
    let tests = TestEvidence { status: TestStatus::Failed, commands: vec![], head_sha: Some(HEAD.into()) };
    let report = build_report(&inputs(Ok(tests), Ok(ReviewInput { rounds_observed: Some(1), outcome: None, head_sha: Some(HEAD.into()) }), vec![]));
    assert_eq!(report["tests"]["status"], "failed");
    assert_eq!(report["status"], "current");
}

#[test]
fn test_status_comes_from_exit_codes_only() {
    let dir = std::env::temp_dir().join(format!("orq-report-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    };
    let ok = write("ok.json", r#"{"schema_version":1,"commands":[{"command":"cargo test","exit_code":0},{"command":"lint","exit_code":0}]}"#);
    assert_eq!(load_test_evidence(&ok).unwrap().status, TestStatus::Passed);
    let bad = write("bad.json", r#"{"schema_version":1,"commands":[{"command":"cargo test","exit_code":0},{"command":"lint","exit_code":2}]}"#);
    assert_eq!(load_test_evidence(&bad).unwrap().status, TestStatus::Failed);
    let none = write("none.json", r#"{"schema_version":1,"commands":[]}"#);
    assert_eq!(load_test_evidence(&none).unwrap().status, TestStatus::NotRun);
    let prose = write("prose.json", r#"{"schema_version":1,"summary":"all tests passed"}"#);
    assert!(matches!(load_test_evidence(&prose), Err(InputError::Malformed(_))));
    let nocode = write("nocode.json", r#"{"schema_version":1,"commands":[{"command":"x"}]}"#);
    assert_eq!(load_test_evidence(&nocode).unwrap().status, TestStatus::Unknown);
    assert_eq!(load_test_evidence(&dir.join("absent.json")), Err(InputError::Missing));
}

#[test]
fn markdown_shows_commit_tests_and_gaps_deterministically() {
    let report = build_report(&inputs(Err(InputError::Missing), Err(InputError::Missing), vec![file("a.rs", Some(1), Some(0))]));
    let md = render_markdown(&report);
    assert!(md.contains(HEAD) && md.contains("a.rs"), "{md}");
    assert!(md.contains("incomplete") && md.contains("unknown") && md.contains("unavailable"), "{md}");
    assert!(md.contains("No findings"), "{md}");
    assert_eq!(md, render_markdown(&report));
}
