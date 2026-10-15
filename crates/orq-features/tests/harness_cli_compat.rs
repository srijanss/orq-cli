//! Pins the read-only flags to what the installed harness CLIs document, so a
//! renamed or removed flag fails here instead of silently widening access.
//! Skipped for a harness that is not installed.
use std::process::Command;

fn help(program: &str) -> Option<String> {
    let out = Command::new(program).arg("--help").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
}

#[test]
fn the_installed_claude_documents_every_read_only_flag_and_the_plan_mode() {
    let Some(help) = help("claude") else { return };
    for needle in ["--model", "--tools", "--permission-mode", "--strict-mcp-config", "--disallowedTools", "\"plan\""] {
        assert!(help.contains(needle), "claude --help no longer mentions {needle}");
    }
}

#[test]
fn the_installed_pi_documents_every_read_only_flag() {
    let Some(help) = help("pi") else { return };
    for needle in ["--model", "--tools", "--no-mcp", "--no-extensions", "--no-context-files", "--no-session"] {
        assert!(help.contains(needle), "pi --help no longer mentions {needle}");
    }
}
