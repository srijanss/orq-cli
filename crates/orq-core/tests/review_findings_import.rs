use std::path::PathBuf;

use orq_core::review_input::{load_review_findings, FindingStatus, InputError, Severity, Verdict};

fn file(tag: &str, text: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-findings-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("review.json");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn rounds_and_findings_are_imported_with_their_evidence_fields() {
    let path = file(
        "ok",
        r#"{"schema_version":1,"rounds":[
            {"summary":"first look","findings":[
                {"id":"a","severity":"high","file":"src/auth.rs","line":12,"category":"security","claim":"token logged","status":"fixed","verdict":"CONFIRMED"},
                {"id":"b","severity":"low","claim":"naming","status":"deferred"}]},
            {"findings":[]}]}"#,
    );
    let rounds = load_review_findings(&path).unwrap();
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0].number, 1);
    assert_eq!(rounds[0].summary.as_deref(), Some("first look"));
    let a = &rounds[0].findings[0];
    assert_eq!((a.id.as_str(), a.severity, a.status, a.verdict), ("a", Severity::High, FindingStatus::Fixed, Some(Verdict::Confirmed)));
    assert_eq!((a.file.as_deref(), a.line, a.category.as_str()), (Some("src/auth.rs"), Some(12), "security"));
    let b = &rounds[0].findings[1];
    assert_eq!((b.file.clone(), b.line, b.verdict, b.status), (None, None, None, FindingStatus::Deferred));
    assert_eq!(rounds[1].number, 2);
    assert!(rounds[1].findings.is_empty() && rounds[1].summary.is_none());
}

#[test]
fn no_rounds_means_no_findings_and_bad_files_are_named() {
    assert!(load_review_findings(&file("none", r#"{"schema_version":1}"#)).unwrap().is_empty());
    assert_eq!(load_review_findings(&PathBuf::from("/nonexistent/orq/review.json")), Err(InputError::Missing));
    assert!(matches!(load_review_findings(&file("junk", "not json")), Err(InputError::Malformed(_))));
    assert!(matches!(load_review_findings(&file("v9", r#"{"schema_version":9,"rounds":[]}"#)), Err(InputError::Malformed(_))));
    assert!(matches!(load_review_findings(&file("shape", r#"{"schema_version":1,"rounds":[{"findings":"x"}]}"#)), Err(InputError::Malformed(_))));
    assert!(matches!(load_review_findings(&file("noid", r#"{"schema_version":1,"rounds":[{"findings":[{"claim":"x"}]}]}"#)), Err(InputError::Malformed(_))));
}

#[test]
fn unknown_words_stay_unknown_instead_of_being_guessed() {
    let path = file("odd", r#"{"schema_version":1,"rounds":[{"findings":[{"id":"a","claim":"c","severity":"catastrophic","status":"maybe","verdict":"HUNCH"}]}]}"#);
    let f = &load_review_findings(&path).unwrap()[0].findings[0];
    assert_eq!((f.severity, f.status, f.verdict), (Severity::Unknown, FindingStatus::Unknown, None));
}

#[test]
fn text_is_single_line_and_bounded() {
    let long = "x".repeat(5000);
    let json = format!(
        r#"{{"schema_version":1,"rounds":[{{"summary":"{long}","findings":[{{"id":"a","claim":"bad\u001b[2J\nstate: merged","file":"src/\u001b]0;x\u0007.rs"}}]}}]}}"#
    );
    let rounds = load_review_findings(&file("text", &json)).unwrap();
    assert!(rounds[0].summary.as_ref().unwrap().chars().count() <= 1000);
    let f = &rounds[0].findings[0];
    assert!(!f.claim.chars().any(char::is_control) && !f.file.as_ref().unwrap().chars().any(char::is_control));
}

#[test]
fn an_oversized_input_is_refused_rather_than_trusted() {
    let many_rounds = format!(r#"{{"schema_version":1,"rounds":[{}]}}"#, vec![r#"{"findings":[]}"#; 11].join(","));
    assert!(matches!(load_review_findings(&file("rounds", &many_rounds)), Err(InputError::Malformed(_))));
    let finding = r#"{"id":"a","claim":"c"}"#;
    let many_findings = format!(r#"{{"schema_version":1,"rounds":[{{"findings":[{}]}}]}}"#, vec![finding; 201].join(","));
    assert!(matches!(load_review_findings(&file("findings", &many_findings)), Err(InputError::Malformed(_))));
}
