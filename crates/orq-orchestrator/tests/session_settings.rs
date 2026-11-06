use std::fs;
use std::path::Path;
use std::process::Command;

use orq_orchestrator::{exclude_worktrees_dir, prepare_mcp_servers, setup_mcp_servers, write_session_settings};
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

fn checkout(tag: &str) -> (GitFixture, std::path::PathBuf) {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree(tag);
    (repo, worktree)
}

fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_runs_environment_and_mcp_servers_are_merged_into_the_worktrees_local_session_settings() {
    let (_repo, worktree) = checkout("settings-merge");
    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{"tdd":{"command":"c"},"docs":{"command":"d"}}}"#,
    )
    .unwrap();
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    fs::write(
        worktree.join(".claude/settings.local.json"),
        r#"{"permissions":{"allow":["Bash(ls)"]},"env":{"KEEP":"1"},"enabledMcpjsonServers":["tdd"]}"#,
    )
    .unwrap();

    let notes = write_session_settings(&worktree, &vars(&[("DB_NAME", "secretdb_run1")]), None, None);

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(
        settings["permissions"]["allow"],
        serde_json::json!(["Bash(ls)"])
    );
    assert_eq!(
        settings["env"],
        serde_json::json!({"KEEP": "1", "DB_NAME": "secretdb_run1"})
    );
    assert_eq!(
        settings["enabledMcpjsonServers"],
        serde_json::json!(["tdd", "docs"])
    );
    assert!(
        !git_out(&worktree, &["status", "--porcelain"]).contains("settings.local.json"),
        "must stay out of git status"
    );
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &fs::metadata(worktree.join(".claude/settings.local.json"))
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600, "owner-only, existing file included");
    assert!(
        notes.iter().all(|n| !n.contains("secretdb_run1")),
        "values are never logged: {notes:?}"
    );
}

#[test]
fn only_the_configured_servers_are_approved_and_a_problem_is_a_note_not_a_failure() {
    let (_repo, worktree) = checkout("settings-notes");
    let notes = write_session_settings(&worktree, &[], Some(&["tdd".to_string()]), None);
    assert!(
        notes.iter().any(|n| n.contains(".mcp.json")),
        "no .mcp.json: {notes:?}"
    );

    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{"tdd":{"command":"c"},"docs":{"command":"d"}}}"#,
    )
    .unwrap();
    let notes = write_session_settings(
        &worktree,
        &[],
        Some(&["tdd".to_string(), "absent".to_string()]),
        None,
    );
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(
        settings["enabledMcpjsonServers"],
        serde_json::json!(["tdd"])
    );
    assert!(notes.iter().any(|n| n.contains("absent")), "{notes:?}");

    let (_other_repo, blocked) = checkout("settings-blocked");
    fs::create_dir_all(blocked.join(".claude/settings.local.json")).unwrap();
    let notes = write_session_settings(&blocked, &vars(&[("A", "1")]), None, None);
    assert!(
        notes.iter().any(|n| n.contains("settings.local.json")),
        "{notes:?}"
    );
}

#[test]
fn mcpctl_sets_up_all_configured_servers_in_one_call_and_a_missing_or_failing_tool_is_a_note() {
    // `mcpctl setup` replaces the project's selection, so one call per server would keep only the
    // last one, and the others would run at the machine's active version.
    let (repo, worktree) = checkout("mcpctl-setup");
    let seen = repo
        .root()
        .parent()
        .unwrap()
        .join(format!("mcpctl-seen-{}", std::process::id()));
    let tool = repo
        .root()
        .parent()
        .unwrap()
        .join(format!("fake-mcpctl-{}", std::process::id()));
    fs::write(
        &tool,
        format!(
            "#!/bin/sh\necho \"$PWD $* $DB_NAME\" >> '{}'\ncase \"$*\" in *bad*) exit 3;; esac\nexit 0\n",
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let servers = vec!["tdd".to_string(), "project".to_string()];

    let notes = setup_mcp_servers(
        &worktree,
        &servers,
        &vars(&[("DB_NAME", "run1")]),
        Some(&tool),
    );

    let log = fs::read_to_string(&seen).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].ends_with("setup --mcp tdd --mcp project run1"), "{log}");
    assert!(
        lines[0].starts_with(&worktree.display().to_string()),
        "run inside the worktree: {log}"
    );
    assert!(notes.is_empty(), "{notes:?}");

    let failing = vec!["tdd".to_string(), "bad".to_string()];
    let notes = setup_mcp_servers(&worktree, &failing, &[], Some(&tool));
    assert!(notes.iter().any(|n| n.contains("tdd") && n.contains("bad")), "{notes:?}");

    let again = vec!["tdd".to_string(), "tdd".to_string()];
    setup_mcp_servers(&worktree, &again, &[], Some(&tool));
    let log = fs::read_to_string(&seen).unwrap();
    assert!(
        log.lines().last().unwrap().ends_with("setup --mcp tdd "),
        "a repeated name is set up once: {log}"
    );

    let missing = setup_mcp_servers(&worktree, &servers, &[], None);
    assert!(missing.iter().any(|n| n.contains("mcpctl")), "{missing:?}");
    assert!(
        setup_mcp_servers(&worktree, &[], &[], None).is_empty(),
        "nothing configured, nothing said"
    );
}

#[test]
fn prepared_servers_come_from_the_setting_or_else_the_worktrees_mcpctl_toml_and_are_approved_after_setup() {
    let (repo, worktree) = checkout("mcpctl-prepare");
    let seen = repo.root().parent().unwrap().join(format!("mcpctl-prepare-seen-{}", std::process::id()));
    let tool = repo.root().parent().unwrap().join(format!("fake-mcpctl-prepare-{}", std::process::id()));
    fs::write(
        &tool,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\necho '{{\"mcpServers\":{{\"tdd\":{{\"command\":\"c\"}}}}}}' > .mcp.json\n",
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let log = || fs::read_to_string(&seen).unwrap_or_default();

    // Nothing selected anywhere: no setup.
    prepare_mcp_servers(&worktree, &[], None, Some(&tool));
    assert_eq!(log(), "");

    // A name that would read as an option is not passed on.
    let toml = worktree.join(".mcpctl.toml");
    fs::write(&toml, "[[mcp]]\nname = \"tdd\"\n\n[[mcp]]\nname = \"--evil\"\n\n[[mcp]]\nname = \"project\"\n").unwrap();
    let notes = prepare_mcp_servers(&worktree, &[], None, Some(&tool));
    assert_eq!(log(), "setup --mcp tdd --mcp project\n");
    assert!(notes.iter().any(|n| n.contains(".mcpctl.toml")), "{notes:?}");
    let approved = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(approved["enabledMcpjsonServers"], serde_json::json!(["tdd"]), "what setup generated is approved");

    // The setting wins over the file.
    let chosen = vec!["tdd".to_string()];
    prepare_mcp_servers(&worktree, &[], Some(&chosen), Some(&tool));
    assert_eq!(log().lines().last(), Some("setup --mcp tdd"));

    // An unreadable selection is a note and no setup.
    let before = log();
    fs::write(&toml, "[[mcp]\n").unwrap();
    let notes = prepare_mcp_servers(&worktree, &[], None, Some(&tool));
    assert_eq!(log(), before);
    assert!(notes.iter().any(|n| n.contains(".mcpctl.toml")), "{notes:?}");
}

#[test]
fn a_symlinked_mcpctl_toml_is_neither_read_nor_set_up_whatever_the_setting() {
    let (repo, worktree) = checkout("mcpctl-symlink");
    let dir = repo.root().parent().unwrap();
    let seen = dir.join(format!("mcpctl-symlink-seen-{}", std::process::id()));
    let tool = dir.join(format!("fake-mcpctl-symlink-{}", std::process::id()));
    fs::write(&tool, format!("#!/bin/sh\necho \"$*\" >> '{}'\n", seen.display())).unwrap();
    fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let outside = dir.join(format!("outside-mcpctl-{}.toml", std::process::id()));
    fs::write(&outside, "[[mcp]]\nname = \"from-outside\"\n").unwrap();
    std::os::unix::fs::symlink(&outside, worktree.join(".mcpctl.toml")).unwrap();

    let notes = prepare_mcp_servers(&worktree, &vars(&[("KEEP", "1")]), None, Some(&tool));
    assert!(!seen.exists(), "the outside file selected servers: {}", fs::read_to_string(&seen).unwrap_or_default());
    assert!(notes.iter().any(|n| n.contains(".mcpctl.toml") && n.contains("symbolic link")), "{notes:?}");

    // Setup rewrites the file, so an explicit setting must not write through the link either.
    let chosen = vec!["tdd".to_string()];
    let notes = prepare_mcp_servers(&worktree, &[], Some(&chosen), Some(&tool));
    assert!(!seen.exists(), "setup ran through the link");
    assert!(notes.iter().any(|n| n.contains("symbolic link")), "{notes:?}");

    // The run's environment still reaches the session.
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["env"]["KEEP"], "1");
}

#[test]
fn the_run_worktrees_directory_is_kept_out_of_the_main_checkouts_git_status_through_the_local_exclude_only(
) {
    let repo = GitFixture::new();
    fs::create_dir_all(repo.root().join(".claude/worktrees/run-1")).unwrap();
    fs::write(repo.root().join(".claude/worktrees/run-1/file"), "x").unwrap();
    assert!(
        git_out(repo.root(), &["status", "--porcelain"]).contains(".claude"),
        "premise"
    );

    let first = exclude_worktrees_dir(repo.root());
    let second = exclude_worktrees_dir(repo.root());

    assert!(first.is_none() && second.is_none(), "{first:?} {second:?}");
    assert!(!git_out(repo.root(), &["status", "--porcelain"]).contains(".claude/worktrees"));
    assert!(
        !repo.root().join(".gitignore").exists(),
        "a tracked file is never edited"
    );
    let exclude = fs::read_to_string(repo.root().join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches(".claude/worktrees").count(), 1, "{exclude}");
}
