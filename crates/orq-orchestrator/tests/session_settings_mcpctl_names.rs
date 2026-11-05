use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::write_session_settings;
use orq_test_support::GitFixture;

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// The `mcpctl` orq found and sets servers up with, outside the worktree.
fn trusted_mcpctl(repo: &GitFixture, tag: &str) -> PathBuf {
    let bin = repo.root().parent().unwrap().join(format!("bin-{tag}-{}", std::process::id()));
    fs::create_dir_all(&bin).unwrap();
    let mcpctl = bin.join("mcpctl");
    fs::write(&mcpctl, "#!/bin/sh\nexit 0\n").unwrap();
    mcpctl
}

#[test]
fn a_chosen_mcpctl_package_approves_the_mcp_json_server_that_runs_it_under_another_name() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("mcpctl-names");
    let mcpctl = trusted_mcpctl(&repo, "names");
    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{
            "outside-in-tdd":{"command":"MCPCTL","args":["run","outside-in-tdd-mcp"]},
            "project-mcp":{"command":"MCPCTL","args":["run","project-mcp"]},
            "docs":{"command":"docs-server"}
        }}"#
            .replace("MCPCTL", &mcpctl.display().to_string()),
    )
    .unwrap();

    let chosen = vec!["outside-in-tdd-mcp".to_string(), "project-mcp".to_string()];
    let notes = write_session_settings(&worktree, &[], Some(&chosen), Some(&mcpctl));

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["enabledMcpjsonServers"], serde_json::json!(["outside-in-tdd", "project-mcp"]));
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn a_package_is_matched_only_through_an_mcpctl_run_entry_and_an_unmatched_one_is_still_a_note() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("mcpctl-match");
    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{
            "tdd":{"command":"mcpctl","args":["run","tdd-mcp"]},
            "lookalike":{"command":"npx","args":["run","other-mcp"]}
        }}"#,
    )
    .unwrap();

    let chosen = vec!["tdd-mcp".to_string(), "other-mcp".to_string()];
    let notes = write_session_settings(&worktree, &[], Some(&chosen), Some(&trusted_mcpctl(&repo, "match")));

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["enabledMcpjsonServers"], serde_json::json!(["tdd"]));
    assert_eq!(notes, vec!["MCP server other-mcp is not declared in .mcp.json; it was not approved".to_string()]);
}

#[test]
fn an_exact_server_name_wins_over_a_package_alias_and_an_alias_run_by_two_servers_approves_neither() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("mcpctl-exact");
    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{
            "a":{"command":"mcpctl","args":["run","docs"]},
            "docs":{"command":"docs-server"},
            "one":{"command":"mcpctl","args":["run","shared-mcp"]},
            "two":{"command":"mcpctl","args":["run","shared-mcp"]}
        }}"#,
    )
    .unwrap();

    let chosen = vec!["docs".to_string(), "shared-mcp".to_string()];
    let notes = write_session_settings(&worktree, &[], Some(&chosen), Some(&trusted_mcpctl(&repo, "exact")));

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["enabledMcpjsonServers"], serde_json::json!(["docs"]));
    assert_eq!(
        notes,
        vec!["MCP server shared-mcp is run by more than one .mcp.json server (one, two); it was not approved".to_string()]
    );
}

#[test]
fn a_package_alias_counts_only_for_the_mcpctl_orq_sets_up_and_not_for_another_program_of_that_name() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("mcpctl-trusted");
    let mcpctl = trusted_mcpctl(&repo, "trusted");
    fs::create_dir_all(worktree.join("scripts")).unwrap();
    fs::write(worktree.join("scripts/mcpctl"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{
            "trusted":{"command":"MCPCTL","args":["run","good-mcp"]},
            "local":{"command":"./scripts/mcpctl","args":["run","evil-mcp"]}
        }}"#
            .replace("MCPCTL", &mcpctl.display().to_string()),
    )
    .unwrap();

    let chosen = vec!["good-mcp".to_string(), "evil-mcp".to_string()];
    let notes = write_session_settings(&worktree, &[], Some(&chosen), Some(&mcpctl));

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["enabledMcpjsonServers"], serde_json::json!(["trusted"]));
    assert_eq!(notes, vec!["MCP server evil-mcp is not declared in .mcp.json; it was not approved".to_string()]);

    let without = write_session_settings(&worktree, &[], Some(&["good-mcp".to_string()]), None);
    assert!(
        without.contains(&"MCP server good-mcp is not declared in .mcp.json; it was not approved".to_string()),
        "with no mcpctl found, no entry is trusted as one: {without:?}"
    );
}
