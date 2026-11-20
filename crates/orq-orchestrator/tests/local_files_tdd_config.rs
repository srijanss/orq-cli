//! The project's own TDD config wins: it is carried into the worktree before MCP setup scaffolds a
//! template there, and every session is pointed at the main checkout's TDD files.

use std::fs;
use std::path::Path;
use std::process::Command;

use orq_orchestrator::{carry_local_files, run_mcp_setup, McpSetupRequest, McpTimeouts};
use orq_test_support::GitFixture;

const CARGO_CONFIG: &str = "{\n  \"adapter\": \"cargo-adapter\",\n  \"defaultTestDir\": \"crates\",\n  \"testCommand\": \"cargo test\"\n}\n";
const PYTEST_CONFIG: &str = "{\n  \"adapter\": \"pytest-adapter\",\n  \"defaultTestDir\": \"tests\",\n  \"testCommand\": \"cargo test\"\n}\n";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn env_value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[test]
fn the_main_checkouts_config_is_carried_in_before_setup_so_the_scaffold_keeps_it() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("carried");
    fs::write(repo.root().join(".tdd-config.json"), CARGO_CONFIG).unwrap();
    let dir = repo
        .root()
        .parent()
        .unwrap()
        .join(format!("tdd-config-shim-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let tool = dir.join("mcpctl");
    // Like mcpctl init: the template config is written only where the project has none.
    fs::write(&tool, "#!/bin/sh\n[ -e .tdd-config.json ] || echo '{\"adapter\":\"pytest-adapter\"}' > .tdd-config.json\nexit 0\n").unwrap();
    fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let found = carry_local_files(repo.root(), &worktree, None);
    let servers = vec!["tdd".to_string()];
    run_mcp_setup(&McpSetupRequest {
        main_checkout: repo.root(),
        worktree: &worktree,
        servers: &servers,
        env: &found.env,
        mcpctl: Some(&tool),
        log: &dir.join("mcp-setup.log"),
        timeouts: McpTimeouts::default(),
    });

    assert_eq!(
        fs::read_to_string(worktree.join(".tdd-config.json")).unwrap(),
        CARGO_CONFIG
    );
    assert_eq!(
        git(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        "",
        "the copy is kept out of git status"
    );
    let config = repo.root().join(".tdd-config.json").display().to_string();
    assert_eq!(
        env_value(&found.env, "TDD_CONFIG_PATH"),
        Some(config.as_str())
    );
    assert!(
        found.notes.iter().all(|n| !n.contains(".tdd-config.json")),
        "{:?}",
        found.notes
    );
}

#[test]
fn an_ignored_worktree_copy_that_differs_is_named_by_its_keys_only_and_every_later_session_still_reads_the_main_one(
) {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("differs");
    fs::write(repo.root().join(".tdd-config.json"), CARGO_CONFIG).unwrap();
    fs::write(repo.root().join(".tdd-features.json"), "{}").unwrap();
    fs::write(worktree.join(".tdd-config.json"), PYTEST_CONFIG).unwrap();
    fs::write(worktree.join(".tdd-features.json"), "{}").unwrap();

    let first = carry_local_files(repo.root(), &worktree, None);
    let later = carry_local_files(repo.root(), &worktree, None);

    let config = repo.root().join(".tdd-config.json").display().to_string();
    let features = repo.root().join(".tdd-features.json").display().to_string();
    for found in [&first, &later] {
        assert_eq!(
            env_value(&found.env, "TDD_CONFIG_PATH"),
            Some(config.as_str())
        );
        assert_eq!(
            env_value(&found.env, "TDD_FEATURES_PATH"),
            Some(features.as_str())
        );
    }
    let note = first
        .notes
        .iter()
        .find(|n| n.contains(".tdd-config.json"))
        .expect("a note on the differing copy");
    assert!(
        note.contains("adapter") && note.contains("defaultTestDir"),
        "{note}"
    );
    assert!(
        !note.contains("testCommand"),
        "an equal key is not named: {note}"
    );
    for value in ["cargo-adapter", "pytest-adapter", "crates", "tests"] {
        assert!(
            !note.contains(&format!("\"{value}\"")) && !note.contains(&format!(" {value}")),
            "no values: {note}"
        );
    }
    assert_eq!(
        fs::read_to_string(worktree.join(".tdd-config.json")).unwrap(),
        PYTEST_CONFIG,
        "the worktree's copy is not rewritten"
    );
}

#[test]
fn a_config_the_branch_tracks_is_its_own_and_gets_no_override_and_no_note() {
    let repo = GitFixture::new();
    fs::write(repo.root().join(".tdd-config.json"), PYTEST_CONFIG).unwrap();
    git(repo.root(), &["add", ".tdd-config.json"]);
    git(
        repo.root(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "config",
        ],
    );
    let worktree = repo.add_linked_worktree("tracked");
    fs::write(repo.root().join(".tdd-config.json"), CARGO_CONFIG).unwrap();

    let found = carry_local_files(repo.root(), &worktree, None);

    assert_eq!(env_value(&found.env, "TDD_CONFIG_PATH"), None);
    assert!(
        found.notes.iter().all(|n| !n.contains(".tdd-config.json")),
        "{:?}",
        found.notes
    );
    assert_eq!(
        fs::read_to_string(worktree.join(".tdd-config.json")).unwrap(),
        PYTEST_CONFIG
    );
}
