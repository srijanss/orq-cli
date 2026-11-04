use std::fs;
use std::path::Path;
use std::process::Command;

use orq_orchestrator::carry_local_files;
use orq_test_support::GitFixture;

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A main checkout and a linked worktree of it.
fn checkout(tag: &str) -> (GitFixture, std::path::PathBuf) {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree(tag);
    (repo, worktree)
}

fn pairs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[test]
fn the_main_checkouts_dotenv_file_is_linked_into_the_worktree_and_kept_out_of_git_status() {
    let (repo, worktree) = checkout("dotenv-link");
    fs::write(repo.root().join(".env"), "DB_NAME=shop\n").unwrap();

    let found = carry_local_files(repo.root(), &worktree, None);

    assert_eq!(fs::read_to_string(worktree.join(".env")).unwrap(), "DB_NAME=shop\n");
    assert!(fs::symlink_metadata(worktree.join(".env")).unwrap().file_type().is_symlink());
    assert!(!git_out(&worktree, &["status", "--porcelain"]).contains(".env"), "must not show as untracked");
    assert!(found.notes.iter().all(|n| !n.contains(".env")), "{:?}", found.notes);
}

#[test]
fn a_dotenv_file_the_worktree_already_has_is_left_alone_and_a_missing_one_is_noted() {
    let (repo, worktree) = checkout("dotenv-own");
    fs::write(worktree.join(".env"), "OWN=1\n").unwrap();
    fs::write(repo.root().join(".env"), "MAIN=1\n").unwrap();
    carry_local_files(repo.root(), &worktree, None);
    assert_eq!(fs::read_to_string(worktree.join(".env")).unwrap(), "OWN=1\n");

    let (bare_repo, bare_worktree) = checkout("dotenv-none");
    let found = carry_local_files(bare_repo.root(), &bare_worktree, None);
    assert!(found.notes.iter().any(|n| n.contains(".env")), "{:?}", found.notes);
    assert!(!bare_worktree.join(".env").exists());
}

#[test]
fn the_tdd_server_is_pointed_at_the_main_checkouts_files_unless_the_worktree_has_its_own() {
    let (repo, worktree) = checkout("tdd-paths");
    fs::write(repo.root().join(".tdd-config.json"), "{}").unwrap();
    fs::write(repo.root().join(".tdd-features.json"), "{}").unwrap();
    fs::write(worktree.join(".tdd-features.json"), "{}").unwrap();

    let found = carry_local_files(repo.root(), &worktree, None);

    let config = repo.root().join(".tdd-config.json").display().to_string();
    assert_eq!(pairs(&found.env), [("TDD_CONFIG_PATH", config.as_str())]);
    assert!(!worktree.join(".tdd-config.json").exists(), "the files are pointed at, not copied");
}

#[test]
fn mcp_json_is_copied_when_missing_and_merged_per_server_when_the_worktree_generated_its_own() {
    let (repo, worktree) = checkout("mcp");
    fs::write(
        repo.root().join(".mcp.json"),
        r#"{"mcpServers":{"tdd":{"command":"main-cmd","args":["m"],"env":{"TOKEN":"t","SHARED":"main"}}}}"#,
    )
    .unwrap();
    carry_local_files(repo.root(), &worktree, None);
    assert_eq!(fs::read_to_string(worktree.join(".mcp.json")).unwrap(), fs::read_to_string(repo.root().join(".mcp.json")).unwrap());

    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{"tdd":{"command":"wt-cmd","args":["w"],"env":{"SHARED":"wt","KEEP":"k"}},"other":{"command":"x"}}}"#,
    )
    .unwrap();
    carry_local_files(repo.root(), &worktree, None);

    let merged: serde_json::Value = serde_json::from_str(&fs::read_to_string(worktree.join(".mcp.json")).unwrap()).unwrap();
    let tdd = &merged["mcpServers"]["tdd"];
    assert_eq!(tdd["command"], "wt-cmd");
    assert_eq!(tdd["args"], serde_json::json!(["w"]));
    assert_eq!(tdd["env"], serde_json::json!({"TOKEN": "t", "SHARED": "main", "KEEP": "k"}));
    assert_eq!(merged["mcpServers"]["other"]["command"], "x");
}

#[test]
fn mcpctl_config_is_copied_and_its_lock_file_is_kept_out_of_git_status() {
    let (repo, worktree) = checkout("mcpctl");
    fs::write(repo.root().join(".mcpctl.toml"), "[servers]\n").unwrap();

    carry_local_files(repo.root(), &worktree, None);
    fs::write(worktree.join(".mcpctl.lock"), "").unwrap();

    assert_eq!(fs::read_to_string(worktree.join(".mcpctl.toml")).unwrap(), "[servers]\n");
    let status = git_out(&worktree, &["status", "--porcelain"]);
    assert!(!status.contains(".mcpctl"), "{status}");
}

#[test]
fn the_list_of_carried_files_is_configurable_and_only_plain_relative_paths_are_accepted() {
    let (repo, worktree) = checkout("custom");
    fs::write(repo.root().join("secrets.json"), "{\"k\":1}").unwrap();
    fs::write(repo.root().join(".env"), "A=1\n").unwrap();
    let files = vec!["secrets.json".to_string(), "../outside".to_string(), "/etc/passwd".to_string()];

    let found = carry_local_files(repo.root(), &worktree, Some(&files));

    assert_eq!(fs::read_to_string(worktree.join("secrets.json")).unwrap(), "{\"k\":1}");
    assert!(!worktree.join(".env").exists(), "a configured list replaces the default one");
    assert_eq!(found.notes.iter().filter(|n| n.contains("not a plain relative path")).count(), 2, "{:?}", found.notes);
    assert!(!worktree.parent().unwrap().join("outside").exists());
}
