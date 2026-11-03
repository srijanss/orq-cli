use orq_core::review_input::{Finding, InputError, FindingStatus, Round, Severity, Verdict};
use orq_core::review_intelligence::{apply_intelligence, LinkSource, MAX_CLASSIFIED_FILES};
use orq_core::review_report::{build_report, render_markdown, ReportFile, ReportInputs};

const SHA: &str = "1111111111111111111111111111111111111111";

fn file(path: &str) -> ReportFile {
    ReportFile { path: path.into(), old_path: None, added: Some(1), deleted: Some(0) }
}

fn report(files: Vec<ReportFile>) -> serde_json::Value {
    build_report(&ReportInputs {
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        base_sha: "0".repeat(40),
        head_sha: SHA.into(),
        files,
        tests: Err(InputError::Missing),
        review: Err(InputError::Missing),
        generated_at: "2026-01-01T00:00:00Z".into(),
    })
}

fn finding(id: &str, status: FindingStatus, verdict: Option<Verdict>, path: Option<&str>) -> Finding {
    Finding { id: id.into(), severity: Severity::High, file: path.map(Into::into), line: Some(4), category: "bug".into(), claim: format!("claim {id}"), status, verdict }
}

fn applied(files: Vec<ReportFile>, rounds: Vec<Round>, links: Option<&LinkSource>) -> serde_json::Value {
    let mut value = report(files.clone());
    apply_intelligence(&mut value, &files, &rounds, links);
    value
}

fn round(number: usize, summary: Option<&str>, findings: Vec<Finding>) -> Round {
    Round { number, summary: summary.map(Into::into), findings }
}

#[test]
fn findings_become_risks_with_their_standing_and_evidence() {
    let value = applied(
        vec![file("src/auth/login.rs"), file("src/lib.rs")],
        vec![round(1, None, vec![finding("a", FindingStatus::Open, Some(Verdict::Confirmed), Some("src/auth/login.rs")), finding("b", FindingStatus::Open, None, Some("src/lib.rs"))])],
        None,
    );
    let risks = value["risks"].as_array().unwrap();
    assert_eq!(risks.len(), 2);
    assert_eq!((risks[0]["id"].as_str(), risks[0]["standing"].as_str(), risks[0]["unresolved"].as_bool()), (Some("a"), Some("confirmed"), Some(true)));
    assert_eq!(risks[1]["standing"], "potential");
    assert!(risks[0]["evidence"].as_str().unwrap().contains("verdict confirmed"));
}

#[test]
fn file_risks_list_the_riskiest_files_first_with_their_evidence() {
    let value = applied(vec![file("README.md"), file("src/auth/login.rs"), file("src/main.zig")], vec![], None);
    let files = value["file_risks"]["files"].as_array().unwrap();
    assert_eq!(files[0]["path"], "src/auth/login.rs");
    assert_eq!(files[0]["level"], "high");
    assert!(files[0]["evidence"].as_str().unwrap().contains("auth"));
    assert!(files.iter().any(|f| f["path"] == "src/main.zig" && f["category"] == "unclassified"));
}

#[test]
fn rounds_are_summarized_from_what_was_recorded_not_rerun() {
    let value = applied(
        vec![file("a.rs")],
        vec![
            round(1, Some("first look"), vec![finding("a", FindingStatus::Fixed, None, Some("a.rs")), finding("b", FindingStatus::Rejected, None, None)]),
            round(2, None, vec![finding("c", FindingStatus::Open, None, Some("a.rs"))]),
            round(3, None, vec![]),
        ],
        None,
    );
    let rounds = value["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 3);
    assert_eq!((rounds[0]["findings"].as_u64(), rounds[0]["confirmed"].as_u64(), rounds[0]["dismissed"].as_u64()), (Some(2), Some(1), Some(1)));
    assert_eq!(rounds[0]["summary"], "first look");
    assert_eq!(rounds[1]["unresolved"], 1);
    assert_eq!(rounds[2]["findings"], 0);
}

#[test]
fn links_appear_only_for_files_that_exist_at_the_reviewed_commit() {
    let exists = |p: &str| p == "src/a b.rs";
    let links = LinkSource { base_url: "https://github.example/o/r", sha: SHA, exists: &exists };
    let value = applied(
        vec![file("src/a b.rs"), file("gone.rs")],
        vec![round(1, None, vec![finding("a", FindingStatus::Open, None, Some("src/a b.rs")), finding("b", FindingStatus::Open, None, Some("gone.rs"))])],
        Some(&links),
    );
    let risks = value["risks"].as_array().unwrap();
    assert_eq!(risks[0]["link"], format!("https://github.example/o/r/blob/{SHA}/src/a%20b.rs#L4"));
    assert!(risks[1]["link"].is_null());
}

#[test]
fn the_readable_report_lists_risks_unresolved_and_outdated_ones_included() {
    let value = applied(
        vec![file("src/auth/login.rs")],
        vec![round(1, None, vec![finding("a", FindingStatus::Open, Some(Verdict::Confirmed), Some("src/auth/login.rs")), finding("old", FindingStatus::Deferred, None, Some("src/removed.rs")), finding("done", FindingStatus::Fixed, None, None)])],
        None,
    );
    let md = render_markdown(&value);
    assert!(!md.contains("No findings are recorded"), "{md}");
    assert!(md.contains("claim a") && md.contains("confirmed") && md.contains("unresolved"), "{md}");
    assert!(md.contains("outdated"), "{md}");
    assert!(md.contains("src/auth/login.rs") && md.contains("high"), "{md}");
}

#[test]
fn without_findings_the_report_still_says_so_and_still_lists_file_risks() {
    let value = applied(vec![file("src/auth/login.rs")], vec![], None);
    let md = render_markdown(&value);
    assert!(md.contains("No findings are recorded"), "{md}");
    assert!(md.contains("src/auth/login.rs"), "{md}");
    let plain = render_markdown(&report(vec![file("a.rs")]));
    assert!(plain.contains("No findings are recorded") && !plain.contains("Files needing attention"), "{plain}");
}

#[test]
fn a_diff_too_large_to_classify_in_full_is_reported_as_a_gap() {
    let files: Vec<ReportFile> = (0..MAX_CLASSIFIED_FILES + 3).map(|i| file(&format!("src/f{i}.rs"))).collect();
    let value = applied(files, vec![], None);
    assert_eq!(value["file_risks"]["omitted"], 3);
    assert_eq!(value["status"], "incomplete");
    assert!(value["gaps"].as_array().unwrap().iter().any(|g| g.as_str().unwrap().contains("3 files")));
    assert!(render_markdown(&value).contains("3 files"));
}

#[test]
fn hostile_text_in_findings_cannot_drive_the_terminal() {
    let mut f = finding("a", FindingStatus::Open, None, None);
    f.claim = "x\u{1b}[2J".into();
    let md = render_markdown(&applied(vec![], vec![round(1, None, vec![f])], None));
    assert!(!md.contains('\u{1b}'));
}
