//! The phone guide must only name commands and settings the code accepts.

use std::collections::BTreeSet;
use std::process::Command;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn guide() -> String {
    std::fs::read_to_string(root().join("docs/remote-inbox.md")).expect("docs/remote-inbox.md exists")
}

/// The usage line `orq` prints for an unknown command.
fn usage() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_orq")).arg("no-such-command").output().unwrap();
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn the_guide_is_linked_from_the_readme() {
    let readme = std::fs::read_to_string(root().join("README.md")).unwrap();
    assert!(readme.contains("docs/remote-inbox.md"));
}

#[test]
fn every_flag_on_a_documented_serve_command_is_one_the_command_accepts() {
    let guide = guide();
    let usage = usage();
    let commands: Vec<&str> = guide.lines().map(str::trim).filter(|line| line.starts_with("orq remote serve")).collect();
    assert!(!commands.is_empty(), "the guide shows the serve command");
    let serve_usage = usage.split(" | ").find(|part| part.contains("orq remote serve")).expect("usage lists remote serve");
    for command in commands {
        let flags: BTreeSet<&str> = command.split_whitespace().filter(|word| word.starts_with("--")).collect();
        assert!(flags.contains("--repo"), "{command}");
        for flag in flags {
            assert!(serve_usage.contains(flag), "{flag} is not accepted by `orq remote serve`: {serve_usage}");
        }
    }
}

#[test]
fn the_documented_settings_and_files_exist() {
    let guide = guide();
    let settings = std::fs::read_to_string(root().join("crates/orq-orchestrator/src/run_settings.rs")).unwrap();
    for name in ["remote_control", "ORQ_REMOTE_CONTROL"] {
        assert!(guide.contains(name), "the guide names {name}");
        assert!(settings.contains(&format!("\"{name}\"")), "{name} is not a setting");
    }
    let inbox = std::fs::read_to_string(root().join("crates/orq-core/src/remote_inbox.rs")).unwrap();
    assert!(inbox.contains("\"remote-token\"") && guide.contains("remote-token"));
    for topic in ["chmod 600", "Tailscale", "rotate", "read-only"] {
        assert!(guide.contains(topic), "the guide covers {topic}");
    }
}
