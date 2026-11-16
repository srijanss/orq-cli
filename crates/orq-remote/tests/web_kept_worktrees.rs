//! With remote actions on, the Settings page's Storage card lists "Kept worktrees": every worktree orq made
//! that a run left behind, grouped by project, each with its branch, run, how the run ended, why it was kept,
//! its commits, changed files and size. Delete asks first: a merged worktree with only ignored files names
//! them; any other asks for the branch name typed back and sends force. "Delete all merged" removes the
//! safe ones together. A worktree that changed since it was read is refused in the server's words.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 9] = [
    "[data-kept-worktrees]",
    "[data-kept-worktree]",
    "[data-kept-delete]",
    "[data-kept-delete-merged]",
    "[data-kept-branch-typed]",
    "[data-confirm]",
    "[data-confirm]:disabled",
    "[data-cancel]",
    "[data-kept-file]",
];

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settings(on: bool) -> Value {
    json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": on, "version": "v-1"}}})
}

/// A worktree as GET /api/storage/worktrees lists it.
fn worktree(branch: &str, run: &str, commits: u64, files: Value, safe: bool) -> Value {
    json!({"branch": branch, "run": run, "ending": "completed", "reason": "the worktree has uncommitted changes", "target": "main",
        "commits": commits, "files": files, "more_files": 0, "files_unreadable": false, "merged": commits == 0, "safe": safe,
        "bytes": 700, "size": "700 B", "version": format!("v-{branch}")})
}

fn ignored_only(branch: &str, run: &str) -> Value {
    worktree(branch, run, 0, json!([{"path": ".claude/settings.local.json", "kind": "ignored"}]), true)
}

fn listing(projects: Value, count: u64, size: &str) -> Value {
    json!({"status": 200, "json": {"projects": projects, "count": count, "bytes": 1, "size": size}})
}

fn two_projects() -> Value {
    listing(
        json!([
            {"id": "alpha", "name": "Alpha", "worktrees": [ignored_only("fix--A", "run-1")]},
            {"id": "beta", "name": "Beta", "worktrees": [worktree("team/fix--B", "run-2", 2, json!([{"path": "notes.txt", "kind": "untracked"}]), false)]}
        ]),
        2,
        "1.4 GB",
    )
}

fn on_settings(storage: Vec<Value>, steps: Vec<Value>, phone: bool) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/settings", "responses": [settings(true)], "storage": storage, "steps": all, "selectors": SELECTORS, "phone": phone}))
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn has(frame: &Value, text: &str) -> bool {
    shown(frame).iter().any(|t| t == text)
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn storage_posts(out: &Value) -> Vec<Value> {
    out["storageRequests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

fn body(request: &Value) -> Value {
    serde_json::from_str(request["body"].as_str().unwrap()).unwrap()
}

#[test]
fn the_storage_card_lists_kept_worktrees_grouped_by_project_with_their_run_commits_files_and_size() {
    let Some(out) = on_settings(vec![two_projects()], vec![], false) else { return };
    let read = &out["storageRequests"][0];
    assert_eq!(read["url"], "/api/storage/worktrees", "{out}");
    assert_eq!(read["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    let page = last(&out);
    assert_eq!(count(page, "[data-kept-worktrees]"), 1, "{out}");
    assert_eq!(count(page, "[data-kept-worktree]"), 2, "{out}");
    for text in ["Kept worktrees · 2 · 1.4 GB", "Alpha", "Beta", "fix--A", "team/fix--B", "Run ended: completed", "Kept because the worktree has uncommitted changes", "merged", "2 commits not on main", "700 B"] {
        assert!(has(page, text), "{text}: {:?}", shown(page));
    }
    let links: Vec<&str> = page["settled"]["links"].as_array().unwrap().iter().filter_map(|l| l["href"].as_str()).collect();
    assert!(links.contains(&"/projects/alpha/orchestrations/run-1"), "{links:?}");
    assert!(links.contains(&"/projects/beta/orchestrations/run-2"), "{links:?}");
    assert_eq!(count(page, "[data-kept-file]"), 2, "{out}");
    assert!(shown(page).iter().any(|t| t.contains(".claude/settings.local.json")), "{:?}", shown(page));
    assert_eq!(count(page, "[data-kept-delete-merged]"), 0, "one safe worktree is not a batch: {out}");
}

#[test]
fn none_kept_says_so_and_an_unknown_size_and_many_files_are_worded() {
    let Some(out) = on_settings(vec![listing(json!([]), 0, "0 B")], vec![], false) else { return };
    assert!(has(last(&out), "No kept worktrees"), "{:?}", shown(last(&out)));
    let mut big = worktree("fix--Z", "run-9", 0, json!([{"path": "a.txt", "kind": "tracked"}]), false);
    big["more_files"] = json!(31);
    big["size"] = Value::Null;
    big["bytes"] = Value::Null;
    let Some(out) = on_settings(vec![listing(json!([{"id": "alpha", "name": "Alpha", "worktrees": [big]}]), 1, "0 B")], vec![], false) else { return };
    for text in ["and 31 more", "size unknown"] {
        assert!(has(last(&out), text), "{text}: {:?}", shown(last(&out)));
    }
}

#[test]
fn the_section_is_absent_when_the_server_has_no_worktree_route_or_actions_are_off() {
    let Some(out) = on_settings(vec![], vec![], false) else { return };
    assert_eq!(count(last(&out), "[data-kept-worktrees]"), 0, "{out}");
    let Some(out) = run(json!({"url": "/settings", "responses": [settings(false)], "storage": [two_projects()], "steps": [{"submit": TOKEN}], "selectors": SELECTORS})) else { return };
    assert_eq!(count(last(&out), "[data-kept-worktrees]"), 0, "{out}");
    assert!(out["storageRequests"].as_array().unwrap().is_empty(), "{out}");
}

#[test]
fn deleting_a_safe_worktree_names_its_ignored_files_and_sends_its_version() {
    let deleted = json!({"status": 200, "json": {"project": "alpha", "branch": "fix--A", "sha": "abc", "run": "run-1"}});
    let after = listing(json!([{"id": "beta", "name": "Beta", "worktrees": []}]), 0, "0 B");
    let steps = vec![json!({"click": "[data-kept-delete]", "index": 0}), json!({"click": "[data-confirm]"})];
    let Some(out) = on_settings(vec![two_projects(), deleted, after], steps, false) else { return };
    let asked = &out["frames"][2];
    assert!(has(asked, "Delete this worktree and its branch? Its ignored files (.claude/settings.local.json) are deleted with it."), "{:?}", shown(asked));
    assert_eq!(count(asked, "[data-kept-branch-typed]"), 0, "a safe delete asks for no typing: {out}");
    let sent = storage_posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/storage/worktrees/alpha/fix--A/delete");
    assert_eq!(sent[0]["headers"]["Authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(body(&sent[0]), json!({"version": "v-fix--A"}));
    let done = last(&out);
    assert!(has(done, "Deleted fix--A."), "{:?}", shown(done));
    assert!(has(done, "No kept worktrees"), "the list is read again: {:?}", shown(done));
}

#[test]
fn deleting_unmerged_work_names_it_and_waits_for_the_branch_typed_back_then_forces() {
    let deleted = json!({"status": 200, "json": {"project": "beta", "branch": "team/fix--B", "sha": "abc", "run": "run-2"}});
    let steps = vec![
        json!({"click": "[data-kept-delete]", "index": 1}),
        json!({"set": "[data-kept-branch-typed]", "value": "team/fix"}),
        json!({"click": "[data-confirm]"}),
        json!({"set": "[data-kept-branch-typed]", "value": "team/fix--B"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = on_settings(vec![two_projects(), deleted, two_projects()], steps, false) else { return };
    let asked = &out["frames"][2];
    assert!(
        has(asked, "Delete this worktree and its branch with 2 commits not on main and its changed files (notes.txt)? They are lost for good."),
        "{:?}",
        shown(asked)
    );
    assert_eq!(count(asked, "[data-confirm]:disabled"), 1, "{out}");
    assert_eq!(count(&out["frames"][3], "[data-confirm]:disabled"), 1, "a partial name does not unlock it: {out}");
    assert_eq!(count(&out["frames"][5], "[data-confirm]:disabled"), 0, "{out}");
    let sent = storage_posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/storage/worktrees/beta/team/fix--B/delete");
    assert_eq!(body(&sent[0]), json!({"version": "v-team/fix--B", "force": true}));
}

#[test]
fn a_refused_delete_is_shown_in_the_servers_words_and_nothing_else_is_sent() {
    let refused = json!({"status": 409, "json": {"error": "the worktree changed since you opened it; nothing was deleted"}});
    let steps = vec![json!({"click": "[data-kept-delete]", "index": 0}), json!({"click": "[data-confirm]"})];
    let Some(out) = on_settings(vec![two_projects(), refused, two_projects()], steps, false) else { return };
    assert!(has(last(&out), "the worktree changed since you opened it; nothing was deleted"), "{:?}", shown(last(&out)));
    assert_eq!(storage_posts(&out).len(), 1, "{out}");
}

#[test]
fn delete_all_merged_is_offered_for_two_safe_worktrees_and_sends_only_those() {
    let three = listing(
        json!([
            {"id": "alpha", "name": "Alpha", "worktrees": [ignored_only("fix--A", "run-1"), worktree("fix--U", "run-3", 1, json!([]), false)]},
            {"id": "beta", "name": "Beta", "worktrees": [ignored_only("fix--C", "run-2")]}
        ]),
        3,
        "2.1 KB",
    );
    let results = json!({"status": 200, "json": {"results": [
        {"project": "alpha", "branch": "fix--A", "deleted": true},
        {"project": "beta", "branch": "fix--C", "deleted": false, "error": "the worktree changed since you opened it; nothing was deleted"}
    ]}});
    let steps = vec![json!({"click": "[data-kept-delete-merged]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = on_settings(vec![three, results, two_projects()], steps, false) else { return };
    let asked = &out["frames"][2];
    assert!(has(asked, "Delete 2 merged worktrees and their branches?"), "{:?}", shown(asked));
    let sent = storage_posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0]["url"], "/api/storage/worktrees/delete-merged");
    assert_eq!(
        body(&sent[0]),
        json!({"worktrees": [{"project": "alpha", "branch": "fix--A", "version": "v-fix--A"}, {"project": "beta", "branch": "fix--C", "version": "v-fix--C"}]})
    );
    let done = last(&out);
    assert!(has(done, "Deleted 1 of 2 merged worktrees."), "{:?}", shown(done));
    assert!(has(done, "fix--C: the worktree changed since you opened it; nothing was deleted"), "{:?}", shown(done));
}

#[test]
fn on_a_phone_each_kept_worktree_is_a_card_with_its_delete() {
    let Some(out) = on_settings(vec![two_projects()], vec![], true) else { return };
    let page = last(&out);
    assert_eq!(count(page, "[data-kept-worktree]"), 2, "{out}");
    assert_eq!(count(page, "[data-kept-delete]"), 2, "{out}");
    assert!(page["settled"]["classes"].as_array().unwrap().iter().any(|c| c == "kept-card"), "{out}");
}

#[test]
fn a_projects_settings_page_has_no_worktree_section() {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let projects = json!({"status": 200, "json": {"projects": [project("alpha")]}});
    let project_settings = json!({"status": 200, "json": {"settings": {
        "implementer": {"project": null, "fallback": {"harness": "claude", "model": "opus", "effort": "high", "source": "machine_default"}},
        "reviewer": {"project": null, "fallback": {"harness": "pi", "model": "kimi", "effort": null, "source": "tdd_config"}},
        "delivery": "merge_local", "revision": 1
    }}});
    let scenario = json!({"url": "/projects/alpha/settings", "responses": [project_settings, projects, settings(true)], "storage": [two_projects()], "steps": [{"submit": TOKEN}], "selectors": SELECTORS});
    let Some(out) = run(scenario) else { return };
    assert_eq!(count(last(&out), "[data-kept-worktrees]"), 0, "{out}");
    assert!(out["storageRequests"].as_array().unwrap().is_empty(), "{out}");
}
