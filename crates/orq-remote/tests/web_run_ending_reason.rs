//! A blocked or failed run says why it ended: the API gives the reason its `run.completed` recorded (redacted)
//! and, for a review escalation, the open findings it names with file:line from the worktree's findings; the run
//! page shows it in a box at the top (once, not again when a step's failure already says it), the Runs row shows
//! its first sentence, and the queue's paused line links to the run.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const ESCALATED: &str = "the review escalated after 3 rounds with 2 findings open, and an unattended run does not leave a high-severity or security finding for later: group-kill-incompatible-with-dash (high, bugs), token-in-log (medium, security)";

struct Fixture {
    roots: StorageRoots,
    scan: PathBuf,
    store: RunStore,
    project: String,
}

fn fixture(name: &str) -> Fixture {
    let base = web_deps::private_temp_dir(name);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let scan = base.join("code");
    let alpha = scan.join("alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&alpha).status().unwrap().success());
    let project = orq_git::project_id(&orq_git::resolve_project(&alpha).unwrap());
    let store = RunStore::new(&roots.state);
    Fixture { roots, scan, store, project }
}

impl Fixture {
    /// A run whose events are `kinds`, each `(kind, payload)`, with its worktree at `worktree`.
    fn run(&self, id: &str, worktree: &Path, kinds: &[(&str, Value)]) {
        let record = RunRecord { run_id: id.into(), project_id: self.project.clone(), feature_id: "f-".to_string() + id, remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{id}"), worktree: worktree.to_path_buf(), repository: String::new(), profiles: Default::default(), delivery: Default::default() };
        self.store.create(&record).unwrap();
        for (n, (kind, payload)) in kinds.iter().enumerate() {
            let stage = kind.starts_with("stage.").then(|| "implement_and_review".to_string());
            let event = Event { schema_version: 1, event_id: format!("{id}-{n}"), run_id: id.into(), project_id: self.project.clone(), feature_id: "f".into(), kind: kind.to_string(), stage, attempt: 1, occurred_at: format!("2026-11-20T10:00:{n:02}Z"), payload: payload.clone() };
            self.store.append_event(&event).unwrap();
        }
    }

    fn get(&self, path: &str) -> Value {
        let provider = StoreProjects::new(self.roots.clone(), self.scan.clone());
        let config = RemoteConfig { token: "tok".into() };
        let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let reply = handle_with(&self.roots, &config, "server-project", "server", &request, &provider);
        assert_eq!(reply.status, 200, "{path}: {:?}", reply.body);
        serde_json::to_value(&reply.body).unwrap()
    }
}

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 3] = ["[data-ending-reason]", "[data-ending-finding]", "[data-ending-first]"];

/// The page at `url`, given `responses` in order, after signing in; `None` when the web deps are missing.
fn page(url: &str, responses: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": [{"submit": TOKEN}], "selectors": SELECTORS, "token": TOKEN});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

/// The run page of `run` with `events`.
fn run_page(run: Value, events: Value) -> Option<Value> {
    let id = run["run_id"].as_str().unwrap().to_string();
    let body = json!({"status": 200, "json": {"run": run, "events": events, "next_cursor": null, "remote_actions": false}});
    page(&format!("/projects/alpha/orchestrations/{id}"), vec![body, projects(), json!({"status": 200, "stream": [], "end": "hold"})])
}

fn row(id: &str, state: &str, extra: Value) -> Value {
    let mut row = json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    for (key, value) in extra.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(out: &Value, selector: &str) -> u64 {
    last(out)["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(out: &Value) -> Vec<String> {
    last(out)["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn the_page_of_a_blocked_run_shows_why_with_the_escalated_findings_one_per_line() {
    let findings = json!([
        {"id": "group-kill-incompatible-with-dash", "severity": "high", "category": "bugs", "file": "src/kill.rs", "line": 42},
        {"id": "token-in-log", "severity": "medium", "category": "security"}
    ]);
    let run = row("r-blocked", "blocked", json!({"ending_reason": ESCALATED, "ending_findings": findings}));
    let events = json!([{"event_id": "e1", "kind": "stage.started", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-20T10:00:00Z"}, {"event_id": "e2", "kind": "stage.completed", "stage": "implement_and_review", "attempt": 1, "occurred_at": "2026-11-20T10:05:00Z"}]);
    let Some(out) = run_page(run, events) else { return };
    assert_eq!(count(&out, "[data-ending-reason]"), 1, "{out}");
    let shown = texts(&out);
    assert!(shown.iter().any(|t| t == ESCALATED), "{shown:?}");
    assert_eq!(count(&out, "[data-ending-finding]"), 2, "{out}");
    assert!(shown.iter().any(|t| t == "group-kill-incompatible-with-dash (high, bugs) src/kill.rs:42"), "{shown:?}");
    assert!(shown.iter().any(|t| t == "token-in-log (medium, security)"), "{shown:?}");
}

#[test]
fn a_reason_a_failed_step_already_shows_is_not_repeated_in_a_box() {
    const WHY: &str = "the gate failed: 3 tests failed";
    let failed = json!({"event_id": "e2", "kind": "stage.failed", "stage": "gate", "attempt": 1, "occurred_at": "2026-11-20T10:05:00Z", "failure": {"reason": WHY}});
    let events = json!([{"event_id": "e1", "kind": "stage.started", "stage": "gate", "attempt": 1, "occurred_at": "2026-11-20T10:00:00Z"}, failed]);
    let Some(out) = run_page(row("r-failed", "failed", json!({"ending_reason": WHY})), events) else { return };
    assert_eq!(count(&out, "[data-ending-reason]"), 0, "{out}");
    assert!(texts(&out).iter().any(|t| t.starts_with(&format!("Failed: {WHY}"))), "the step still says it: {:?}", texts(&out));
    // A different reason than the step's is still shown.
    let other = json!([{"event_id": "e1", "kind": "stage.failed", "stage": "gate", "attempt": 1, "occurred_at": "2026-11-20T10:05:00Z", "failure": {"reason": "something else"}}]);
    let Some(out) = run_page(row("r-failed", "failed", json!({"ending_reason": WHY})), other) else { return };
    assert_eq!(count(&out, "[data-ending-reason]"), 1, "{out}");
}

#[test]
fn merged_and_resumed_run_pages_show_no_box() {
    // A reason the server should not give is still not shown for a run that did not end blocked or failed.
    for (id, state) in [("r-merged", "merged"), ("r-resumed", "resumed")] {
        let Some(out) = run_page(row(id, state, json!({"ending_reason": ESCALATED})), json!([])) else { return };
        assert_eq!(count(&out, "[data-ending-reason]"), 0, "{out}");
    }
}

#[test]
fn a_blocked_or_failed_runs_row_shows_the_first_sentence_of_why() {
    let rows = vec![
        row("r-failed", "failed", json!({"ending_reason": "The gate failed. 3 tests failed in crates/orq-remote."})),
        row("r-blocked", "blocked", json!({"ending_reason": ESCALATED})),
        row("r-done", "completed", json!({"ending_reason": "Never shown."})),
    ];
    let list = json!({"status": 200, "json": {"runs": rows, "next_cursor": null, "total": 3, "running": 0, "remote_actions": false}});
    let Some(out) = page("/projects/alpha/orchestrations", vec![list, projects()]) else { return };
    assert_eq!(count(&out, "[data-ending-first]"), 2, "{out}");
    let shown = texts(&out);
    assert!(shown.iter().any(|t| t == "The gate failed."), "{shown:?}");
    assert!(!shown.iter().any(|t| t.contains("3 tests failed")), "only the first sentence: {shown:?}");
    assert!(shown.iter().any(|t| t == ESCALATED), "{shown:?}");
    assert!(!shown.iter().any(|t| t == "Never shown."), "{shown:?}");
}

#[test]
fn the_queues_paused_line_links_to_the_run_it_names() {
    let queue = json!({"entries": [], "count": 0, "paused": "the run run-1795163152-2728058-1 of release-after-every-merge failed: the review escalated"});
    let reply = json!({"status": 200, "json": {"queue": queue, "remote_actions": false}});
    let listing = json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": {"count": 0, "paused": true}}]}});
    let Some(out) = page("/projects/alpha/queue", vec![reply, listing]) else { return };
    let links: Vec<(String, String)> = last(&out)["settled"]["links"].as_array().unwrap().iter().map(|l| (l["href"].as_str().unwrap_or("").to_string(), l["text"].as_str().unwrap_or("").to_string())).collect();
    assert!(links.contains(&("/projects/alpha/orchestrations/run-1795163152-2728058-1".into(), "run-1795163152-2728058-1".into())), "{links:?}");
    let shown = texts(&out);
    // The paragraph's own text, around the link.
    assert!(shown.iter().any(|t| t == "Paused: the run  of release-after-every-merge failed: the review escalated"), "{shown:?}");
}

fn blocked_after_steps(reason: &str) -> Vec<(&'static str, Value)> {
    vec![("run.started", json!({})), ("stage.started", json!({})), ("stage.completed", json!({"exit_code": 0})), ("run.completed", json!({"ending": "blocked", "reason": reason}))]
}

#[test]
fn a_blocked_run_whose_steps_all_completed_gives_its_ending_reason_and_the_findings_it_names() {
    let f = fixture("ending-blocked");
    let worktree = f.roots.data.join("wt-blocked");
    std::fs::create_dir_all(&worktree).unwrap();
    let findings = json!({"review:abc": [{"id": "group-kill-incompatible-with-dash", "severity": "high", "category": "bugs", "file": "src/kill.rs", "line": 42, "status": "open"}]});
    std::fs::write(worktree.join(".tdd-review-findings.json"), findings.to_string()).unwrap();
    f.run("r-blocked", &worktree, &blocked_after_steps(ESCALATED));

    let detail = f.get("/api/projects/alpha/runs/r-blocked");
    assert_eq!(detail["run"]["ending_reason"], json!(ESCALATED), "{detail}");
    assert_eq!(
        detail["run"]["ending_findings"],
        json!([
            {"id": "group-kill-incompatible-with-dash", "severity": "high", "category": "bugs", "file": "src/kill.rs", "line": 42},
            {"id": "token-in-log", "severity": "medium", "category": "security"}
        ]),
        "{detail}"
    );
    let list = f.get("/api/projects/alpha/runs");
    let row = list["runs"].as_array().unwrap().iter().find(|r| r["run_id"] == "r-blocked").unwrap();
    assert_eq!(row["ending_reason"], json!(ESCALATED), "{list}");
}

#[test]
fn a_finding_id_whose_review_scopes_disagree_on_its_place_gets_no_file_line() {
    let f = fixture("ending-ambiguous");
    let worktree = f.roots.data.join("wt-ambiguous");
    std::fs::create_dir_all(&worktree).unwrap();
    let findings = json!({
        "review:old": [{"id": "group-kill-incompatible-with-dash", "file": "src/old.rs", "line": 7}, {"id": "token-in-log", "file": "src/log.rs", "line": 3}],
        "review:new": [{"id": "group-kill-incompatible-with-dash", "file": "src/kill.rs", "line": 42}, {"id": "token-in-log", "file": "src/log.rs", "line": 3}]
    });
    std::fs::write(worktree.join(".tdd-review-findings.json"), findings.to_string()).unwrap();
    f.run("r-ambiguous", &worktree, &blocked_after_steps(ESCALATED));
    let detail = f.get("/api/projects/alpha/runs/r-ambiguous");
    assert_eq!(
        detail["run"]["ending_findings"],
        json!([
            {"id": "group-kill-incompatible-with-dash", "severity": "high", "category": "bugs"},
            {"id": "token-in-log", "severity": "medium", "category": "security", "file": "src/log.rs", "line": 3}
        ]),
        "places that disagree are left out, places that agree kept: {detail}"
    );
}

#[test]
fn a_finding_id_whose_scopes_name_one_file_at_different_lines_gets_no_place() {
    let f = fixture("ending-lines");
    let worktree = f.roots.data.join("wt-lines");
    std::fs::create_dir_all(&worktree).unwrap();
    let findings = json!({"review:old": [{"id": "token-in-log", "file": "src/log.rs", "line": 3}], "review:new": [{"id": "token-in-log", "file": "src/log.rs", "line": 30}]});
    std::fs::write(worktree.join(".tdd-review-findings.json"), findings.to_string()).unwrap();
    f.run("r-lines", &worktree, &blocked_after_steps(ESCALATED));
    let detail = f.get("/api/projects/alpha/runs/r-lines");
    assert_eq!(detail["run"]["ending_findings"][1], json!({"id": "token-in-log", "severity": "medium", "category": "security"}), "{detail}");
}

#[test]
fn a_linked_or_unreadable_findings_ledger_gives_no_place_but_keeps_the_reason_and_findings() {
    let f = fixture("ending-unreadable");
    let elsewhere = f.roots.data.join("elsewhere.json");
    std::fs::write(&elsewhere, json!({"review:x": [{"id": "token-in-log", "file": "secret/place.rs", "line": 1}]}).to_string()).unwrap();
    let linked = f.roots.data.join("wt-linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(&elsewhere, linked.join(".tdd-review-findings.json")).unwrap();
    let malformed = f.roots.data.join("wt-malformed");
    std::fs::create_dir_all(&malformed).unwrap();
    std::fs::write(malformed.join(".tdd-review-findings.json"), "{not json").unwrap();
    f.run("r-linked", &linked, &blocked_after_steps(ESCALATED));
    f.run("r-malformed", &malformed, &blocked_after_steps(ESCALATED));
    for id in ["r-linked", "r-malformed"] {
        let detail = f.get(&format!("/api/projects/alpha/runs/{id}"));
        assert_eq!(detail["run"]["ending_reason"], json!(ESCALATED), "{detail}");
        assert_eq!(detail["run"]["ending_findings"][1], json!({"id": "token-in-log", "severity": "medium", "category": "security"}), "{detail}");
    }
}

#[test]
fn merged_and_resumed_runs_give_no_ending_reason() {
    let f = fixture("ending-none");
    let worktree = f.roots.data.join("wt-none");
    f.run("r-merged", &worktree, &[("run.started", json!({})), ("run.completed", json!({"ending": "merged", "reason": "kept for the record"}))]);
    let mut resumed = blocked_after_steps(ESCALATED);
    resumed.push(("run.resumed", json!({"resumed_as": "r-next"})));
    resumed.push(("run.completed", json!({"ending": "resumed", "reason": "continued as r-next"})));
    f.run("r-resumed", &worktree, &resumed);
    for id in ["r-merged", "r-resumed"] {
        let detail = f.get(&format!("/api/projects/alpha/runs/{id}"));
        assert!(detail["run"].get("ending_reason").is_none(), "{detail}");
        assert!(detail["run"].get("ending_findings").is_none(), "{detail}");
    }
    let list = f.get("/api/projects/alpha/runs");
    assert!(list["runs"].as_array().unwrap().iter().all(|row| row.get("ending_reason").is_none()), "{list}");
}

#[test]
fn a_secret_in_the_ending_reason_is_redacted() {
    const GITHUB: &str = "ghp_abcdefghijklmnopqrstuvwx";
    let f = fixture("ending-secret");
    let worktree = f.roots.data.join("wt-secret");
    f.run("r-secret", &worktree, &[("run.started", json!({})), ("run.completed", json!({"ending": "failed", "reason": format!("push refused: GITHUB_TOKEN={GITHUB} is invalid")}))]);
    let detail = f.get("/api/projects/alpha/runs/r-secret");
    let list = f.get("/api/projects/alpha/runs");
    let shown = detail["run"]["ending_reason"].as_str().unwrap_or_default();
    assert!(shown.starts_with("push refused:") && shown.contains("[REDACTED]"), "{detail}");
    for body in [&detail, &list] {
        assert!(!body.to_string().contains(GITHUB), "{body}");
    }
}
