use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use orq_orchestrator::{notification_message, send_notification, send_notification_with, wants_notification};

fn dir(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-notify-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_message_names_the_feature_how_it_ended_and_the_review_without_secrets_or_control_characters() {
    let ready = notification_message("COM-01", "review_ready", None, Some("https://github.example/o/r/pull/7"));
    assert_eq!(ready, "orq: COM-01 review_ready: https://github.example/o/r/pull/7");

    let failed = notification_message("COM-01", "failed", Some("push to https://user:hunter2@git.example/o/r.git failed\nline two\u{7}"), None);
    assert!(failed.starts_with("orq: COM-01 failed: push to "), "{failed}");
    assert!(!failed.contains("hunter2"), "{failed}");
    assert!(!failed.chars().any(|c| c.is_control()), "{failed:?}");

    let long = notification_message("COM-01", "failed", Some(&"x".repeat(5000)), None);
    assert!(long.chars().count() <= 1000, "{}", long.chars().count());
}

#[test]
fn only_failures_mode_stays_quiet_for_a_review_that_is_ready() {
    for on in [None, Some("all")] {
        for ending in ["review_ready", "blocked", "failed"] {
            assert!(wants_notification(on, ending), "{on:?} {ending}");
        }
    }
    assert!(!wants_notification(Some("failures"), "review_ready"));
    assert!(wants_notification(Some("failures"), "blocked"));
    assert!(wants_notification(Some("failures"), "failed"));
}

#[test]
fn the_command_gets_the_message_in_notify_message_and_never_through_a_shell() {
    let d = dir("run");
    let out = d.join("out.txt");
    let argv = vec!["sh".to_string(), "-c".to_string(), format!("printf %s \"$NOTIFY_MESSAGE\" > {}", out.display())];

    send_notification(&argv, &d, "hello $(not run) `either`", Duration::from_secs(10)).unwrap();

    assert_eq!(fs::read_to_string(&out).unwrap(), "hello $(not run) `either`");
}

#[test]
fn a_command_that_fails_or_hangs_or_is_missing_is_reported_not_waited_on() {
    let d = dir("fail");
    let failing = vec!["sh".to_string(), "-c".to_string(), "exit 3".to_string()];
    assert!(send_notification(&failing, &d, "m", Duration::from_secs(10)).is_err());

    let hanging = vec!["sh".to_string(), "-c".to_string(), "sleep 30".to_string()];
    let started = std::time::Instant::now();
    assert!(send_notification(&hanging, &d, "m", Duration::from_millis(300)).is_err());
    assert!(started.elapsed() < Duration::from_secs(10));

    let missing = vec!["orq-no-such-notifier".to_string()];
    assert!(send_notification(&missing, &d, "m", Duration::from_secs(10)).is_err());
}

#[test]
fn the_notifier_keeps_the_desktop_session_and_proxy_variables_but_not_other_secrets() {
    let d = dir("env");
    let out = d.join("env.txt");
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/orq-test-bus");
    std::env::set_var("ORQ_TEST_UNRELATED_SECRET", "hunter2");
    let argv = vec!["sh".to_string(), "-c".to_string(), format!("printf '%s|%s' \"$DBUS_SESSION_BUS_ADDRESS\" \"$ORQ_TEST_UNRELATED_SECRET\" > {}", out.display())];

    send_notification(&argv, &d, "m", Duration::from_secs(10)).unwrap();

    assert_eq!(fs::read_to_string(&out).unwrap(), "unix:path=/run/orq-test-bus|");
}

#[test]
fn a_notifier_receives_exactly_the_variables_it_is_told_to_keep() {
    let d = dir("named");
    let out = d.join("named.txt");
    std::env::set_var("ORQ_TEST_WEBHOOK_TOKEN", "s3cret");
    std::env::set_var("ORQ_TEST_OTHER_SECRET", "nope");
    let argv = vec!["sh".to_string(), "-c".to_string(), format!("printf '%s|%s' \"$ORQ_TEST_WEBHOOK_TOKEN\" \"$ORQ_TEST_OTHER_SECRET\" > {}", out.display())];

    send_notification_with(&argv, &d, "m", Duration::from_secs(10), &["ORQ_TEST_WEBHOOK_TOKEN".to_string()]).unwrap();

    assert_eq!(fs::read_to_string(&out).unwrap(), "s3cret|");
}
