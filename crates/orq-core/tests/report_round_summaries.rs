use orq_core::review_input::{Finding, FindingStatus, InputError, Round, Severity};
use orq_core::review_intelligence::apply_intelligence;
use orq_core::review_report::{build_report, render_markdown, ReportFile, ReportInputs};

const SHA: &str = "1111111111111111111111111111111111111111";

fn file(path: &str) -> ReportFile {
    ReportFile { path: path.into(), old_path: None, added: Some(1), deleted: Some(0) }
}

fn finding(id: &str, status: FindingStatus) -> Finding {
    Finding { id: id.into(), severity: Severity::High, file: Some("a.rs".into()), line: Some(1), category: "bug".into(), claim: format!("claim {id}"), status, verdict: None }
}

fn applied(rounds: Vec<Round>) -> serde_json::Value {
    let files = vec![file("a.rs")];
    let mut value = build_report(&ReportInputs {
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        base_sha: "0".repeat(40),
        head_sha: SHA.into(),
        files: files.clone(),
        tests: Err(InputError::Missing),
        review: Err(InputError::Missing),
        generated_at: "2026-01-01T00:00:00Z".into(),
    });
    apply_intelligence(&mut value, &files, &rounds, None);
    value
}

#[test]
fn each_round_counts_what_was_raised_fixed_rejected_and_deferred() {
    let value = applied(vec![
        Round {
            number: 1,
            summary: None,
            findings: vec![finding("a", FindingStatus::Fixed), finding("b", FindingStatus::Fixed), finding("c", FindingStatus::Rejected), finding("d", FindingStatus::Deferred), finding("e", FindingStatus::Open)],
        },
        Round { number: 2, summary: None, findings: vec![] },
    ]);
    let rounds = value["rounds"].as_array().unwrap();
    let first = &rounds[0];
    assert_eq!(
        (first["raised"].as_u64(), first["fixed"].as_u64(), first["rejected"].as_u64(), first["deferred"].as_u64(), first["open"].as_u64()),
        (Some(5), Some(2), Some(1), Some(1), Some(1))
    );
    assert_eq!(first["text"], "Round 1: 5 raised — 2 fixed, 1 rejected, 1 deferred, 1 open.");
    assert_eq!(rounds[1]["text"], "Round 2: no findings raised.");
}

#[test]
fn stored_round_text_is_sanitized_like_every_other_report_field() {
    let mut value = applied(vec![]);
    value["rounds"] = serde_json::json!([{"round": 1, "text": "Round 1: ok\n- forged line \u{1b}[31mred", "summary": null}]);
    let md = render_markdown(&value);
    assert!(!md.contains('\u{1b}'), "{md:?}");
    assert!(!md.contains("\n- forged line"), "{md:?}");
}

#[test]
fn unknown_statuses_count_as_raised_but_are_never_invented_as_an_outcome() {
    let value = applied(vec![
        Round { number: 1, summary: None, findings: vec![finding("a", FindingStatus::Unknown), finding("b", FindingStatus::Unknown)] },
        Round { number: 2, summary: None, findings: vec![finding("c", FindingStatus::Unknown), finding("d", FindingStatus::Fixed)] },
    ]);
    let rounds = value["rounds"].as_array().unwrap();
    assert_eq!(rounds[0]["text"], "Round 1: 2 raised.");
    assert_eq!((rounds[0]["fixed"].as_u64(), rounds[0]["rejected"].as_u64(), rounds[0]["deferred"].as_u64(), rounds[0]["open"].as_u64()), (Some(0), Some(0), Some(0), Some(0)));
    assert_eq!(rounds[1]["text"], "Round 2: 2 raised — 1 fixed.");
    let md = render_markdown(&value);
    assert!(md.contains("- Round 1: 2 raised.") && md.contains("- Round 2: 2 raised — 1 fixed."), "{md}");
}

#[test]
fn a_stored_report_without_round_text_still_renders_its_round_counts() {
    let mut value = applied(vec![]);
    value["rounds"] = serde_json::json!([{"round": 2, "summary": "older", "findings": 3, "confirmed": 1, "potential": 1, "dismissed": 1, "unresolved": 2}]);
    let md = render_markdown(&value);
    assert!(md.contains("Round 2: 3 findings, 1 confirmed, 1 potential, 1 dismissed, 2 unresolved - older"), "{md}");
}

#[test]
fn the_markdown_report_lists_the_round_lines_and_an_unknown_count_is_not_invented() {
    let with_rounds = applied(vec![Round { number: 1, summary: None, findings: vec![finding("a", FindingStatus::Fixed)] }]);
    assert!(render_markdown(&with_rounds).contains("Round 1: 1 raised — 1 fixed."), "{}", render_markdown(&with_rounds));

    let without = applied(vec![]);
    assert_eq!(without["rounds"].as_array().unwrap().len(), 0);
    assert!(!render_markdown(&without).contains("Round 1"));
}
