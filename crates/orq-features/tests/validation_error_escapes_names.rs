use orq_features::{parse_ledger, validate_entries};

fn no_raw_control(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

#[test]
fn validation_messages_never_carry_raw_control_characters_from_names() {
    let list = parse_ledger(
        r#"[
        {"featureName":"a\u001b[31m","status":"draft","dependsOn":["gone\u001b]0;x\u0007"]}
    ]"#,
    )
    .unwrap();

    let message = validate_entries(&list).unwrap_err().to_string();

    assert!(no_raw_control(&message), "{message:?}");
    assert!(message.contains("gone"), "{message:?}");
}

#[test]
fn cycle_and_duplicate_messages_escape_control_characters_too() {
    let cycle = parse_ledger(
        r#"[
        {"featureName":"a\u001b","status":"draft","dependsOn":["b\u0007"]},
        {"featureName":"b\u0007","status":"draft","dependsOn":["a\u001b"]}
    ]"#,
    )
    .unwrap();
    let dup = parse_ledger(
        r#"[
        {"featureName":"x\u001b","status":"draft"},
        {"featureName":"x\u001b","status":"draft"}
    ]"#,
    )
    .unwrap();

    for list in [cycle, dup] {
        let message = validate_entries(&list).unwrap_err().to_string();
        assert!(no_raw_control(&message), "{message:?}");
    }
}
