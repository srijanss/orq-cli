//! Resuming a run from its page says whether an answer went with it: the done note says "with your answer" or
//! "without an answer", the resumed run's page shows the answer it was given, a run blocked on "needs a
//! decision: ..." asks again before it is resumed with no answer, and what was typed is kept until it is sent.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 6] = ["[data-resume]", "[data-answer]", "textarea[data-answer]", "[data-confirm]", "[data-run-answer]", "[data-decision]"];

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

fn one(id: &str, state: &str) -> Value {
    json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"})
}

fn detail(row: Value, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let id = row["run_id"].as_str().unwrap().to_string();
    let body = json!({"run": row,"events": [], "next_cursor": null, "remote_actions": true});
    let mut responses = vec![json!({"status": 200, "json": body}), projects(), json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": format!("/projects/alpha/orchestrations/{id}"), "responses": responses, "steps": all, "selectors": SELECTORS}))
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

fn check() -> Value {
    json!({"status": 200, "json": {"next": "implement"}})
}

fn accepted() -> Value {
    json!({"status": 202, "json": {"run": "r-next"}})
}

#[test]
fn the_done_note_says_whether_the_resume_went_with_an_answer() {
    let Some(out) = detail(
        one("r-fail", "failed"),
        vec![check(), accepted()],
        vec![json!({"click": "[data-resume]"}), json!({"set": "[data-answer]", "value": "use the v2 endpoint"}), json!({"click": "[data-confirm]"})],
    ) else { return };
    assert!(shows(last(&out), "Resumed with your answer as run r-next."), "{:?}", texts(last(&out)));

    let Some(out) = detail(one("r-fail", "failed"), vec![check(), accepted()], vec![json!({"click": "[data-resume]"}), json!({"click": "[data-confirm]"})]) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/runs/r-fail/resume".to_string(), json!({}))]);
    assert!(shows(last(&out), "Resumed without an answer as run r-next."), "{:?}", texts(last(&out)));
}

#[test]
fn the_answer_typed_survives_a_refused_send_and_goes_with_the_next_one() {
    let refused = json!({"status": 409, "json": {"error": "alpha is at its limit of active runs"}});
    let Some(out) = detail(
        deciding(),
        vec![check(), refused, check(), accepted()],
        vec![
            json!({"click": "[data-resume]"}),
            json!({"set": "[data-answer]", "value": "drop it"}),
            json!({"click": "[data-confirm]"}),
            json!({"click": "[data-resume]"}),
            json!({"click": "[data-confirm]"}),
        ],
    ) else { return };
    let resume = "/api/projects/alpha/runs/r-block/resume".to_string();
    assert_eq!(posts(&out), vec![(resume.clone(), json!({"answer": "drop it"})), (resume, json!({"answer": "drop it"}))], "kept through the refusal and the check again");
    assert!(shows(&out["frames"][4], "alpha is at its limit of active runs"), "{:?}", texts(&out["frames"][4]));
    assert!(shows(last(&out), "Resumed with your answer as run r-next."), "{:?}", texts(last(&out)));
}

#[test]
fn the_resumed_runs_page_shows_the_answer_it_was_given_as_plain_text() {
    let mut row = one("r-next", "running");
    row["resumed_from"] = json!("r-block");
    row["answer"] = json!("drop it\n<img src=x onerror=alert(1)>");
    let Some(out) = detail(row, vec![], vec![]) else { return };
    let frame = last(&out);
    assert_eq!(frame["settled"]["matches"]["[data-run-answer]"], json!(1), "{out}");
    assert!(shows(frame, "Answered when resumed:"), "{:?}", texts(frame));
    assert!(texts(frame).iter().any(|t| t.contains("<img src=x onerror=alert(1)>")), "{:?}", texts(frame));
    assert_eq!(frame["settled"]["injected"], json!(0), "{out}");

    let mut row = one("r-next", "running");
    row["resumed_from"] = json!("r-block");
    let Some(out) = detail(row, vec![], vec![]) else { return };
    assert_eq!(last(&out)["settled"]["matches"]["[data-run-answer]"], json!(0), "no answer, nothing shown: {out}");
}

const GITHUB: &str = "ghp_abcdefghijklmnopqrstuvwx";

/// A project `alpha` with run `run`, whose first event is `run.launched` with `launched` as its payload,
/// and whose logs hold `answer` as the answer it was resumed with, when given.
fn resumed_run(label: &str, launched: Value, answer: Option<&str>) -> (orq_core::storage::StorageRoots, std::path::PathBuf) {
    use orq_orchestrator::{Event, RunRecord, RunStore};
    let base = web_deps::private_temp_dir(label);
    let roots = orq_core::storage::StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let scan = base.join("code");
    let alpha = scan.join("alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&alpha).status().unwrap().success());
    let project = orq_git::project_id(&orq_git::resolve_project(&alpha).unwrap());
    let store = RunStore::new(&roots.state);
    store
        .create(&RunRecord { run_id: "r-next".into(), project_id: project.clone(), feature_id: "f1".into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: "orq/f1".into(), worktree: scan.join("wt"), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    let event = Event { schema_version: 1, event_id: "r-next-0".into(), run_id: "r-next".into(), project_id: project, feature_id: "f1".into(), kind: "run.launched".into(), stage: None, attempt: 1, occurred_at: "2026-11-20T10:00:00Z".into(), payload: launched };
    store.append_event(&event).unwrap();
    if let Some(answer) = answer {
        let logs = roots.state.join("logs/r-next");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("resume-answer.log"), answer).unwrap();
    }
    (roots, scan)
}

fn run_detail(roots: &orq_core::storage::StorageRoots, scan: &std::path::Path) -> Value {
    use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
    let provider = orq_remote::projects::StoreProjects::new(roots.clone(), scan.to_path_buf());
    let request = Request { method: "GET", path: "/api/projects/alpha/runs/r-next", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let reply = handle_with(roots, &RemoteConfig { token: "tok".into() }, "server-project", "server", &request, &provider);
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.body
}

#[test]
fn a_run_resumed_with_an_answer_shows_it_redacted_and_one_resumed_without_shows_none() {
    let (roots, scan) = resumed_run("resume-answer-given", json!({"pid": 1, "answer": true}), Some(&format!("use the v2 endpoint\nGITHUB_TOKEN={GITHUB}")));
    let body = run_detail(&roots, &scan);
    let shown = body["run"]["answer"].as_str().unwrap_or_else(|| panic!("{body}"));
    assert!(shown.starts_with("use the v2 endpoint\n"), "{shown}");
    assert!(!body.to_string().contains(GITHUB), "redacted like other text: {body}");

    let (roots, scan) = resumed_run("resume-answer-blank", json!({"pid": 1, "answer": false}), Some("left over"));
    let body = run_detail(&roots, &scan);
    assert_eq!(body["run"].get("answer"), None, "a resume that recorded no answer shows none: {body}");

    let (roots, scan) = resumed_run("resume-answer-missing", json!({"pid": 1, "answer": true}), None);
    let body = run_detail(&roots, &scan);
    assert_eq!(body["run"].get("answer"), None, "{body}");
}

fn deciding() -> Value {
    let mut row = one("r-block", "blocked");
    row["ending_reason"] = json!("needs a decision: keep the v1 endpoint or drop it");
    row
}

#[test]
fn a_run_blocked_on_a_decision_asks_again_before_it_is_resumed_without_an_answer() {
    let Some(out) = detail(deciding(), vec![check(), accepted()], vec![json!({"click": "[data-resume]"}), json!({"click": "[data-confirm]"}), json!({"click": "[data-confirm]"})]) else { return };
    let asked = &out["frames"][2];
    assert_eq!(asked["settled"]["matches"]["[data-decision]"], json!(1), "the decision is shown above the field: {asked}");
    assert!(shows(asked, "needs a decision: keep the v1 endpoint or drop it"), "{:?}", texts(asked));
    assert_eq!(asked["settled"]["matches"]["textarea[data-answer]"], json!(1), "a multi-line field: {asked}");
    let again = &out["frames"][3];
    assert!(shows(again, "Resume without answering? The session will stop at the same decision."), "{:?}", texts(again));
    assert_eq!(again["settled"]["matches"]["[data-confirm]"], json!(1), "{again}");
    assert_eq!(posts(&out), vec![("/api/projects/alpha/runs/r-block/resume".to_string(), json!({}))], "sent on the second confirm only");
    assert!(shows(last(&out), "Resumed without an answer as run r-next."), "{:?}", texts(last(&out)));

    let Some(out) = detail(
        deciding(),
        vec![check(), accepted()],
        vec![json!({"click": "[data-resume]"}), json!({"set": "[data-answer]", "value": "drop it\nand say so in the changelog"}), json!({"click": "[data-confirm]"})],
    ) else { return };
    assert_eq!(posts(&out), vec![("/api/projects/alpha/runs/r-block/resume".to_string(), json!({"answer": "drop it\nand say so in the changelog"}))], "an answer is sent at once");

    let Some(out) = detail(one("r-fail", "failed"), vec![check()], vec![json!({"click": "[data-resume]"})]) else { return };
    assert_eq!(last(&out)["settled"]["matches"]["textarea[data-answer]"], json!(1), "other states keep the optional field, as a textarea: {out}");
}
