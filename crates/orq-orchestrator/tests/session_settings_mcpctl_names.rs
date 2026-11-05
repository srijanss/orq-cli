use std::fs;
use std::path::Path;

use orq_orchestrator::write_session_settings;
use orq_test_support::GitFixture;

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn a_chosen_mcpctl_package_approves_the_mcp_json_server_that_runs_it_under_another_name() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("mcpctl-names");
    fs::write(
        worktree.join(".mcp.json"),
        r#"{"mcpServers":{
            "outside-in-tdd":{"command":"/usr/local/bin/mcpctl","args":["run","outside-in-tdd-mcp"]},
            "project-mcp":{"command":"/usr/local/bin/mcpctl","args":["run","project-mcp"]},
            "docs":{"command":"docs-server"}
        }}"#,
    )
    .unwrap();

    let chosen = vec!["outside-in-tdd-mcp".to_string(), "project-mcp".to_string()];
    let notes = write_session_settings(&worktree, &[], Some(&chosen));

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
    let notes = write_session_settings(&worktree, &[], Some(&chosen));

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
    let notes = write_session_settings(&worktree, &[], Some(&chosen));

    let settings = read_json(&worktree.join(".claude/settings.local.json"));
    assert_eq!(settings["enabledMcpjsonServers"], serde_json::json!(["docs"]));
    assert_eq!(
        notes,
        vec!["MCP server shared-mcp is run by more than one .mcp.json server (one, two); it was not approved".to_string()]
    );
}
