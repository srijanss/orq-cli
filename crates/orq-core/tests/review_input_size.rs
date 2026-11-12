use std::path::PathBuf;

use orq_core::review_input::{load_review_findings, InputError, MAX_REVIEW_INPUT_BYTES};

fn file(tag: &str, text: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-input-size-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("review.json");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn a_file_over_the_byte_limit_is_refused_by_name() {
    let padding = "x".repeat(MAX_REVIEW_INPUT_BYTES as usize);
    let text = format!(r#"{{"schema_version":1,"rounds":[{{"summary":"{padding}"}}]}}"#);
    match load_review_findings(&file("big", &text)) {
        Err(InputError::Malformed(reason)) => assert!(reason.contains("larger than"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_just_inside_the_limit_is_still_read() {
    let head = r#"{"schema_version":1,"rounds":[],"pad":""#;
    let fill = "x".repeat(MAX_REVIEW_INPUT_BYTES as usize - head.len() - 2);
    let text = format!(r#"{head}{fill}"}}"#);
    assert_eq!(text.len() as u64, MAX_REVIEW_INPUT_BYTES);
    assert!(load_review_findings(&file("edge", &text)).unwrap().is_empty());
}

#[test]
fn an_unbounded_source_is_not_read_to_the_end() {
    // /dev/zero is endless; reading it whole would never finish.
    let result = load_review_findings(std::path::Path::new("/dev/zero"));
    assert!(matches!(result, Err(InputError::Malformed(_)) | Err(InputError::Inaccessible(_))), "{result:?}");
}
