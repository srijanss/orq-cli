//! When a web Start or Resume fails for a reason the browser is not told, the server's own log gets it:
//! one line naming the action, project and feature or run, with the reason escaped and bounded.

use orq_core::remote_inbox::launch_failure_line;

#[test]
fn a_failed_launch_is_one_log_line_naming_the_action_target_and_the_real_reason() {
    assert_eq!(
        launch_failure_line("run.start", "orq-cli", "ready", "cannot start orq run: No such file or directory (os error 2)"),
        "orq-remote: run.start orq-cli ready failed: cannot start orq run: No such file or directory (os error 2)"
    );
}

#[test]
fn a_reason_with_line_breaks_or_control_characters_stays_on_one_escaped_line() {
    let line = launch_failure_line("run.resume", "p", "run-1", "first\nforged: line\r\u{1b}[31mred");
    assert_eq!(line, "orq-remote: run.resume p run-1 failed: first\\nforged: line\\r\\u{1b}[31mred");
    assert_eq!(line.lines().count(), 1);
}

#[test]
fn a_very_long_reason_is_cut_short() {
    let line = launch_failure_line("run.start", "p", "f", &"x".repeat(10_000));
    assert!(line.len() < 1_200, "{}", line.len());
    assert!(line.ends_with('…'), "{line}");
}
