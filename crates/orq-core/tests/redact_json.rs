use orq_core::redact::redact_bytes;
use orq_core::storage::{save_run_file, RunFile};
use orq_core::store::Store;
use serde_json::Value;

const SHA: &str = "773a1660000000000000000000000000000000bb";

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| panic!("not valid JSON ({e}): {}", String::from_utf8_lossy(bytes)))
}

#[test]
fn a_secret_behind_escaped_quotes_is_removed_and_the_json_stays_valid() {
    let report = br#"{"note":"set password=\"hunter2hunter2\" then rerun","keep":"fine","n":3}"#;
    let out = redact_bytes(report);
    assert!(!String::from_utf8_lossy(&out).contains("hunter2hunter2"));
    let value = parse(&out);
    assert_eq!(value["keep"], "fine");
    assert_eq!(value["n"], 3);
    assert!(value["note"].as_str().unwrap().contains("[REDACTED]"));
}

#[test]
fn a_truncated_private_key_inside_a_string_cannot_eat_the_rest_of_the_document() {
    let report = format!(r#"{{"log":"{}\nMIIEowIBAAKCAQEA","after":"still here"}}"#, "\x2D----BEGIN RSA PRIVATE KEY-----").into_bytes();
    let out = redact_bytes(&report);
    assert!(!String::from_utf8_lossy(&out).contains("MIIEowIBAAKCAQEA"));
    assert_eq!(parse(&out)["after"], "still here");
}

#[test]
fn values_of_secret_named_keys_are_removed_wherever_they_are_nested() {
    let out = redact_bytes(br#"{"a":{"b":[{"api_key":"custom-secret-value","ok":"x"}]}}"#);
    assert!(!String::from_utf8_lossy(&out).contains("custom-secret-value"));
    assert_eq!(parse(&out)["a"]["b"][0]["ok"], "x");
}

#[test]
fn json_that_holds_no_secret_is_stored_byte_for_byte() {
    let pretty = b"{\n  \"zeta\": 1,\n  \"alpha\": [1, 2]\n}\n";
    assert_eq!(redact_bytes(pretty), pretty);
}

#[test]
fn every_record_of_a_jsonl_log_stays_a_record() {
    let log = b"{\"msg\":\"clone https://u:pa55w0rd@example.com/r.git\"}\n{\"msg\":\"ok\"}\nnot json at all password=hunter2hunter2\n";
    let out = redact_bytes(log);
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains("pa55w0rd") && !text.contains("hunter2hunter2"), "{text}");
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    parse(lines[0].as_bytes());
    assert_eq!(parse(lines[1].as_bytes())["msg"], "ok");
}

#[test]
fn stored_reports_and_run_files_remain_parseable() {
    let root = orq_test_support::process_temp_dir().join(format!("orq-redact-json-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("orq.db")).unwrap();
    let report = br#"{"findings":[{"note":"password=\"hunter2hunter2\""}]}"#;
    let saved = store.save_review(&root, "r1", "alpha", "login", SHA, report).unwrap();
    let stored = std::fs::read(root.join(&saved.report_path)).unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("hunter2hunter2"));
    parse(&stored);

    save_run_file(&root, "r1", RunFile::Handoff, br#"{"cmd":"x --token=\"abcd1234efgh\""}"#).unwrap();
    let handoff = std::fs::read(root.join("runs/r1/handoff.json")).unwrap();
    assert!(!String::from_utf8_lossy(&handoff).contains("abcd1234efgh"));
    parse(&handoff);
}
