use orq_core::review_input::{Finding, FindingStatus, InputError, Round, Severity};
use orq_core::review_intelligence::{apply_intelligence, repository_base, LinkSource};
use orq_core::review_report::{build_report, ReportFile, ReportInputs};

const SHA: &str = "1111111111111111111111111111111111111111";

fn file(path: &str, old: Option<&str>) -> ReportFile {
    ReportFile { path: path.into(), old_path: old.map(Into::into), added: Some(1), deleted: Some(0) }
}

fn report(files: &[ReportFile], current: bool) -> serde_json::Value {
    let tests = if current { Ok(orq_core::review_report::TestEvidence { status: orq_core::review_report::TestStatus::Passed, commands: vec![], head_sha: Some(SHA.into()) }) } else { Err(InputError::Missing) };
    build_report(&ReportInputs {
        run_id: "r".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        base_sha: "0".repeat(40),
        head_sha: SHA.into(),
        files: files.to_vec(),
        tests,
        review: if current { Ok(orq_core::review_input::ReviewInput { rounds_observed: Some(1), outcome: None, head_sha: Some(SHA.into()) }) } else { Err(InputError::Missing) },
        generated_at: "2026-01-01T00:00:00Z".into(),
    })
}

fn finding(path: &str) -> Finding {
    Finding { id: "a".into(), severity: Severity::High, file: Some(path.into()), line: Some(4), category: "bug".into(), claim: "c".into(), status: FindingStatus::Open, verdict: None }
}

fn applied(provider: &str, base: &str, files: Vec<ReportFile>, current: bool, finding_path: &str) -> serde_json::Value {
    let mut value = report(&files, current);
    let exists = |p: &str| files.iter().any(|f| f.path == p);
    let links = LinkSource { provider, base_url: base, sha: SHA, exists: &exists };
    apply_intelligence(&mut value, &files, &[Round { number: 1, summary: None, findings: vec![finding(finding_path)] }], Some(&links));
    value
}

#[test]
fn the_repository_is_taken_from_the_review_url_for_each_provider() {
    assert_eq!(repository_base("github", "https://github.example/o/r/pull/12").as_deref(), Some("https://github.example/o/r"));
    assert_eq!(repository_base("gitlab", "https://gitlab.example/g/sub/r/-/merge_requests/3").as_deref(), Some("https://gitlab.example/g/sub/r"));
    assert_eq!(repository_base("github", "https://github.example/o/r/issues/1"), None);
    assert_eq!(repository_base("bitbucket", "https://x.example/o/r/pull/1"), None);
    assert_eq!(repository_base("github", "http://github.example/o/r/pull/12"), None);
}

#[test]
fn a_current_report_links_each_changed_file_and_risk_by_provider() {
    let github = applied("github", "https://github.example/o/r", vec![file("src/a b.rs", None)], true, "src/a b.rs");
    let entry = &github["file_risks"]["files"][0];
    assert_eq!(entry["link"], format!("https://github.example/o/r/blob/{SHA}/src/a%20b.rs"));
    assert_eq!(github["risks"][0]["link"], format!("https://github.example/o/r/blob/{SHA}/src/a%20b.rs#L4"));

    let gitlab = applied("gitlab", "https://gitlab.example/g/r", vec![file("a.rs", None)], true, "a.rs");
    assert_eq!(gitlab["file_risks"]["files"][0]["link"], format!("https://gitlab.example/g/r/-/blob/{SHA}/a.rs"));
}

#[test]
fn without_a_current_report_there_is_no_link_and_the_reason_is_stated() {
    let value = applied("github", "https://github.example/o/r", vec![file("a.rs", None)], false, "a.rs");
    assert!(value["file_risks"]["files"][0]["link"].is_null());
    assert!(value["file_risks"]["files"][0]["link_reason"].as_str().unwrap().contains("not current"));
    assert!(value["risks"][0]["link"].is_null());
    assert!(value["risks"][0]["link_reason"].as_str().unwrap().contains("not current"));
}

#[test]
fn a_risk_in_a_file_the_change_does_not_touch_has_no_link_and_says_why() {
    let value = applied("github", "https://github.example/o/r", vec![file("a.rs", None)], true, "other.rs");
    assert!(value["risks"][0]["link"].is_null());
    assert!(value["risks"][0]["link_reason"].as_str().unwrap().contains("not changed"));
}

#[test]
fn a_renamed_file_links_to_its_new_path() {
    let value = applied("github", "https://github.example/o/r", vec![file("new/name.rs", Some("old/name.rs"))], true, "old/name.rs");
    assert_eq!(value["file_risks"]["files"][0]["link"], format!("https://github.example/o/r/blob/{SHA}/new/name.rs"));
}
