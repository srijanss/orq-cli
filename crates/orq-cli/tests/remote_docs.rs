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

#[test]
fn the_token_is_created_private_from_the_start_in_the_directory_the_server_reads() {
    let guide = guide();
    let steps: Vec<&str> = guide.lines().map(str::trim).collect();
    let position = |needle: &str| steps.iter().position(|line| line.contains(needle)).unwrap_or_else(|| panic!("the guide shows {needle}"));
    assert!(position("umask 077") < position("> \"$config_dir/remote-token\""), "umask comes before the file is written");
    assert!(guide.contains("${XDG_CONFIG_HOME:-$HOME/.config}/orq"), "the directory follows XDG_CONFIG_HOME like the server");
    assert!(!guide.contains("> ~/.config/orq/remote-token"), "no hard-coded directory is written to");
}

#[test]
fn the_phone_sequence_listens_on_the_tailscale_address() {
    let guide = guide();
    assert!(guide.contains("tailscale ip -4"), "the guide shows how to find the address");
    let listening = guide.lines().map(str::trim).any(|line| line.starts_with("orq remote serve") && line.contains("--listen") && !line.contains("127.0.0.1"));
    assert!(listening, "a documented serve command listens on a non-loopback address");
}
