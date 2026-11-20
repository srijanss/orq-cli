use std::fs;
use std::path::Path;
use std::process::Command;

use orq_orchestrator::{
    exclude_worktrees_dir, prepare_mcp_servers, setup_mcp_servers, write_session_settings,
};
use orq_test_support::{write_executable, GitFixture};

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

/// The fake mcpctl's log, read only after the notes show it could be run: a script that could not
/// be started fails here on the note naming why, not later on a missing log file.
fn read_mcpctl_log(seen: &Path, notes: &[String]) -> String {
    let unrunnable: Vec<&String> = notes
        .iter()
        .filter(|note| note.contains("could not be started"))
        .collect();
    assert!(
        unrunnable.is_empty(),
        "the fake mcpctl did not run: {unrunnable:?}"
    );
    fs::read_to_string(seen)
        .unwrap_or_else(|err| panic!("read the fake mcpctl's log {}: {err}", seen.display()))
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

    let notes = write_session_settings(
        &worktree,
        &vars(&[("DB_NAME", "secretdb_run1")]),
        None,
        None,
    );

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
    write_executable(
        &tool,
        &format!(
            "#!/bin/sh\necho \"$PWD $* $DB_NAME\" >> '{}'\ncase \"$*\" in *bad*) exit 3;; esac\nexit 0\n",
            seen.display()
        ),
    );
    let servers = vec!["tdd".to_string(), "project".to_string()];

    let notes = setup_mcp_servers(
        &worktree,
        &servers,
        &vars(&[("DB_NAME", "run1")]),
        Some(&tool),
    );

    assert!(notes.is_empty(), "{notes:?}");
    let log = read_mcpctl_log(&seen, &notes);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(
        lines[0].ends_with("setup --mcp tdd --mcp project run1"),
        "{log}"
    );
    assert!(
        lines[0].starts_with(&worktree.display().to_string()),
        "run inside the worktree: {log}"
    );

    let failing = vec!["tdd".to_string(), "bad".to_string()];
    let notes = setup_mcp_servers(&worktree, &failing, &[], Some(&tool));
    assert!(
        notes.iter().any(|n| n.contains("tdd") && n.contains("bad")),
        "{notes:?}"
    );

    let again = vec!["tdd".to_string(), "tdd".to_string()];
    let notes = setup_mcp_servers(&worktree, &again, &[], Some(&tool));
    let log = read_mcpctl_log(&seen, &notes);
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
#[should_panic(expected = "could not be started")]
fn a_fake_mcpctl_that_could_not_be_run_fails_on_the_note_naming_why_not_on_a_missing_log() {
    let (repo, worktree) = checkout("mcpctl-unrunnable");
    let dir = repo.root().parent().unwrap();
    let seen = dir.join(format!("mcpctl-unrunnable-seen-{}", std::process::id()));
    let tool = dir.join(format!("fake-mcpctl-unrunnable-{}", std::process::id()));
    write_executable(
        &tool,
        &format!("#!/bin/sh\necho \"$*\" >> '{}'\n", seen.display()),
    );
    fs::remove_file(&tool).unwrap();

    let notes = setup_mcp_servers(&worktree, &["tdd".to_string()], &[], Some(&tool));

    read_mcpctl_log(&seen, &notes);
}

#[test]
fn prepared_servers_come_from_the_setting_or_else_the_worktrees_mcpctl_toml_and_are_approved_after_setup(
) {
    let (repo, worktree) = checkout("mcpctl-prepare");
    let seen = repo
        .root()
        .parent()
        .unwrap()
        .join(format!("mcpctl-prepare-seen-{}", std::process::id()));
    let tool = repo
        .root()
        .parent()
        .unwrap()
        .join(format!("fake-mcpctl-prepare-{}", std::process::id()));
    write_executable(
        &tool,
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\necho '{{\"mcpServers\":{{\"tdd\":{{\"command\":\"c\"}}}}}}' > .mcp.json\n",
            seen.display()
        ),
    );
    let log = || fs::read_to_string(&seen).unwrap_or_default();

    // Nothing selected anywhere: no setup.
    prepare_mcp_servers(&worktree, &[], None, Some(&tool));
    assert_eq!(log(), "");

    // A name that would read as an option is not passed on.
    let toml = worktree.join(".mcpctl.toml");
    fs::write(
        &toml,
        "[[mcp]]\nname = \"tdd\"\n\n[[mcp]]\nname = \"--evil\"\n\n[[mcp]]\nname = \"project\"\n",
    )
    .unwrap();
    let notes = prepare_mcp_servers(&worktree, &[], None, Some(&tool));
    assert_eq!(
        read_mcpctl_log(&seen, &notes),
        "setup --mcp tdd --mcp project\n"
    );
    assert!(
        notes.iter().any(|n| n.contains(".mcpctl.toml")),
        "{notes:?}"
    );
    let approved = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(
        approved["enabledMcpjsonServers"],
        serde_json::json!(["tdd"]),
        "what setup generated is approved"
    );

    // The setting wins over the file.
    let chosen = vec!["tdd".to_string()];
    let notes = prepare_mcp_servers(&worktree, &[], Some(&chosen), Some(&tool));
    assert_eq!(
        read_mcpctl_log(&seen, &notes).lines().last(),
        Some("setup --mcp tdd")
    );

    // An unreadable selection is a note and no setup.
    let before = log();
    fs::write(&toml, "[[mcp]\n").unwrap();
    let notes = prepare_mcp_servers(&worktree, &[], None, Some(&tool));
    assert_eq!(log(), before);
    assert!(
        notes.iter().any(|n| n.contains(".mcpctl.toml")),
        "{notes:?}"
    );
}

#[test]
fn a_symlinked_mcpctl_toml_is_neither_read_nor_set_up_whatever_the_setting() {
    let (repo, worktree) = checkout("mcpctl-symlink");
    let dir = repo.root().parent().unwrap();
    let seen = dir.join(format!("mcpctl-symlink-seen-{}", std::process::id()));
    let tool = dir.join(format!("fake-mcpctl-symlink-{}", std::process::id()));
    write_executable(
        &tool,
        &format!("#!/bin/sh\necho \"$*\" >> '{}'\n", seen.display()),
    );
    let outside = dir.join(format!("outside-mcpctl-{}.toml", std::process::id()));
    fs::write(&outside, "[[mcp]]\nname = \"from-outside\"\n").unwrap();
    std::os::unix::fs::symlink(&outside, worktree.join(".mcpctl.toml")).unwrap();

    let notes = prepare_mcp_servers(&worktree, &vars(&[("KEEP", "1")]), None, Some(&tool));
    assert!(
        !seen.exists(),
        "the outside file selected servers: {}",
        fs::read_to_string(&seen).unwrap_or_default()
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains(".mcpctl.toml") && n.contains("symbolic link")),
        "{notes:?}"
    );

    // Setup rewrites the file, so an explicit setting must not write through the link either.
    let chosen = vec!["tdd".to_string()];
    let notes = prepare_mcp_servers(&worktree, &[], Some(&chosen), Some(&tool));
    assert!(!seen.exists(), "setup ran through the link");
    assert!(
        notes.iter().any(|n| n.contains("symbolic link")),
        "{notes:?}"
    );

    // The run's environment still reaches the session.
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["env"]["KEEP"], "1");
}

#[test]
fn a_setup_that_fails_is_a_note_and_the_session_gets_the_environment_and_what_setup_left_behind() {
    let (repo, worktree) = checkout("mcpctl-prepare-fails");
    let tool = repo
        .root()
        .parent()
        .unwrap()
        .join(format!("fake-mcpctl-fails-{}", std::process::id()));
    write_executable(
        &tool,
        "#!/bin/sh\necho '{\"mcpServers\":{\"tdd\":{\"command\":\"c\"}}}' > .mcp.json\nexit 1\n",
    );
    fs::write(worktree.join(".mcpctl.toml"), "[[mcp]]\nname = \"tdd\"\n").unwrap();
    let env = vars(&[("KEEP", "1")]);

    // No mcpctl: the selection stays unset up, and nothing is approved without a .mcp.json.
    let notes = prepare_mcp_servers(&worktree, &env, None, None);
    assert!(
        notes.iter().any(|n| n.contains("mcpctl is not on PATH")),
        "{notes:?}"
    );
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["env"]["KEEP"], "1");
    assert!(
        settings.get("enabledMcpjsonServers").is_none(),
        "{settings}"
    );

    // A failing mcpctl that wrote part of .mcp.json: said so, and what it left is what is approved.
    let notes = prepare_mcp_servers(&worktree, &env, None, Some(&tool));
    assert!(
        notes
            .iter()
            .any(|n| n.contains("mcpctl setup") && n.contains("failed")),
        "{notes:?}"
    );
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["env"]["KEEP"], "1");
    assert_eq!(
        settings["enabledMcpjsonServers"],
        serde_json::json!(["tdd"]),
        "{settings}"
    );
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

#[test]
fn the_needs_input_hook_joins_the_settings_and_replaces_only_an_earlier_copy_of_itself() {
    let (_repo, worktree) = checkout("needs-input-hook");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    fs::write(
        worktree.join(".claude/settings.local.json"),
        r#"{"env":{"KEEP":"1"},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"mine"}]}]}}"#,
    )
    .unwrap();

    assert!(orq_orchestrator::write_needs_input_hook(
        &worktree,
        "'/old/orq' run needs-input 'r' --repo '/x'"
    )
    .is_empty());
    assert!(orq_orchestrator::write_needs_input_hook(
        &worktree,
        "'/new/orq' run needs-input 'r' --repo '/x'"
    )
    .is_empty());

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["env"]["KEEP"], "1");
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 2, "{pre:?}");
    assert_eq!(pre[0]["hooks"][0]["command"], "mine");
    assert_eq!(pre[1]["matcher"], "AskUserQuestion");
    assert_eq!(
        pre[1]["hooks"][0]["command"],
        "'/new/orq' run needs-input 'r' --repo '/x'"
    );
    let notification = settings["hooks"]["Notification"].as_array().unwrap();
    assert_eq!(notification.len(), 1, "{notification:?}");
    assert_eq!(notification[0]["matcher"], "permission_prompt");
    assert_eq!(
        notification[0]["hooks"][0]["command"],
        "'/new/orq' run needs-input 'r' --repo '/x'"
    );
    let post = settings["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 1, "{post:?}");
    assert_eq!(post[0]["matcher"], "AskUserQuestion");
    assert_eq!(
        post[0]["hooks"][0]["command"],
        "'/new/orq' run needs-input 'r' --repo '/x'"
    );
}

#[test]
fn settings_the_hook_cannot_join_are_a_note_and_are_left_as_they_were() {
    let (_repo, worktree) = checkout("needs-input-hook-bad");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let path = worktree.join(".claude/settings.local.json");
    fs::write(&path, r#"{"hooks":"nonsense"}"#).unwrap();
    let notes =
        orq_orchestrator::write_needs_input_hook(&worktree, "orq run needs-input r --repo /x");
    assert!(
        notes.iter().any(|n| n.contains("settings.local.json")),
        "{notes:?}"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{"hooks":"nonsense"}"#
    );

    let outside = worktree.join("outside.json");
    fs::write(&outside, "{}").unwrap();
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    let notes =
        orq_orchestrator::write_needs_input_hook(&worktree, "orq run needs-input r --repo /x");
    assert!(
        notes.iter().any(|n| n.contains("settings.local.json")),
        "{notes:?}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "{}");
}

#[test]
fn replacing_the_needs_input_hook_keeps_every_other_hook_beside_it() {
    let (_repo, worktree) = checkout("needs-input-hook-beside");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let own = "'/old/orq' run needs-input 'r' --repo '/x'";
    let earlier = serde_json::json!({"hooks": {"PreToolUse": [
        {"matcher": "AskUserQuestion", "hooks": [
            {"type": "command", "command": "audit"},
            {"type": "command", "command": own},
        ]},
        {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/local/bin/wrap run needs-input now"}]},
    ]}});
    fs::write(
        worktree.join(".claude/settings.local.json"),
        earlier.to_string(),
    )
    .unwrap();

    assert!(orq_orchestrator::write_needs_input_hook(
        &worktree,
        "'/new/orq' run needs-input 'r' --repo '/x'"
    )
    .is_empty());

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 3, "{pre:?}");
    assert_eq!(pre[0]["hooks"].as_array().unwrap().len(), 1, "{pre:?}");
    assert_eq!(pre[0]["hooks"][0]["command"], "audit");
    assert_eq!(
        pre[1]["hooks"][0]["command"],
        "/usr/local/bin/wrap run needs-input now"
    );
    assert_eq!(
        pre[2]["hooks"][0]["command"],
        "'/new/orq' run needs-input 'r' --repo '/x'"
    );
}

#[test]
fn taking_the_needs_input_hook_out_leaves_everything_else_and_creates_nothing() {
    let (_repo, worktree) = checkout("needs-input-hook-out");
    assert!(orq_orchestrator::remove_needs_input_hook(&worktree).is_empty());
    assert!(
        !worktree.join(".claude").exists(),
        "a worktree with no settings gets none"
    );

    let own = "'/orq' run needs-input 'r' --repo '/x'";
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let earlier = serde_json::json!({"env": {"A": "1"}, "hooks": {"PreToolUse": [
        {"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": own}]},
        {"matcher": "Bash", "hooks": [{"type": "command", "command": "audit"}]},
    ]}});
    fs::write(
        worktree.join(".claude/settings.local.json"),
        earlier.to_string(),
    )
    .unwrap();
    assert!(orq_orchestrator::write_needs_input_hook(&worktree, own).is_empty());

    assert!(orq_orchestrator::remove_needs_input_hook(&worktree).is_empty());

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["env"]["A"], "1");
    assert_eq!(
        settings["hooks"]["PreToolUse"].as_array().unwrap().len(),
        1,
        "{settings}"
    );
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "audit"
    );
    assert!(!settings.to_string().contains("needs-input"), "{settings}");
}

#[test]
fn only_the_exact_command_orq_writes_counts_as_its_own_hook() {
    let (_repo, worktree) = checkout("needs-input-hook-exact");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let someone_elses = [
        "'/usr/local/bin/audit' && '/old/orq' run needs-input 'r' --repo '/x'",
        "'/old/orq' run needs-input 'r' --repo '/x'; '/usr/local/bin/audit'",
        "'/old/orq' run needs-input 'r' && 'audit' --repo '/x'",
        "'/old/orq' run needs-input 'r' --repo '/x' 'more'",
    ];
    let quoted_quote = "'/it'\\''s/orq' run needs-input 'r' --repo '/it'\\''s'";
    let mut hooks: Vec<serde_json::Value> = someone_elses
        .iter()
        .map(|c| serde_json::json!({"type": "command", "command": c}))
        .collect();
    hooks.push(serde_json::json!({"type": "command", "command": quoted_quote}));
    let earlier = serde_json::json!({"hooks": {"PreToolUse": [{"matcher": "AskUserQuestion", "hooks": hooks}]}});
    fs::write(
        worktree.join(".claude/settings.local.json"),
        earlier.to_string(),
    )
    .unwrap();

    assert!(orq_orchestrator::remove_needs_input_hook(&worktree).is_empty());

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    let left: Vec<&str> = settings["hooks"]["PreToolUse"][0]["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["command"].as_str().unwrap())
        .collect();
    assert_eq!(
        left, someone_elses,
        "someone's compound command stays and ours, however quoted, goes"
    );
}

#[test]
fn a_hook_told_where_the_state_is_is_still_orqs_own() {
    let (_repo, worktree) = checkout("needs-input-hook-state");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let marked = "XDG_STATE_HOME='/st'\\''ate' '/orq' run needs-input 'r' --repo '/x' --marker '/logs/implement.done'";
    assert!(orq_orchestrator::write_needs_input_hook(&worktree, marked).is_empty());
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"], marked,
        "a marked hook hears the session stop: {settings}"
    );

    let someone_elses = "XDG_STATE_HOME='/st' audit; '/orq' run needs-input 'r' --repo '/x'";
    let mut settings = settings;
    settings["hooks"]["PreToolUse"][0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"type": "command", "command": someone_elses}));
    fs::write(
        worktree.join(".claude/settings.local.json"),
        settings.to_string(),
    )
    .unwrap();

    assert!(orq_orchestrator::remove_needs_input_hook(&worktree).is_empty());
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert!(!settings.to_string().contains("--marker"), "{settings}");
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"], someone_elses,
        "{settings}"
    );
}

#[test]
fn a_hook_with_a_marker_also_hears_the_session_stop_go_idle_and_be_answered_and_only_then() {
    let (_repo, worktree) = checkout("needs-input-hook-marker");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    fs::write(
        worktree.join(".claude/settings.local.json"),
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"mine"}]}]}}"#,
    )
    .unwrap();
    let marked = "'/orq' run needs-input 'r' --repo '/x' --marker '/logs/implement.done'";
    assert!(orq_orchestrator::write_needs_input_hook(&worktree, marked).is_empty());
    // A later session's marker replaces the earlier one's.
    let marked = "'/orq' run needs-input 'r' --repo '/x' --marker '/logs/gate-fix.done'";
    assert!(orq_orchestrator::write_needs_input_hook(&worktree, marked).is_empty());

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    let commands = |event: &str| -> Vec<(String, String)> {
        settings["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["matcher"].as_str().unwrap_or("").to_string(),
                    entry["hooks"][0]["command"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(
        commands("Stop"),
        [
            ("".to_string(), "mine".to_string()),
            ("".to_string(), marked.to_string())
        ]
    );
    assert_eq!(
        commands("UserPromptSubmit"),
        [("".to_string(), marked.to_string())]
    );
    assert_eq!(
        commands("Notification"),
        [
            ("permission_prompt".to_string(), marked.to_string()),
            ("idle_prompt".to_string(), marked.to_string())
        ]
    );
    assert_eq!(
        commands("PreToolUse"),
        [("AskUserQuestion".to_string(), marked.to_string())]
    );
    assert!(
        settings["hooks"]["Stop"][1].get("matcher").is_none(),
        "Stop takes no matcher: {settings}"
    );

    // A session without a marker cannot say it is finished: it gets no stop, idle or prompt hooks.
    let unmarked = "'/orq' run needs-input 'r' --repo '/x'";
    assert!(orq_orchestrator::write_needs_input_hook(&worktree, unmarked).is_empty());
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(
        settings["hooks"]["Stop"].as_array().unwrap().len(),
        1,
        "{settings}"
    );
    assert_eq!(settings["hooks"]["Stop"][0]["hooks"][0]["command"], "mine");
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "{settings}"
    );
    assert_eq!(
        settings["hooks"]["Notification"].as_array().unwrap().len(),
        1,
        "{settings}"
    );
    assert_eq!(
        settings["hooks"]["Notification"][0]["matcher"],
        "permission_prompt"
    );

    assert!(orq_orchestrator::write_needs_input_hook(&worktree, marked).is_empty());
    assert!(orq_orchestrator::remove_needs_input_hook(&worktree).is_empty());
    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert!(!settings.to_string().contains("needs-input"), "{settings}");
    assert_eq!(
        settings["hooks"]["Stop"][0]["hooks"][0]["command"], "mine",
        "the user's own hook stays"
    );
}
