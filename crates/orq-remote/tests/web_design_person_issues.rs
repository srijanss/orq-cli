//! A person's design issues from the run page: a frontend run that ended needing review, or failed, in its
//! design review step offers Add design issues next to Review again. Its form posts the issues to
//! `POST /api/projects/{project}/runs/{run}/design-issues`, which checks them as `orq run resume
//! --design-issues` does and resumes the run with them, with the audit and checks of the run's other
//! actions. The design report lists a person's issue with a "From you" label in each later review.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, RunLauncher, StoreProjects};
use serde_json::{json, Value};

/// Resumes runs given a person's design issues, recording what it was given: the run and the issues' text.
#[derive(Default)]
struct Launcher {
    resumed: Mutex<Vec<(String, String)>>,
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, _feature: &str) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("not in this test".to_string()))
    }

    fn resume_with_design_issues(&self, _repo: &Path, run: &str, issues: &str) -> Result<String, LaunchError> {
        let mut resumed = self.resumed.lock().unwrap();
        resumed.push((run.to_string(), issues.to_string()));
        Ok(format!("{run}-r{}", resumed.len()))
    }
}

struct Setup {
    roots: StorageRoots,
    scan: PathBuf,
    launcher: Arc<Launcher>,
    project: String,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let scan = base.join("code");
    let path = scan.join("alpha");
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&path).status().unwrap().success());
    let project = orq_git::project_id(&orq_git::resolve_project(&path).unwrap());
    Setup { roots, scan, launcher: Arc::new(Launcher::default()), project }
}

/// Events as (kind, stage, payload).
fn add_run(s: &Setup, run: &str, events: &[(&str, &str, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord {
            run_id: run.into(),
            project_id: s.project.clone(),
            feature_id: "login".into(),
            remote: String::new(),
            provider: String::new(),
            target_branch: "main".into(),
            base_sha: "a".repeat(40),
            branch: format!("orq/{run}"),
            worktree: s.scan.join(run),
            repository: String::new(),
            profiles: Default::default(),
            delivery: Default::default(),
        })
        .unwrap();
    for (n, (kind, stage, payload)) in events.iter().enumerate() {
        store
            .append_event(&Event {
                schema_version: 1,
                event_id: format!("{run}-{n}"),
                run_id: run.into(),
                project_id: s.project.clone(),
                feature_id: "login".into(),
                kind: (*kind).into(),
                stage: Some((*stage).into()),
                attempt: 1,
                occurred_at: "2026-11-01T10:00:00Z".into(),
                payload: payload.clone(),
            })
            .unwrap();
    }
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A frozen package with a desktop and a mobile reference.
fn freeze_package(s: &Setup, run: &str) {
    let dir = s.roots.state.join("logs").join(run).join("design");
    let file = |path: &str, name: &str, width: u32, height: u32| json!({"path": path, "sha256": "0".repeat(64), "size": 1, "kind": "reference", "width": width, "height": height, "viewport": {"name": name, "width": width, "height": height}});
    let manifest = json!({
        "feature": "login",
        "package_sha256": "1".repeat(64),
        "files": [file("references/desktop.png", "desktop", 1440, 900), file("references/mobile.png", "mobile", 390, 844)],
        "unassigned": [],
        "missing": {"viewports": [], "documents": [], "assets": []},
        "warnings": [],
    });
    write(&dir.join("manifest.json"), manifest.to_string().as_bytes());
}

/// A frontend run whose design review step ended `ending` (needs_review or failed) after one round.
fn design_run(s: &Setup, run: &str, ending: &str) {
    add_run(
        s,
        run,
        &[
            ("stage.started", "design_review", json!({"round": 1, "of": 2})),
            ("design_review", "design_review", json!({"round": 1, "of": 2, "verdict": "changes_required", "head": "abc", "batches": 0, "issues": []})),
            ("run.completed", "design_review", json!({"ending": ending, "reason": "the design still requires changes"})),
        ],
    );
    freeze_package(s, run);
    add_review(s, run, 1, "changes_required", json!([issue("layout-1", "visual", "open")]));
}

fn send(s: &Setup, path: &str, body: &[u8], authorization: Option<&str>) -> (u16, Value) {
    let projects = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(s.launcher.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &projects);
    (r.status, r.body)
}

/// Round `round`'s review answer, giving `issues`.
fn add_review(s: &Setup, run: &str, round: u32, verdict: &str, issues: Value) {
    let dir = s.roots.state.join("logs").join(run).join("design").join(format!("round-{round}"));
    let review = json!({"run_id": run, "reference_hash": "1".repeat(64), "capture_revision": "abc", "evaluation": "compared", "verdict": verdict, "checks": [], "issues": issues, "suggestions": [], "blockers": []});
    let record = json!({"run_id": run, "round": round, "verdict": verdict, "reason": null, "decided_by": "design reviewer", "escalation": null, "review": review, "reviewer_review": null});
    write(&dir.join("review.json"), record.to_string().as_bytes());
}

fn issue(id: &str, basis: &str, status: &str) -> Value {
    json!({"id": id, "section": "Footer", "viewport": "all", "source": basis, "expected": format!("{id} expected"), "actual": format!("{id} actual"), "evidence": [], "severity": "major", "status": status, "fix_direction": format!("{id} fix"), "basis": basis})
}

fn get(s: &Setup, path: &str) -> Value {
    let projects = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = orq_core::remote_inbox::handle_with(&s.roots, &config, "server-project", "server", &request, &projects);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

/// A run whose second review reports the person's issue open and its own visual one.
fn reviewed_person_issue(s: &Setup) -> Value {
    let review = |round: u32, batches: u32| ("design_review", "design_review", json!({"round": round, "of": 3, "verdict": "changes_required", "head": "abc", "batches": batches, "issues": []}));
    add_run(
        s,
        "r-1",
        &[
            ("stage.started", "design_review", json!({"round": 1, "of": 3})),
            review(1, 0),
            ("stage.started", "design_review", json!({"round": 2, "of": 3})),
            review(2, 1),
            ("run.completed", "design_review", json!({"ending": "needs_review", "reason": "the design still requires changes"})),
        ],
    );
    freeze_package(s, "r-1");
    add_review(s, "r-1", 1, "changes_required", json!([issue("layout-1", "visual", "open")]));
    add_review(s, "r-1", 2, "changes_required", json!([issue("person-1", "person", "open"), issue("layout-1", "visual", "resolved")]));
    get(s, "/api/projects/alpha/runs/r-1")
}

fn post_issues(s: &Setup, run: &str, issues: Value) -> (u16, Value) {
    send(s, &format!("/api/projects/alpha/runs/{run}/design-issues"), json!({ "issues": issues }).to_string().as_bytes(), Some("Bearer tok"))
}

fn resumed(s: &Setup) -> Vec<(String, String)> {
    s.launcher.resumed.lock().unwrap().clone()
}

fn footer_issue() -> Value {
    json!({"section": "Footer", "viewport": "all", "expected": "the logo on a transparent background", "actual": "the logo sits on a white box", "fix_direction": "drop the logo's background", "severity": "major"})
}

// The run page itself, in jsdom, fed the run the server above answers.

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 10] = [
    "[data-design-issues]",
    "[data-design-issues-form]",
    "[data-person-issue]",
    "[data-person-issue='0'] [data-issue-viewport] option",
    "[data-issue-sections] option",
    "[data-design-issues-send]",
    "[data-design-issue='person-1'] [data-from-person]",
    "[data-design-issue='layout-1'] [data-from-person]",
    "[data-design-review]",
    "[data-resume]",
];

/// The run page's answer for `run`, as the server gives it with actions on, its references left out so the page
/// asks for no images.
fn run_page(s: &Setup, run: &str) -> Value {
    let mut body = get(s, &format!("/api/projects/alpha/runs/{run}"));
    body["remote_actions"] = json!(true);
    assert!(body["run"]["design"]["rounds"].is_array(), "{body}");
    for round in body["run"]["design"]["rounds"].as_array_mut().into_iter().flatten() {
        for viewport in round["viewports"].as_array_mut().into_iter().flatten() {
            viewport["reference"] = Value::Null;
        }
    }
    body
}

/// Opens the run page with `body`; the live follow gets a held stream, then `extra` answer what `steps` ask.
fn open(run: &str, body: Value, extra: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let projects = json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}});
    let mut responses = vec![json!({"status": 200, "json": body}), projects, json!({"status": 200, "stream": [], "end": "hold"})];
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": format!("/projects/alpha/orchestrations/{run}"), "responses": responses, "steps": all, "selectors": SELECTORS});
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} was not measured"))
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn the_action_shows_only_for_a_frontend_run_that_ended_in_its_design_review_step() {
    let s = setup("person-issues-shown");
    design_run(&s, "r-1", "needs_review");
    let needs_review = run_page(&s, "r-1");
    let mut failed = needs_review.clone();
    failed["run"]["state"] = json!("failed");
    let mut running = needs_review.clone();
    running["run"]["state"] = json!("running");
    let mut not_design = needs_review.clone();
    not_design["run"]["stage"] = json!("implement");
    not_design["run"].as_object_mut().unwrap().remove("design");

    let shown: Vec<Option<u64>> = [needs_review, failed, running, not_design].into_iter().map(|body| open("r-1", body, vec![], vec![]).map(|out| count(last(&out), "[data-design-issues]"))).collect();

    if shown.contains(&None) {
        return;
    }
    assert_eq!(shown, [Some(1), Some(1), Some(0), Some(0)]);
}

/// The steps that fill issue `at` of the form.
fn fill(at: usize, section: &str, viewport: &str, expected: &str, actual: &str, fix: &str, severity: Option<&str>) -> Vec<Value> {
    let field = |name: &str| format!("[data-person-issue='{at}'] [data-issue-{name}]");
    let mut steps = vec![
        json!({"set": field("section"), "value": section}),
        json!({"set": field("viewport"), "value": viewport}),
        json!({"set": field("expected"), "value": expected}),
        json!({"set": field("actual"), "value": actual}),
        json!({"set": field("fix"), "value": fix}),
    ];
    if let Some(severity) = severity {
        steps.push(json!({"set": field("severity"), "value": severity}));
    }
    steps
}

fn posts(out: &Value) -> Vec<(String, Value)> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").map(|r| (r["url"].as_str().unwrap().to_string(), r["body"].as_str().and_then(|body| serde_json::from_str(body).ok()).unwrap_or(Value::Null))).collect()
}

#[test]
fn the_form_posts_each_issue_and_says_which_run_corrects_them() {
    let s = setup("person-issues-form");
    design_run(&s, "r-1", "needs_review");
    let mut steps = vec![json!({"click": "[data-design-issues]"})];
    steps.extend(fill(0, "Footer", "all", "the logo on a transparent background", "the logo sits on a white box", "drop the logo's background", None));
    steps.push(json!({"click": "[data-design-issue-add]"}));
    steps.extend(fill(1, "Header", "mobile", "the menu folds", "the menu overflows", "", Some("critical")));
    steps.push(json!({"click": "[data-design-issue-add]"}));
    let third = steps.len() + 1;
    steps.push(json!({"click": "[data-design-issue-remove]", "index": 2}));
    steps.push(json!({"click": "[data-design-issues-send]"}));

    let Some(out) = open("r-1", run_page(&s, "r-1"), vec![json!({"status": 202, "json": {"run": "r-1-r1"}})], steps) else {
        return;
    };

    // Frame 0 is before the sign-in, frame 1 after it, and frame k + 2 after step k.
    let form = &out["frames"][2];
    assert_eq!(count(form, "[data-person-issue='0'] [data-issue-viewport] option"), 3, "All, desktop and mobile: {out}");
    assert_eq!(count(form, "[data-issue-sections] option"), 1, "the report's Footer: {out}");
    assert_eq!(count(&out["frames"][third], "[data-person-issue]"), 3, "{out}");
    let mobile = json!({"section": "Header", "viewport": "mobile", "expected": "the menu folds", "actual": "the menu overflows", "severity": "critical"});
    assert_eq!(posts(&out), [("/api/projects/alpha/runs/r-1/design-issues".to_string(), json!({"issues": [footer_issue(), mobile]}))], "{out}");
    let end = last(&out);
    assert!(texts(end).contains(&"Correcting your issues as run r-1-r1.".to_string()), "{out}");
    assert_eq!(count(end, "[data-design-issues-form]"), 0, "{out}");
}

/// The most an issue's expected, what's wrong or how to fix may hold, as `orq run resume --design-issues` checks it.
const ISSUE_TEXT_MAX: usize = 2000;

#[test]
fn the_form_lets_each_issue_text_hold_what_the_server_takes_and_no_more() {
    let s = setup("person-issues-text-limit");
    design_run(&s, "r-1", "needs_review");
    for field in ["expected", "actual", "fix_direction"] {
        let mut longest = footer_issue();
        longest[field] = json!("x".repeat(ISSUE_TEXT_MAX));
        let mut over = footer_issue();
        over[field] = json!("x".repeat(ISSUE_TEXT_MAX + 1));
        let (status, body) = post_issues(&s, "r-1", json!([over]));
        assert_eq!(status, 409, "{field} one over the limit: {body}");
        assert!(body["error"].as_str().unwrap().contains(field), "{field}: {body}");
        assert!(resumed(&s).is_empty(), "{field}");
        let (status, body) = post_issues(&s, "r-1", json!([longest]));
        assert_eq!(status, 202, "{field} at the limit: {body}");
        s.launcher.resumed.lock().unwrap().clear();
    }

    let code = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web-src/src/main.jsx")).unwrap();
    let form = code.split("function DesignIssuesAction(").nth(1).expect("DesignIssuesAction").split("\nfunction ").next().unwrap();
    let textareas: Vec<&str> = form.split("<textarea ").skip(1).map(|tag| tag.split("/>").next().unwrap()).collect();
    assert!(!textareas.is_empty(), "the form's textareas");
    for textarea in textareas {
        assert!(textarea.contains(&format!("maxLength={{{ISSUE_TEXT_MAX}}}")), "{textarea}");
    }
}

#[test]
fn a_refused_post_shows_its_reason_in_the_form_which_stays_open() {
    let s = setup("person-issues-refused");
    design_run(&s, "r-1", "needs_review");
    let mut steps = vec![json!({"click": "[data-design-issues]"})];
    steps.extend(fill(0, "Footer", "all", "", "the logo sits on a white box", "", None));
    steps.push(json!({"click": "[data-design-issues-send]"}));

    let Some(out) = open("r-1", run_page(&s, "r-1"), vec![json!({"status": 409, "json": {"error": "issue 1: its expected is empty"}})], steps) else {
        return;
    };

    let end = last(&out);
    assert!(texts(end).contains(&"issue 1: its expected is empty".to_string()), "{out}");
    assert_eq!(count(end, "[data-design-issues-form]"), 1, "{out}");
    assert_eq!(count(end, "[data-person-issue]"), 1, "the issue is kept: {out}");
}

#[test]
fn the_report_on_the_page_labels_a_persons_issue_from_you() {
    let s = setup("person-issues-label");
    reviewed_person_issue(&s);

    let Some(out) = open("r-1", run_page(&s, "r-1"), vec![], vec![]) else {
        return;
    };

    let end = last(&out);
    assert_eq!(count(end, "[data-design-issue='person-1'] [data-from-person]"), 1, "{out}");
    assert_eq!(count(end, "[data-design-issue='layout-1'] [data-from-person]"), 0, "{out}");
    assert!(texts(end).contains(&"From you".to_string()), "{out}");
}

#[test]
fn a_valid_post_resumes_the_run_with_the_issues() {
    let s = setup("person-issues-valid");
    design_run(&s, "r-1", "needs_review");
    let mobile = json!({"section": "Header", "viewport": "mobile", "expected": "the menu folds", "actual": "the menu overflows", "severity": "critical"});

    let (status, body) = post_issues(&s, "r-1", json!([footer_issue(), mobile]));

    assert_eq!((status, &body), (202, &json!({"run": "r-1-r1"})));
    let resumed = resumed(&s);
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert_eq!(resumed[0].0, "r-1");
    let given: Value = serde_json::from_str(&resumed[0].1).unwrap();
    assert_eq!(given, json!([footer_issue(), mobile]));
}

#[test]
fn an_invalid_post_is_refused_with_the_field_named_and_nothing_started() {
    let s = setup("person-issues-invalid");
    design_run(&s, "r-1", "failed");
    let mut no_expected = footer_issue();
    no_expected["expected"] = json!("  ");
    let mut tablet = footer_issue();
    tablet["viewport"] = json!("tablet");

    let (empty, empty_body) = post_issues(&s, "r-1", json!([no_expected]));
    let (unknown, unknown_body) = post_issues(&s, "r-1", json!([footer_issue(), tablet]));
    let (none, _) = post_issues(&s, "r-1", json!([]));
    let (not_a_list, _) = send(&s, "/api/projects/alpha/runs/r-1/design-issues", br#"{"issues": "the logo"}"#, Some("Bearer tok"));

    assert_eq!(empty, 409, "{empty_body}");
    assert!(empty_body["error"].as_str().unwrap().contains("issue 1") && empty_body["error"].as_str().unwrap().contains("expected"), "{empty_body}");
    assert_eq!(unknown, 409, "{unknown_body}");
    assert!(unknown_body["error"].as_str().unwrap().contains("issue 2") && unknown_body["error"].as_str().unwrap().contains("tablet"), "{unknown_body}");
    assert_eq!(none, 409);
    assert_eq!(not_a_list, 400);
    assert!(resumed(&s).is_empty(), "{:?}", resumed(&s));
}

#[test]
fn a_signed_out_post_is_refused_and_nothing_started() {
    let s = setup("person-issues-signed-out");
    design_run(&s, "r-1", "needs_review");
    let body = json!({ "issues": [footer_issue()] }).to_string();
    let path = "/api/projects/alpha/runs/r-1/design-issues";

    let (none, _) = send(&s, path, body.as_bytes(), None);
    let (wrong, _) = send(&s, path, body.as_bytes(), Some("Bearer nope"));

    assert_eq!((none, wrong), (401, 401));
    assert!(resumed(&s).is_empty(), "{:?}", resumed(&s));
}

#[test]
fn a_run_not_ended_in_its_design_review_step_is_refused_and_each_attempt_audited() {
    let s = setup("person-issues-not-design");
    add_run(&s, "plain", &[("stage.started", "implement", json!({})), ("run.completed", "implement", json!({"ending": "needs_review"}))]);
    add_run(&s, "running", &[("stage.started", "design_review", json!({"round": 1, "of": 2}))]);

    let (plain, plain_body) = post_issues(&s, "plain", json!([footer_issue()]));
    let (running, running_body) = post_issues(&s, "running", json!([footer_issue()]));
    let (unknown, _) = post_issues(&s, "nope", json!([footer_issue()]));

    assert_eq!(plain, 409, "{plain_body}");
    assert!(plain_body["error"].as_str().unwrap().contains("not a frontend run"), "{plain_body}");
    assert_eq!(running, 409, "{running_body}");
    assert_eq!(unknown, 404);
    assert!(resumed(&s).is_empty(), "{:?}", resumed(&s));
    let audit = orq_core::remote_audit::read(&s.roots, 10).unwrap();
    assert_eq!(audit.iter().filter(|line| line["action"] == "run.design_issues" && line["outcome"] == "refused").count(), 3, "{audit:?}");
    assert!(!audit.iter().any(|line| line.to_string().contains("the logo")), "the issues stay out of the audit: {audit:?}");
}

#[test]
fn the_report_marks_a_persons_issue_as_from_the_person() {
    let s = setup("person-issues-report");

    let body = reviewed_person_issue(&s);

    let rounds = body["run"]["design"]["rounds"].as_array().unwrap_or_else(|| panic!("{body}"));
    let later = rounds.iter().find(|round| round["round"] == 2).unwrap_or_else(|| panic!("{body}"));
    let issues = later["issues"].as_array().unwrap();
    assert_eq!((&issues[0]["id"], &issues[0]["status"], &issues[0]["from_person"]), (&json!("person-1"), &json!("open"), &json!(true)), "{later}");
    assert_eq!(issues[1]["from_person"], json!(false), "{later}");
}
