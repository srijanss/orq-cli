//! A run's implementer has nobody to answer a permission prompt, so the worktree's session settings
//! say what it may do: accept its edits, run the local commands and MCP tools a run needs, never
//! publish to a remote, and write its exit marker into the run's logs.

use std::fs;
use std::path::Path;

use orq_orchestrator::write_run_permissions;
use orq_test_support::GitFixture;

fn checkout(tag: &str) -> (GitFixture, std::path::PathBuf) {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree(tag);
    (repo, worktree)
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    serde_json::from_value(value.clone()).unwrap()
}

#[test]
fn the_permissions_join_the_settings_beside_what_is_already_there_and_are_not_repeated() {
    let (_repo, worktree) = checkout("permissions-merge");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let path = worktree.join(".claude/settings.local.json");
    fs::write(&path, r#"{"env":{"A":"1"},"permissions":{"allow":["WebFetch"],"additionalDirectories":["/elsewhere"]}}"#).unwrap();
    let logs = Path::new("/state/orq/logs/run-1");

    let extra = vec!["Bash(make lint:*)".to_string()];
    assert_eq!(write_run_permissions(&worktree, &extra, logs), Vec::<String>::new());
    assert_eq!(write_run_permissions(&worktree, &extra, logs), Vec::<String>::new());

    let settings = read_json(&path);
    assert_eq!(settings["env"]["A"], "1", "other keys stay");
    let permissions = &settings["permissions"];
    assert_eq!(permissions["defaultMode"], "acceptEdits");
    let allow = strings(&permissions["allow"]);
    for wanted in ["WebFetch", "Bash(git add:*)", "Bash(git commit:*)", "Bash(git status:*)", "Bash(cargo test:*)", "Bash(npm ci)", "Bash(node build.mjs)", "mcp__outside-in-tdd", "mcp__project-mcp", "Bash(make lint:*)"] {
        assert_eq!(allow.iter().filter(|rule| *rule == wanted).count(), 1, "{wanted} once in {allow:?}");
    }
    let deny = strings(&permissions["deny"]);
    for wanted in ["Bash(git push:*)", "Bash(cargo publish:*)", "Bash(npm publish:*)"] {
        assert_eq!(deny.iter().filter(|rule| *rule == wanted).count(), 1, "{wanted} once in {deny:?}");
    }
    assert_eq!(strings(&permissions["additionalDirectories"]), ["/elsewhere", "/state/orq/logs/run-1"]);
}

/// Publishing is orq's own step: a project cannot allow the session to do it.
#[test]
fn an_added_rule_that_would_publish_to_a_remote_is_left_out_with_a_note() {
    let (_repo, worktree) = checkout("permissions-publish");
    let extra = vec!["Bash(git push origin:*)".to_string(), "Bash(cargo publish)".to_string(), "Bash(make lint:*)".to_string()];

    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"));

    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().all(|note| note.contains("allowed_tools")), "{notes:?}");
    let allow = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"]);
    assert!(allow.iter().any(|rule| rule == "Bash(make lint:*)"), "{allow:?}");
    assert!(!allow.iter().any(|rule| rule.contains("push") || rule.contains("publish")), "{allow:?}");
}

/// A rule broad enough to reach publishing another way, a whole program that can publish or a
/// shell that can run anything, is left out too; a narrow one stays.
#[test]
fn an_added_rule_broad_enough_to_publish_another_way_is_left_out() {
    let (_repo, worktree) = checkout("permissions-broad");
    let broad = ["Bash", "Bash(*)", "Bash(git:*)", "Bash(git *)", "Bash(git -C:*)", "Bash(cargo:*)", "Bash(npm *)", "Bash(gh:*)", "Bash(sh:*)", "Bash(bash -c:*)", "Bash(env:*)", "Bash(xargs:*)"];
    let narrow = ["Bash(make lint:*)", "Bash(cargo nextest:*)", "Bash(git fetch:*)", "WebFetch"];
    let extra: Vec<String> = broad.iter().chain(narrow.iter()).map(|rule| rule.to_string()).collect();

    let notes = write_run_permissions(&worktree, &extra, Path::new("/logs/r"));

    assert_eq!(notes.len(), broad.len(), "{notes:?}");
    let allow = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"]);
    for rule in broad {
        assert!(!allow.iter().any(|allowed| allowed == rule), "{rule} in {allow:?}");
    }
    for rule in narrow {
        assert!(allow.iter().any(|allowed| allowed == rule), "{rule} not in {allow:?}");
    }
}

#[test]
fn settings_the_permissions_cannot_join_are_a_note_and_are_left_as_they_were() {
    let (_repo, worktree) = checkout("permissions-bad");
    fs::create_dir_all(worktree.join(".claude")).unwrap();
    let path = worktree.join(".claude/settings.local.json");
    fs::write(&path, r#"{"permissions":["nonsense"]}"#).unwrap();

    let notes = write_run_permissions(&worktree, &[], Path::new("/logs/r"));

    assert!(notes.iter().any(|note| note.contains("settings.local.json")), "{notes:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"permissions":["nonsense"]}"#);
}

/// A session that refuses what it was not allowed still edits the worktree and writes its marker.
#[test]
fn the_session_may_edit_the_worktree_and_write_into_the_runs_logs() {
    let (_repo, worktree) = checkout("permissions-edits");

    assert_eq!(write_run_permissions(&worktree, &[], Path::new("/state/orq/logs/run-1")), Vec::<String>::new());

    let allow = strings(&read_json(&worktree.join(".claude/settings.local.json"))["permissions"]["allow"]);
    for wanted in ["Edit(./**)", "Edit(//state/orq/logs/run-1/**)", "Read", "Skill"] {
        assert!(allow.iter().any(|rule| rule == wanted), "{wanted} in {allow:?}");
    }
    assert!(!allow.iter().any(|rule| rule == "Edit"), "no edit anywhere: {allow:?}");
}
