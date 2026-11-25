//! A run's Resume, Review again and Accept, on the run page and behind its row's ⋯, can be done now or
//! queued: "Queue resume" (with an optional answer for the session), "Queue review again" and "Queue
//! accept" add them to the project's queue, where they show as "Resume X", "Review again X" and
//! "Accept X", and as "No longer needed" once something else did them. Resume now takes an optional
//! answer too, and a failed run whose commits after its last review no review saw offers Review again.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 11] = [
    "[data-resume]",
    "[data-accept]",
    "[data-review-again]",
    "[data-queue-run-resume]",
    "[data-queue-run-review-again]",
    "[data-queue-run-accept]",
    "[data-answer]",
    "[data-confirm]",
    "[data-queue-entry]",
    "[data-queue-state=\"skipped\"]",
    "[data-open-run]",
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

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

/// A run in `state`; one that needs review has an escalated review to review again.
fn one(id: &str, state: &str) -> Value {
    let mut run = json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"});
    if state == "needs_review" {
        run["review_again_possible"] = json!(true);
    }
    run
}

fn detail(run: Value, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let id = run["run_id"].as_str().unwrap().to_string();
    let body = json!({"run": run, "events": [], "next_cursor": null, "remote_actions": true});
    let mut responses = vec![json!({"status": 200, "json": body}), projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run_page(format!("/projects/alpha/orchestrations/{id}"), responses, all)
}

fn run_page(url: String, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    run(json!({"url": url, "responses": responses, "steps": steps, "selectors": SELECTORS}))
}

fn runs_list() -> Option<Value> {
    let rows = [("r-fail", "failed"), ("r-paused", "paused"), ("r-blocked", "blocked"), ("r-review", "needs_review"), ("r-run", "running"), ("r-done", "completed")];
    let list = json!({"status": 200, "json": {"runs": rows.iter().map(|(id, state)| one(id, state)).collect::<Vec<_>>(), "next_cursor": null, "total": 6, "running": 1, "remote_actions": true}});
    run_page("/projects/alpha/orchestrations".into(), vec![list, projects()], vec![json!({"submit": TOKEN})])
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn shows(frame: &Value, wanted: &str) -> bool {
    texts(frame).iter().any(|t| t == wanted)
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn posts(out: &Value) -> Vec<(String, Value)> {
    out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "POST")
        .map(|r| (r["url"].as_str().unwrap().to_string(), serde_json::from_str(r["body"].as_str().unwrap_or("null")).unwrap()))
        .collect()
}

#[test]
fn each_row_offers_to_queue_what_it_can_do_now() {
    let Some(out) = runs_list() else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-run-resume]"), 3, "the paused, failed and blocked runs: {out}");
    assert_eq!(count(frame, "[data-queue-run-review-again]"), 1, "the run that needs review: {out}");
    assert_eq!(count(frame, "[data-queue-run-accept]"), 2, "the failed run and the one that needs review: {out}");
    assert!(shows(frame, "Queue resume") && shows(frame, "Queue review again") && shows(frame, "Queue accept"), "{:?}", texts(frame));
}

#[test]
fn a_queued_resume_carries_the_answer_typed_for_it_and_a_queued_accept_names_the_run() {
    let queued = || json!({"status": 200, "json": {"queue": {"entries": [], "count": 1, "paused": null}}});
    let Some(out) = detail(
        one("r-fail", "failed"),
        vec![queued()],
        vec![json!({"click": "[data-queue-run-resume]"}), json!({"set": "[data-answer]", "value": "  use the v2 endpoint "}), json!({"click": "[data-confirm]"})],
    ) else { return };
    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t == "Queue Resume r-fail? It starts when the project's limits and queue hours allow."), "{asked:?}");
    assert_eq!(posts(&out), vec![("/api/projects/alpha/queue/add".to_string(), json!({"action": "resume", "run": "r-fail", "answer": "use the v2 endpoint"}))]);
    assert!(shows(last(&out), "Queued Resume r-fail."), "{:?}", texts(last(&out)));

    let Some(out) = detail(one("r-review", "needs_review"), vec![queued()], vec![json!({"click": "[data-queue-run-accept]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/queue/add".to_string(), json!({"action": "accept", "run": "r-review"}))]);
    assert!(shows(last(&out), "Queued Accept r-review."), "{:?}", texts(last(&out)));

    let Some(out) = detail(one("r-review", "needs_review"), vec![queued()], vec![json!({"click": "[data-queue-run-review-again]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/queue/add".to_string(), json!({"action": "review_again", "run": "r-review"}))]);
}

#[test]
fn resume_now_sends_the_answer_typed_for_the_session_and_nothing_when_it_is_blank() {
    let check = json!({"status": 200, "json": {"next": "implement"}});
    let ok = json!({"status": 202, "json": {"run": "r-next"}});
    let Some(out) = detail(
        one("r-paused", "paused"),
        vec![check.clone(), ok.clone()],
        vec![json!({"click": "[data-resume]"}), json!({"set": "[data-answer]", "value": "use the v2 endpoint"}), json!({"click": "[data-confirm]"})],
    ) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/runs/r-paused/resume".to_string(), json!({"answer": "use the v2 endpoint"}))]);

    let Some(out) = detail(one("r-paused", "paused"), vec![check, ok], vec![json!({"click": "[data-resume]"}), json!({"set": "[data-answer]", "value": "   "}), json!({"click": "[data-confirm]"})]) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/runs/r-paused/resume".to_string(), json!({}))]);
}

#[test]
fn a_failed_run_with_commits_no_review_saw_offers_review_again_now() {
    let mut reviewable = one("r-fail", "failed");
    reviewable["reviewable_again"] = json!(true);
    let ok = json!({"status": 202, "json": {"run": "r-next"}});
    let Some(out) = detail(reviewable, vec![ok], vec![json!({"click": "[data-review-again]"}), json!({"click": "[data-confirm]"})]) else { return };
    let asked = texts(&out["frames"][2]);
    assert!(asked.iter().any(|t| t == "Review r-fail again? A session reviews the commits after its last review and fixes what that review finds."), "{asked:?}");
    assert_eq!(posts(&out), vec![("/api/projects/alpha/runs/r-fail/review-again".to_string(), json!({}))]);
    assert!(shows(last(&out), "Reviewing again as run r-next."), "{:?}", texts(last(&out)));

    let Some(out) = detail(one("r-fail", "failed"), vec![], vec![]) else { return };
    assert_eq!(count(last(&out), "[data-review-again]"), 0, "a failed run with nothing new to review: {out}");
}

fn waiting_for_files(id: &str, stage: &str) -> Value {
    let mut run = one(id, "design_files_needed");
    run["stage"] = json!(stage);
    run
}

#[test]
fn a_run_waiting_for_design_files_offers_resume_and_offers_accept_only_once_something_was_built() {
    let Some(out) = detail(waiting_for_files("r-files", "design_package"), vec![], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-resume]"), 1, "the header offers Resume: {out}");
    assert_eq!(count(frame, "[data-queue-run-resume]"), 1, "and Queue resume: {out}");
    assert_eq!(count(frame, "[data-accept]"), 0, "nothing was built, so nothing to accept: {out}");
    assert_eq!(count(frame, "[data-queue-run-accept]"), 0, "{out}");

    let Some(out) = detail(waiting_for_files("r-later", "implement_and_review"), vec![], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-resume]"), 1, "{out}");
    assert_eq!(count(frame, "[data-accept]"), 1, "earlier work was built, so it may be accepted: {out}");
    assert_eq!(count(frame, "[data-queue-run-accept]"), 1, "{out}");

    let rows = [waiting_for_files("r-files", "design_package"), waiting_for_files("r-later", "implement_and_review")];
    let list = json!({"status": 200, "json": {"runs": rows, "next_cursor": null, "total": 2, "running": 0, "remote_actions": true}});
    let Some(out) = run_page("/projects/alpha/orchestrations".into(), vec![list, projects()], vec![json!({"submit": TOKEN})]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-resume]"), 2, "each row offers Resume: {out}");
    assert_eq!(count(frame, "[data-queue-run-resume]"), 2, "{out}");
    assert_eq!(count(frame, "[data-accept]"), 1, "only the row with built work offers Accept: {out}");
    assert_eq!(count(frame, "[data-queue-run-accept]"), 1, "{out}");
}

#[test]
fn the_header_resume_of_a_run_waiting_for_design_files_checks_then_posts_to_the_resume_path() {
    let check = json!({"status": 200, "json": {"next": "design_package"}});
    let ok = json!({"status": 202, "json": {"run": "r-next"}});
    let Some(out) = detail(waiting_for_files("r-files", "design_package"), vec![check, ok], vec![json!({"click": "[data-resume]"}), json!({"click": "[data-confirm]"})]) else { return };
    let gets: Vec<String> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] != "POST").filter_map(|r| r["url"].as_str()).map(String::from).collect();
    assert!(gets.iter().any(|u| u == "/api/projects/alpha/runs/r-files/resume-check"), "{gets:?}");
    assert_eq!(posts(&out), vec![("/api/projects/alpha/runs/r-files/resume".to_string(), json!({}))]);
}

const DESIGN_SELECTORS: [&str; 6] = [
    "[data-review-again]",
    "[data-resume]",
    "[data-queue-run-review-again]",
    "[data-design-issues]",
    ".run-actions > .run-action:first-child > [data-design-issues].primary-button",
    "[data-design-stopped-note]",
];

const DESIGN_STOPPED_NOTE: &str = "The design review stopped this run. Add the issues it should fix and run another round, or accept or discard it.";

/// A design group run that ended needing review at its design review, stopped at its second member.
fn design_stopped(review_again_possible: bool) -> Value {
    let mut run = one("r-design", "needs_review");
    run["stage"] = json!("design_review");
    run["review_again_possible"] = json!(review_again_possible);
    let design = json!({"of": 3, "rounds": [{"round": 1, "viewports": [{"name": "desktop"}], "issues": [{"section": "header"}]}]});
    run["design_group"] = json!({"at": 2, "of": 3, "stopped": "home-header", "members": [
        {"feature": "home-hero", "state": "completed", "round": 1, "step": "design_review", "components": [], "design": design.clone()},
        {"feature": "home-header", "state": "needs_review", "round": 1, "step": "design_review", "components": [], "design": design},
    ]});
    run
}

fn design_page(shown: Value) -> Option<Value> {
    let id = shown["run_id"].as_str().unwrap().to_string();
    let body = json!({"run": shown, "events": [], "next_cursor": null, "remote_actions": true});
    let responses = vec![json!({"status": 200, "json": body}), projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    run(json!({"url": format!("/projects/alpha/orchestrations/{id}"), "responses": responses, "steps": [{"submit": TOKEN}], "selectors": DESIGN_SELECTORS}))
}

#[test]
fn a_run_its_design_review_stopped_offers_add_design_issues_first_and_no_review_again() {
    let Some(out) = design_page(design_stopped(false)) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-review-again]"), 0, "{out}");
    assert_eq!(count(frame, "[data-resume]"), 0, "no Review again in the header: {out}");
    assert_eq!(count(frame, "[data-queue-run-review-again]"), 0, "no Queue review again: {out}");
    assert_eq!(count(frame, ".run-actions > .run-action:first-child > [data-design-issues].primary-button"), 1, "Add design issues is the first, primary action: {out}");
    assert_eq!(count(frame, "[data-design-stopped-note]"), 1, "{out}");
    assert!(shows(frame, DESIGN_STOPPED_NOTE), "{:?}", texts(frame));

    let list = json!({"status": 200, "json": {"runs": [design_stopped(false)], "next_cursor": null, "total": 1, "running": 0, "remote_actions": true}});
    let Some(out) = run(json!({"url": "/projects/alpha/orchestrations", "responses": [list, projects()], "steps": [{"submit": TOKEN}], "selectors": DESIGN_SELECTORS})) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-resume]"), 0, "its row offers no Review again: {out}");
    assert_eq!(count(frame, "[data-queue-run-review-again]"), 0, "nor Queue review again: {out}");

    let Some(out) = design_page(design_stopped(true)) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-resume]"), 1, "with an escalated review Review again stays: {out}");
    assert_eq!(count(frame, "[data-queue-run-review-again]"), 1, "{out}");
    assert_eq!(count(frame, "[data-design-stopped-note]"), 0, "{out}");
    assert!(shows(frame, "Review again"), "{:?}", texts(frame));
}

#[test]
fn queued_run_actions_show_by_their_labels_and_one_no_longer_needed_says_so() {
    let entry = |feature: &str, kind: &str, label: &str, state: &str, reason: Value| {
        json!({"feature": feature, "kind": kind, "label": label, "target_run": null, "answer": null, "state": state, "run": null, "reason": reason, "added_at": 1, "added_by": "web", "implementer": null, "reviewer": null, "mr_url": null})
    };
    let queue = json!({"entries": [
        entry("COM-01", "start", "Start COM-01", "next", Value::Null),
        entry("resume:r-1", "resume", "Resume r-1", "queued", Value::Null),
        entry("accept:r-2", "accept", "Accept r-2", "skipped", json!("No longer needed: r-2 was discarded")),
    ], "count": 3, "paused": null});
    let reply = json!({"status": 200, "json": {"queue": queue, "remote_actions": true}});
    let Some(out) = run_page("/projects/alpha/queue".into(), vec![reply, projects()], vec![json!({"submit": TOKEN})]) else { return };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-queue-entry]"), 3, "{out}");
    for wanted in ["COM-01", "Resume r-1", "Accept r-2", "No longer needed", "No longer needed: r-2 was discarded"] {
        assert!(shows(frame, wanted), "{wanted}: {:?}", texts(frame));
    }
    assert_eq!(count(frame, "[data-queue-state=\"skipped\"]"), 1, "{out}");
}
