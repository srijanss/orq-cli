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
    assert!(position("umask 077") < position("> \"$tmp\""), "umask comes before the file is written");
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

#[test]
fn the_token_is_written_to_a_fresh_file_and_renamed_over_the_old_one() {
    let guide = guide();
    let lines: Vec<&str> = guide.lines().map(str::trim).collect();
    let at = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap_or_else(|| panic!("the guide shows {needle}"));
    assert!(at("mktemp") < at("mv -f"), "a fresh file is made before it replaces the token");
    assert!(lines[at("mktemp")].contains("$config_dir/"), "the fresh file is in the config directory");
    assert!(!guide.contains("> \"$config_dir/remote-token\""), "the token is never written through the existing path");
}

/// The `sh` block in the guide that creates the token, as the reader would run it.
fn token_snippet() -> String {
    let guide = guide();
    let block = guide.split("```sh\n").skip(1).map(|rest| rest.split("```").next().unwrap()).find(|block| block.contains("remote-token")).expect("the guide shows the token commands");
    block.to_string()
}

fn run_snippet(root: &std::path::Path, path_prefix: Option<&std::path::Path>) -> std::process::Output {
    let mut command = Command::new("sh");
    command.arg("-c").arg(token_snippet());
    command.env("XDG_CONFIG_HOME", root.join("config"));
    command.env("HOME", root.join("home"));
    if let Some(prefix) = path_prefix {
        command.env("PATH", format!("{}:{}", prefix.display(), std::env::var("PATH").unwrap()));
    }
    command.output().unwrap()
}

#[test]
fn the_documented_token_commands_replace_unsafe_tokens_and_keep_a_good_one_when_generation_fails() {
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    let base = orq_test_support::process_temp_dir().join(format!("orq-docs-token-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let fresh = |name: &str| {
        let root = base.join(name);
        std::fs::create_dir_all(root.join("config/orq")).unwrap();
        root
    };
    let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().mode() & 0o777;
    let is_regular_private_token = |path: &std::path::Path| {
        let meta = std::fs::symlink_metadata(path).unwrap();
        meta.file_type().is_file() && mode(path) == 0o600 && std::fs::read_to_string(path).unwrap().len() >= 32
    };

    // An existing token file that anyone can read.
    let root = fresh("loose");
    let token = root.join("config/orq/remote-token");
    std::fs::write(&token, "old").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(run_snippet(&root, None).status.success());
    assert!(is_regular_private_token(&token));

    // A symlink to a file and a symlink to a directory: replaced, targets untouched.
    let root = fresh("link-file");
    let (token, target) = (root.join("config/orq/remote-token"), root.join("elsewhere"));
    std::fs::write(&target, "keep").unwrap();
    symlink(&target, &token).unwrap();
    assert!(run_snippet(&root, None).status.success());
    assert!(is_regular_private_token(&token));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");

    let root = fresh("link-dir");
    let (token, target) = (root.join("config/orq/remote-token"), root.join("dir"));
    std::fs::create_dir_all(&target).unwrap();
    symlink(&target, &token).unwrap();
    assert!(run_snippet(&root, None).status.success());
    assert!(is_regular_private_token(&token));
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0, "nothing was moved into the linked directory");

    // A failing generator leaves a working token alone and no stray files.
    let root = fresh("failing");
    let token = root.join("config/orq/remote-token");
    std::fs::write(&token, "a-working-token-0123456789abcdef0123456789").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let fake = root.join("fakebin");
    std::fs::create_dir_all(&fake).unwrap();
    std::fs::write(fake.join("head"), "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(fake.join("head"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!run_snippet(&root, Some(&fake)).status.success(), "a failed generation is reported");
    assert_eq!(std::fs::read_to_string(&token).unwrap(), "a-working-token-0123456789abcdef0123456789");
    assert_eq!(std::fs::read_dir(root.join("config/orq")).unwrap().count(), 1, "no temporary file is left");
}

#[test]
fn a_generator_that_prints_then_fails_or_a_directory_at_the_token_path_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let base = orq_test_support::process_temp_dir().join(format!("orq-docs-token-edge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let fresh = |name: &str| {
        let root = base.join(name);
        std::fs::create_dir_all(root.join("config/orq")).unwrap();
        root
    };

    // head prints plenty and then fails: the working token stays.
    let root = fresh("noisy-failure");
    let token = root.join("config/orq/remote-token");
    std::fs::write(&token, "a-working-token-0123456789abcdef0123456789").unwrap();
    let fake = root.join("fakebin");
    std::fs::create_dir_all(&fake).unwrap();
    std::fs::write(fake.join("head"), "#!/bin/sh\nprintf '%064d' 0\nexit 1\n").unwrap();
    std::fs::set_permissions(fake.join("head"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!run_snippet(&root, Some(&fake)).status.success());
    assert_eq!(std::fs::read_to_string(&token).unwrap(), "a-working-token-0123456789abcdef0123456789");
    assert_eq!(std::fs::read_dir(root.join("config/orq")).unwrap().count(), 1, "no temporary file is left");

    // A real directory at the token path: refused, nothing moved into it, nothing left behind.
    let root = fresh("directory");
    let token = root.join("config/orq/remote-token");
    std::fs::create_dir_all(&token).unwrap();
    assert!(!run_snippet(&root, None).status.success());
    assert_eq!(std::fs::read_dir(&token).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(root.join("config/orq")).unwrap().count(), 1, "no temporary file is left");
}
