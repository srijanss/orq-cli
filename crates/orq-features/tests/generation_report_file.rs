use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use orq_features::{generation_log_dir, write_generation_report, GenerationReport};

fn dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orq-genrep-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn report() -> GenerationReport {
    GenerationReport {
        harness: "claude".into(),
        model: "sonnet".into(),
        spec: "docs/spec.md".into(),
        focus: "MVP2".into(),
        program: "/home/dev/.local/bin/claude".into(),
        argv: vec!["--model".into(), "sonnet".into(), "-p".into(), "<prompt elided>".into()],
        cwd: "/repo".into(),
        exit: "exit status 1".into(),
        duration: Duration::from_millis(1500),
        stderr_tail: "ANTHROPIC_API_KEY=sk-ant-abc123def456ghi789 failed\nNot logged in".into(),
        stdout_head: "partial".into(),
        error: "the agent exited with status 1".into(),
    }
}

#[test]
fn the_log_dir_follows_xdg_state_then_home() {
    assert_eq!(
        generation_log_dir(Some("/state".into()), Some("/home/u".into())),
        Some(PathBuf::from("/state/orq/logs"))
    );
    assert_eq!(
        generation_log_dir(None, Some("/home/u".into())),
        Some(PathBuf::from("/home/u/.local/state/orq/logs"))
    );
    assert_eq!(
        generation_log_dir(Some("relative".into()), Some("/home/u".into())),
        Some(PathBuf::from("/home/u/.local/state/orq/logs"))
    );
    assert_eq!(generation_log_dir(None, None), None);
}

#[test]
fn the_report_is_owner_only_named_by_timestamp_and_complete() {
    let logs = dir("write");

    let path = write_generation_report(&logs, &report()).unwrap();

    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("generation-") && name.ends_with(".log"), "{name}");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let text = fs::read_to_string(&path).unwrap();
    for needle in [
        "claude", "sonnet", "docs/spec.md", "MVP2", "/home/dev/.local/bin/claude", "<prompt elided>", "/repo",
        "exit status 1", "1.5", "Not logged in", "partial", "the agent exited with status 1",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
}

#[test]
fn secrets_never_reach_the_file() {
    let logs = dir("redact");

    let path = write_generation_report(&logs, &report()).unwrap();

    let text = fs::read_to_string(path).unwrap();
    assert!(!text.contains("sk-ant-abc123def456ghi789"), "{text}");
    assert!(text.contains("[REDACTED]"));
}

#[test]
fn two_reports_never_overwrite_each_other() {
    let logs = dir("unique");

    let a = write_generation_report(&logs, &report()).unwrap();
    let b = write_generation_report(&logs, &report()).unwrap();

    assert_ne!(a, b);
}

#[test]
fn oversized_fields_are_bounded() {
    let logs = dir("bounded");
    let mut big = report();
    big.stderr_tail = "y".repeat(1_000_000);
    big.stdout_head = "z".repeat(1_000_000);

    let path = write_generation_report(&logs, &big).unwrap();

    assert!(fs::metadata(path).unwrap().len() < 64 * 1024);
}
