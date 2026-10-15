use std::fs;
use std::time::Duration;

use orq_features::{write_generation_report, GenerationReport};

fn written(argv: &[&str], label: &str) -> String {
    let dir = std::env::temp_dir().join(format!("orq-genargv-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let report = GenerationReport {
        harness: "h".into(),
        model: "m".into(),
        spec: "s.md".into(),
        focus: String::new(),
        program: "/bin/agent".into(),
        argv: argv.iter().map(|a| a.to_string()).collect(),
        cwd: "/repo".into(),
        exit: "exit status 1".into(),
        duration: Duration::ZERO,
        stderr_tail: String::new(),
        stdout_head: String::new(),
        error: "failed".into(),
    };
    let path = write_generation_report(&dir, &report).unwrap();
    let text = fs::read_to_string(path).unwrap();
    let _ = fs::remove_dir_all(&dir);
    text
}

#[test]
fn a_secret_in_the_element_after_a_credential_flag_is_redacted() {
    let text = written(&["--api-key", "short-secret", "--model", "sonnet"], "flag");

    assert!(!text.contains("short-secret"), "{text}");
    assert!(text.contains("--api-key"), "{text}");
    assert!(text.contains("sonnet"), "{text}");
}

#[test]
fn equals_form_flags_and_other_flags_are_handled() {
    let text = written(&["--token=abc", "--password", "p4ss", "--verbose", "visible"], "forms");

    assert!(!text.contains("abc"), "{text}");
    assert!(!text.contains("p4ss"), "{text}");
    assert!(text.contains("visible"), "{text}");
}

#[test]
fn a_hyphen_prefixed_credential_value_is_redacted() {
    let text = written(&["--password", "-short-secret", "--model", "sonnet"], "hyphen");

    assert!(!text.contains("short-secret"), "{text}");
    assert!(text.contains("sonnet"), "{text}");
}
