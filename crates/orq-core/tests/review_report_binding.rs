use orq_core::review_input::{load_review_input, ReviewInput};
use orq_core::review_report::{build_report, load_test_evidence, ReportInputs, TestEvidence, TestStatus};

const HEAD: &str = "2222222222222222222222222222222222222222";
const OLD: &str = "3333333333333333333333333333333333333333";

fn inputs(tests: TestEvidence, review: ReviewInput) -> ReportInputs {
    ReportInputs {
        run_id: "run-1".into(),
        project_id: "proj".into(),
        feature_id: "feat".into(),
        base_sha: "1".repeat(40),
        head_sha: HEAD.into(),
        files: vec![],
        tests: Ok(tests),
        review: Ok(review),
        generated_at: "2026-11-03T00:00:00Z".into(),
    }
}

fn tests(head: Option<&str>) -> TestEvidence {
    TestEvidence { status: TestStatus::Passed, commands: vec![], head_sha: head.map(String::from) }
}

fn review(head: Option<&str>) -> ReviewInput {
    ReviewInput { rounds_observed: Some(2), outcome: Some("approved".into()), head_sha: head.map(String::from) }
}

#[test]
fn evidence_for_the_reported_commit_keeps_the_report_current() {
    let report = build_report(&inputs(tests(Some(HEAD)), review(Some(HEAD))));
    assert_eq!(report["status"], "current");
    assert_eq!(report["tests"]["status"], "passed");
    assert_eq!(report["review"]["outcome"], "approved");
}

#[test]
fn evidence_for_an_older_commit_is_hidden_and_reported_as_a_gap() {
    let report = build_report(&inputs(tests(Some(OLD)), review(Some(OLD))));
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["tests"]["status"], "unknown");
    assert!(report["tests"]["reason"].as_str().unwrap().contains(OLD), "{report}");
    assert_eq!(report["review"]["status"], "unavailable");
    assert!(report["review"]["outcome"].is_null());
    assert_eq!(report["gaps"].as_array().unwrap().len(), 2);
}

#[test]
fn evidence_naming_no_commit_is_unverified_and_makes_the_report_incomplete() {
    let report = build_report(&inputs(tests(None), review(None)));
    assert_eq!(report["status"], "incomplete");
    let gaps = report["gaps"].as_array().unwrap();
    assert!(gaps.iter().all(|gap| gap.as_str().unwrap().contains("not bound to a commit")), "{gaps:?}");
    assert_eq!(gaps.len(), 2);
}

#[test]
fn loaders_read_the_commit_the_evidence_names() {
    let dir = std::env::temp_dir().join(format!("orq-report-binding-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tests_path = dir.join("tests.json");
    std::fs::write(&tests_path, format!(r#"{{"schema_version":1,"head_sha":"{HEAD}","commands":[]}}"#)).unwrap();
    assert_eq!(load_test_evidence(&tests_path).unwrap().head_sha.as_deref(), Some(HEAD));
    let review_path = dir.join("review.json");
    std::fs::write(&review_path, format!(r#"{{"schema_version":1,"head_sha":"{HEAD}","outcome":"approved"}}"#)).unwrap();
    assert_eq!(load_review_input(&review_path).unwrap().head_sha.as_deref(), Some(HEAD));
    std::fs::write(&review_path, r#"{"schema_version":1,"head_sha":7}"#).unwrap();
    assert!(load_review_input(&review_path).is_err());
}
