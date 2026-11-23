//! The run page of a frontend run shows its design review, read-only: each round's verdict, the reference
//! beside the capture per viewport, the issues by section, the suggestions and the checks not assessed.
//! Images come from a route that serves only the files the run's own design records list. A run waiting
//! for design files shows what is missing instead, with a Resume.

mod web_chromium;
mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-image-bytes";

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    Setup {
        scan: base.join("code"),
        roots,
    }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&path)
        .status()
        .unwrap()
        .success());
    path
}

fn project_key(path: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(path).unwrap())
}

/// Events as (kind, stage, attempt, payload).
fn add_run(s: &Setup, run: &str, project: &str, events: &[(&str, &str, u32, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord {
            run_id: run.into(),
            project_id: project.into(),
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
    for (n, (kind, stage, attempt, payload)) in events.iter().enumerate() {
        let event = Event {
            schema_version: 1,
            event_id: format!("{run}-{n}"),
            run_id: run.into(),
            project_id: project.into(),
            feature_id: "login".into(),
            kind: (*kind).into(),
            stage: Some((*stage).into()),
            attempt: *attempt,
            occurred_at: "2026-11-01T10:00:00Z".into(),
            payload: payload.clone(),
        };
        store.append_event(&event).unwrap();
    }
}

fn design_dir(s: &Setup, run: &str) -> PathBuf {
    s.roots.state.join("logs").join(run).join("design")
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A frozen package with a desktop and a mobile reference, and one approved asset.
fn freeze_package(s: &Setup, run: &str) {
    let dir = design_dir(s, run);
    write(&dir.join("package/references/desktop.png"), PNG);
    write(&dir.join("package/references/mobile.png"), PNG);
    write(&dir.join("package/assets/logo.png"), PNG);
    write(&dir.join("package/DESIGN.md"), b"# Design");
    let file = |path: &str, kind: &str, viewport: Option<(&str, u32, u32)>| json!({"path": path, "sha256": "0".repeat(64), "size": PNG.len(), "kind": kind, "width": viewport.map(|v| v.1), "height": viewport.map(|v| v.2), "viewport": viewport.map(|(name, width, height)| json!({"name": name, "width": width, "height": height}))});
    let manifest = json!({
        "feature": "login",
        "package_sha256": "1".repeat(64),
        "files": [file("references/desktop.png", "reference", Some(("desktop", 1440, 900))), file("references/mobile.png", "reference", Some(("mobile", 390, 844))), file("assets/logo.png", "asset", None), file("DESIGN.md", "document", None)],
        "unassigned": [],
        "missing": {"viewports": [], "documents": [], "assets": []},
        "warnings": [],
    });
    write(&dir.join("manifest.json"), manifest.to_string().as_bytes());
}

fn issue(id: &str, section: &str, severity: &str, status: &str) -> Value {
    json!({"id": id, "section": section, "viewport": "desktop", "source": "references/desktop.png", "expected": format!("{id} expected"), "actual": format!("{id} actual"), "evidence": [], "severity": severity, "status": status, "fix_direction": format!("{id} fix")})
}

/// Round `round`'s captures (both viewports at `/`) and its review answer.
fn add_round(s: &Setup, run: &str, round: u32, verdict: &str, issues: Value) {
    let dir = design_dir(s, run).join(format!("round-{round}"));
    write(&dir.join("desktop/index.png"), PNG);
    write(&dir.join("mobile/index.png"), PNG);
    let page = |viewport: &str, width: u32, height: u32| json!({"route": "/", "viewport": viewport, "viewport_width": width, "viewport_height": height, "image": format!("{viewport}/index.png"), "image_width": width, "image_height": height, "failure": null});
    let capture = json!({"round": round, "commit": "abc", "browser_version": "Chromium 140", "port": 4000, "pages": [page("desktop", 1440, 900), page("mobile", 390, 844)], "failures": []});
    write(&dir.join("capture.json"), capture.to_string().as_bytes());
    let review = json!({
        "run_id": run, "reference_hash": "1".repeat(64), "capture_revision": "abc", "evaluation": "compared", "verdict": verdict,
        "checks": [{"id": "layout", "required": true, "status": "pass", "evidence": []}, {"id": "hover-states", "required": false, "status": "not_assessed", "evidence": ["no hover in a static capture"]}],
        "issues": issues,
        "suggestions": ["Tighten the card shadow"],
        "blockers": if verdict == "blocked" { json!(["The mobile capture is blank"]) } else { json!([]) },
    });
    let record = json!({"run_id": run, "round": round, "verdict": verdict, "reason": null, "decided_by": "design reviewer", "escalation": null, "review": review, "reviewer_review": null});
    write(&dir.join("review.json"), record.to_string().as_bytes());
}

fn ask(s: &Setup, path: &str, authorization: Option<&str>) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method: "GET",
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization,
    };
    let r = handle_with(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        &provider,
    );
    (r.status, r.body)
}

fn page(s: &Setup, path: &str) -> Value {
    let (status, body) = ask(s, path, Some("Bearer tok"));
    assert_eq!(status, 200, "{body}");
    body
}

/// A frontend run with two review rounds: changes required, then a pass.
fn two_round_run(s: &Setup, run: &str) -> String {
    let alpha = git_repo(&s.scan, "alpha");
    let project = project_key(&alpha);
    add_run(
        s,
        run,
        &project,
        &[
            (
                "stage.started",
                "design_review",
                1,
                json!({"round": 1, "of": 4}),
            ),
            (
                "design_review",
                "design_review",
                1,
                json!({"round": 1, "of": 4, "verdict": "changes_required", "head": "abc"}),
            ),
            (
                "stage.started",
                "design_review",
                2,
                json!({"round": 2, "of": 4}),
            ),
            (
                "design_review",
                "design_review",
                2,
                json!({"round": 2, "of": 4, "verdict": "pass", "head": "def"}),
            ),
            (
                "stage.completed",
                "design_review",
                2,
                json!({"round": 2, "of": 4}),
            ),
        ],
    );
    freeze_package(s, run);
    add_round(
        s,
        run,
        1,
        "changes_required",
        json!([
            issue("button-colour", "Header", "major", "open"),
            issue("gap", "Footer", "minor", "open")
        ]),
    );
    add_round(
        s,
        run,
        2,
        "pass",
        json!([issue("button-colour", "Header", "major", "resolved")]),
    );
    project
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for at in 0..4 {
            out.push(if at <= chunk.len() {
                ALPHABET[(n >> (18 - 6 * at) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

fn image(s: &Setup, run: &str, path: &str) -> (u16, Value) {
    ask(
        s,
        &format!("/api/projects/alpha/runs/{run}/design/image?path={path}"),
        Some("Bearer tok"),
    )
}

#[test]
fn the_image_route_serves_a_reference_and_a_capture_the_runs_records_list() {
    let s = setup("design-image-served");
    two_round_run(&s, "r-1");

    for path in ["package/references/mobile.png", "round-2/desktop/index.png"] {
        let (status, body) = image(&s, "r-1", path);
        assert_eq!(status, 200, "{path}: {body}");
        assert_eq!(
            body,
            json!({"content_type": "image/png", "data": base64(PNG)}),
            "{path}"
        );
    }
}

#[test]
fn the_image_route_refuses_dot_dot_an_absolute_path_a_link_an_unlisted_file_another_runs_file_and_no_auth(
) {
    let s = setup("design-image-refused");
    let project = two_round_run(&s, "r-1");
    // Another run of the same project, with a third round r-1 never had.
    add_run(
        &s,
        "r-2",
        &project,
        &[(
            "design_review",
            "design_review",
            3,
            json!({"round": 3, "of": 4}),
        )],
    );
    freeze_package(&s, "r-2");
    add_round(&s, "r-2", 3, "pass", json!([]));
    // A run of another project.
    let beta = git_repo(&s.scan, "beta");
    add_run(
        &s,
        "r-3",
        &project_key(&beta),
        &[(
            "design_review",
            "design_review",
            1,
            json!({"round": 1, "of": 4}),
        )],
    );
    freeze_package(&s, "r-3");
    add_round(&s, "r-3", 1, "pass", json!([]));
    // A capture swapped for a link to a file outside the run, still listed in its capture record.
    let outside = s.roots.state.join("outside.png");
    write(&outside, PNG);
    let linked = design_dir(&s, "r-1").join("round-1/mobile/index.png");
    std::fs::remove_file(&linked).unwrap();
    std::os::unix::fs::symlink(&outside, &linked).unwrap();
    // A file in the run's folder that no record lists.
    write(
        &design_dir(&s, "r-1").join("round-1/desktop/extra.png"),
        PNG,
    );

    let absolute = design_dir(&s, "r-1").join("round-1/desktop/index.png");
    for path in [
        "package/../round-1/desktop/index.png".to_string(),
        "../r-2/design/round-3/desktop/index.png".to_string(),
        absolute.display().to_string(),
        "/round-1/desktop/index.png".to_string(),
        "round-1/mobile/index.png".to_string(),
        "round-1/desktop/extra.png".to_string(),
        "round-3/desktop/index.png".to_string(),
        "manifest.json".to_string(),
        String::new(),
    ] {
        let (status, body) = image(&s, "r-1", &path);
        assert_eq!(status, 404, "{path:?} was served: {body}");
    }
    let (status, _) = ask(
        &s,
        "/api/projects/alpha/runs/r-3/design/image?path=round-1/desktop/index.png",
        Some("Bearer tok"),
    );
    assert_eq!(
        status, 404,
        "another project's run is not served under alpha"
    );
    assert_eq!(
        ask(
            &s,
            "/api/projects/alpha/runs/r-1/design/image",
            Some("Bearer tok")
        )
        .0,
        404,
        "no path"
    );
    for authorization in [None, Some("Bearer wrong")] {
        let (status, body) = ask(
            &s,
            "/api/projects/alpha/runs/r-1/design/image?path=round-2/desktop/index.png",
            authorization,
        );
        assert_eq!(status, 401, "{authorization:?}: {body}");
    }
}

/// A run that stopped for want of design files, as the pipeline records it.
fn files_needed_run(s: &Setup, run: &str) {
    let alpha = git_repo(&s.scan, "alpha");
    let files = json!([
        {"what": "the mobile reference, 390x844 (PNG, JPEG, WebP or PDF)", "path": "designs/login/references/mobile.png"},
        {"what": "DESIGN.md", "path": "designs/login/DESIGN.md"},
        {"what": "the asset DESIGN.md names", "path": "designs/login/assets/logo.svg"},
    ]);
    add_run(
        s,
        run,
        &project_key(&alpha),
        &[
            ("stage.started", "design_package", 1, json!({})),
            (
                "design.files_needed",
                "design_package",
                1,
                json!({"feature": "login", "files": files}),
            ),
            (
                "run.completed",
                "design_package",
                1,
                json!({"ending": "design_files_needed"}),
            ),
        ],
    );
}

#[test]
fn a_run_waiting_for_design_files_lists_each_with_its_path_why_and_the_size_of_a_reference() {
    let s = setup("design-files-needed");
    files_needed_run(&s, "r-1");

    let body = page(&s, "/api/projects/alpha/runs/r-1");

    let needed = &body["run"]["design_files_needed"];
    assert_eq!(body["run"]["state"], json!("design_files_needed"), "{body}");
    assert_eq!(needed["feature"], json!("login"), "{body}");
    let files = needed["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files: {body}"));
    assert_eq!(
        files
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "designs/login/references/mobile.png",
            "designs/login/DESIGN.md",
            "designs/login/assets/logo.svg"
        ],
        "{body}"
    );
    assert_eq!(
        files[0]["viewport"],
        json!({"name": "mobile", "width": 390, "height": 844}),
        "{body}"
    );
    assert_eq!(files[1]["viewport"], Value::Null, "{body}");
    assert!(
        files
            .iter()
            .all(|f| f["what"].is_string() && f["why"].as_str().is_some_and(|why| !why.is_empty())),
        "{body}"
    );
    assert_ne!(
        files[0]["why"], files[1]["why"],
        "a reference and a document are needed for different reasons: {body}"
    );
}

#[test]
fn the_run_page_carries_each_design_round_with_its_viewports_issues_suggestions_and_unassessed_checks(
) {
    let s = setup("design-review-rounds");
    two_round_run(&s, "r-1");

    let body = page(&s, "/api/projects/alpha/runs/r-1");

    let design = &body["run"]["design"];
    assert_eq!(design["of"], json!(4), "{design}");
    let rounds = design["rounds"]
        .as_array()
        .unwrap_or_else(|| panic!("no rounds: {body}"));
    assert_eq!(
        rounds
            .iter()
            .map(|r| (r["round"].clone(), r["verdict"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!(1), json!("changes_required")),
            (json!(2), json!("pass"))
        ],
        "{design}"
    );
    let first = &rounds[0];
    assert_eq!(
        first["viewports"],
        json!([
            {"name": "desktop", "width": 1440, "height": 900, "reference": "package/references/desktop.png", "captures": [{"route": "/", "image": "round-1/desktop/index.png", "failure": null}]},
            {"name": "mobile", "width": 390, "height": 844, "reference": "package/references/mobile.png", "captures": [{"route": "/", "image": "round-1/mobile/index.png", "failure": null}]},
        ]),
        "{design}"
    );
    assert_eq!(
        first["issues"][0],
        json!({"id": "button-colour", "section": "Header", "viewport": "desktop", "severity": "major", "status": "open", "expected": "button-colour expected", "actual": "button-colour actual", "fix_direction": "button-colour fix"}),
        "{design}"
    );
    assert_eq!(first["issues"].as_array().unwrap().len(), 2, "{design}");
    assert_eq!(
        first["suggestions"],
        json!(["Tighten the card shadow"]),
        "{design}"
    );
    assert_eq!(
        first["not_assessed"],
        json!([{"id": "hover-states", "evidence": ["no hover in a static capture"]}]),
        "{design}"
    );
    assert_eq!(
        rounds[1]["issues"][0]["status"],
        json!("resolved"),
        "{design}"
    );
}

// The page itself, in jsdom, fed the run page the server above answers.

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 15] = [
    "[data-design-review]",
    "[data-design-viewport]",
    "[data-design-reference]",
    "[data-design-capture]",
    "[data-design-section]",
    "[data-design-issue]",
    "[data-design-suggestions]",
    "[data-design-not-assessed]",
    "[data-design-blockers]",
    "[data-design-pick]",
    "[data-design-zoom]",
    "[data-design-files-needed]",
    "[data-design-needed-file]",
    "[data-design-files-needed] [data-resume]",
    "img[src^='blob:']",
];

fn harness(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node")
        .arg(format!("{dir}/tests/web_react_harness.mjs"))
        .arg(format!("{dir}/web/app.react.js"))
        .arg(scenario.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

/// Opens the run page of `run` with `body` as its answer; `images` answer the image requests the page
/// makes after it is drawn, then the live follow gets a held stream, then `extra` answer what `steps` ask.
fn open(
    run: &str,
    body: Value,
    images: Vec<Value>,
    extra: Vec<Value>,
    steps: Vec<Value>,
) -> Option<Value> {
    let mut responses = vec![json!({"status": 200, "json": body}), projects()];
    responses.extend(images);
    responses.push(json!({"status": 200, "stream": [], "end": "hold"}));
    responses.extend(extra);
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    harness(
        json!({"url": format!("/projects/alpha/orchestrations/{run}"), "responses": responses, "steps": all, "selectors": SELECTORS}),
    )
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector]
        .as_u64()
        .unwrap_or_else(|| panic!("{selector} was not measured"))
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .map(String::from)
        .collect()
}

fn position(texts: &[String], text: &str) -> usize {
    texts
        .iter()
        .position(|t| t == text)
        .unwrap_or_else(|| panic!("{text:?} is not shown: {texts:?}"))
}

/// A frontend run with one review round of `verdict` and `issues`.
fn one_round_run(s: &Setup, run: &str, verdict: &str, issues: Value) {
    let alpha = git_repo(&s.scan, "alpha");
    add_run(
        s,
        run,
        &project_key(&alpha),
        &[(
            "design_review",
            "design_review",
            1,
            json!({"round": 1, "of": 4, "verdict": verdict}),
        )],
    );
    freeze_package(s, run);
    add_round(s, run, 1, verdict, issues);
}

#[test]
fn a_changes_required_round_shows_its_verdict_viewports_issues_by_section_then_suggestions_and_unassessed_checks(
) {
    let s = setup("design-panel-changes");
    let mut regressed = issue("spacing", "Header", "minor", "regressed");
    regressed["viewport"] = json!("mobile");
    one_round_run(
        &s,
        "r-1",
        "changes_required",
        json!([
            issue("button-colour", "Header", "major", "open"),
            issue("gap", "Footer", "critical", "open"),
            regressed
        ]),
    );
    let body = page(&s, "/api/projects/alpha/runs/r-1");

    let Some(out) = open("r-1", body, vec![], vec![], vec![]) else {
        return;
    };

    let end = last(&out);
    assert_eq!(count(end, "[data-design-review]"), 1, "{out}");
    let shown = texts(end);
    for text in [
        "Design review",
        "Changes required",
        "Round 1 of 4",
        "Expected",
        "Actual",
        "Fix direction",
        "button-colour expected",
        "button-colour actual",
        "button-colour fix",
        "Major",
        "Critical",
        "Minor",
        "Open",
        "Regressed",
    ] {
        position(&shown, text);
    }
    assert_eq!(count(end, "[data-design-viewport]"), 2, "{out}");
    assert_eq!(count(end, "[data-design-reference]"), 2, "{out}");
    assert_eq!(count(end, "[data-design-capture]"), 2, "{out}");
    // Grouped by section in the order the review names them: both Header issues, then the Footer one.
    assert_eq!(count(end, "[data-design-section]"), 2, "{out}");
    assert_eq!(count(end, "[data-design-issue]"), 3, "{out}");
    let header = position(&shown, "Header");
    let footer = position(&shown, "Footer");
    assert!(
        header < position(&shown, "button-colour expected")
            && position(&shown, "spacing expected") < footer
            && footer < position(&shown, "gap expected"),
        "{shown:?}"
    );
    // Then the suggestions, then the checks the reviewer could not assess.
    let suggestion = position(&shown, "Tighten the card shadow");
    assert!(position(&shown, "gap fix") < suggestion, "{shown:?}");
    assert!(
        suggestion < position(&shown, "hover-states: no hover in a static capture"),
        "{shown:?}"
    );
    assert_eq!(count(end, "[data-design-not-assessed]"), 1, "{out}");
    assert_eq!(count(end, "[data-design-blockers]"), 0, "{out}");
    assert_eq!(count(end, "[data-design-files-needed]"), 0, "{out}");
}

#[test]
fn a_passing_round_shows_pass_and_no_issues() {
    let s = setup("design-panel-pass");
    one_round_run(&s, "r-1", "pass", json!([]));
    let body = page(&s, "/api/projects/alpha/runs/r-1");

    let Some(out) = open("r-1", body, vec![], vec![], vec![]) else {
        return;
    };

    let end = last(&out);
    let shown = texts(end);
    position(&shown, "Pass");
    position(&shown, "Round 1 of 4");
    position(&shown, "No issues.");
    assert_eq!(count(end, "[data-design-issue]"), 0, "{out}");
    assert_eq!(count(end, "[data-design-viewport]"), 2, "{out}");
}

#[test]
fn a_blocked_round_shows_blocked_and_what_blocked_it() {
    let s = setup("design-panel-blocked");
    one_round_run(&s, "r-1", "blocked", json!([]));
    let body = page(&s, "/api/projects/alpha/runs/r-1");

    let Some(out) = open("r-1", body, vec![], vec![], vec![]) else {
        return;
    };

    let end = last(&out);
    let shown = texts(end);
    position(&shown, "Blocked");
    position(&shown, "The mobile capture is blank");
    assert_eq!(count(end, "[data-design-blockers]"), 1, "{out}");
}

#[test]
fn the_latest_round_shows_first_and_an_older_one_opens_from_the_round_selector() {
    let s = setup("design-panel-rounds");
    two_round_run(&s, "r-1");
    let body = page(&s, "/api/projects/alpha/runs/r-1");

    let Some(out) = open(
        "r-1",
        body,
        vec![],
        vec![],
        vec![json!({"click": "[data-design-pick='1']"})],
    ) else {
        return;
    };

    let before = &out["frames"][1];
    assert_eq!(count(before, "[data-design-pick]"), 2, "{out}");
    position(&texts(before), "Round 2 of 4");
    position(&texts(before), "Pass");
    assert_eq!(count(before, "[data-design-issue]"), 1, "{out}");
    let after = last(&out);
    position(&texts(after), "Round 1 of 4");
    position(&texts(after), "Changes required");
    assert_eq!(count(after, "[data-design-issue]"), 2, "{out}");
}

#[test]
fn the_images_come_from_the_image_route_with_the_token_and_one_opens_large_when_tapped() {
    let s = setup("design-panel-images");
    one_round_run(&s, "r-1", "pass", json!([]));
    let body = page(&s, "/api/projects/alpha/runs/r-1");
    let png = json!({"status": 200, "json": {"content_type": "image/png", "data": base64(PNG)}});

    let Some(out) = open(
        "r-1",
        body,
        vec![png.clone(), png.clone(), png.clone(), png],
        vec![],
        vec![json!({"click": "[data-design-reference] button"})],
    ) else {
        return;
    };

    let asked: Vec<&Value> = out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["url"].as_str().unwrap().contains("/design/image"))
        .collect();
    let paths: Vec<&str> = asked.iter().map(|r| r["url"].as_str().unwrap()).collect();
    for path in [
        "package%2Freferences%2Fdesktop.png",
        "round-1%2Fdesktop%2Findex.png",
        "package%2Freferences%2Fmobile.png",
        "round-1%2Fmobile%2Findex.png",
    ] {
        assert!(
            paths.contains(
                &format!("/api/projects/alpha/runs/r-1/design/image?path={path}").as_str()
            ),
            "{path} not asked: {paths:?}"
        );
    }
    assert!(
        asked
            .iter()
            .all(|r| r["headers"]["Authorization"] == format!("Bearer {TOKEN}")),
        "{out}"
    );
    let before = &out["frames"][1];
    assert_eq!(count(before, "img[src^='blob:']"), 4, "{out}");
    assert_eq!(count(before, "[data-design-zoom]"), 0, "{out}");
    assert_eq!(count(last(&out), "[data-design-zoom]"), 1, "{out}");
}

/// A live design review event, as the run's event stream sends it.
fn live_design_event(body: &Value, id: &str) -> Value {
    let mut event = body["events"][0].clone();
    event["event_id"] = json!(id);
    json!({"status": 200, "stream": [format!("id: {id}\nevent: event\ndata: {event}\n\n")], "end": "hold"})
}

#[test]
fn a_design_review_round_that_ends_while_the_page_is_open_shows_without_a_reload() {
    let s = setup("design-panel-live");
    one_round_run(&s, "r-1", "changes_required", json!([issue("gap", "Footer", "critical", "open")]));
    let after = page(&s, "/api/projects/alpha/runs/r-1");
    let mut before = after.clone();
    before["run"]["design"] = Value::Null;

    let responses = vec![json!({"status": 200, "json": before}), projects(), live_design_event(&after, "r-1-9"), json!({"status": 200, "json": after})];
    let Some(out) = harness(json!({"url": "/projects/alpha/orchestrations/r-1", "responses": responses, "steps": [{"submit": TOKEN}], "selectors": SELECTORS})) else {
        return;
    };

    let end = last(&out);
    assert_eq!(count(end, "[data-design-review]"), 1, "{out}");
    position(&texts(end), "Changes required");
    position(&texts(end), "gap expected");
}

#[test]
fn a_new_round_that_arrives_live_is_the_one_shown() {
    let s = setup("design-panel-live-round");
    two_round_run(&s, "r-1");
    let before = page(&s, "/api/projects/alpha/runs/r-1");
    let mut after = before.clone();
    let mut third = after["run"]["design"]["rounds"][0].clone();
    third["round"] = json!(3);
    third["verdict"] = json!("blocked");
    after["run"]["design"]["rounds"].as_array_mut().unwrap().push(third);

    // The shown round's four images are asked for before the live stream opens.
    let missing = json!({"status": 404, "json": {"error": "not found"}});
    let mut responses = vec![json!({"status": 200, "json": before}), projects()];
    responses.extend(std::iter::repeat_n(missing, 4));
    responses.extend([live_design_event(&after, "r-1-9"), json!({"status": 200, "json": after})]);
    let Some(out) = harness(json!({"url": "/projects/alpha/orchestrations/r-1", "responses": responses, "steps": [{"submit": TOKEN}], "selectors": SELECTORS})) else {
        return;
    };

    let end = last(&out);
    assert_eq!(count(end, "[data-design-pick]"), 3, "{out}");
    position(&texts(end), "Round 3 of 4");
    position(&texts(end), "Blocked");
}

#[test]
fn a_run_waiting_for_design_files_shows_what_is_needed_where_and_why_with_resume_instead_of_a_review(
) {
    let s = setup("design-panel-files-needed");
    files_needed_run(&s, "r-1");
    let mut body = page(&s, "/api/projects/alpha/runs/r-1");
    body["remote_actions"] = json!(true);
    let checked = json!({"status": 200, "json": {"run": "r-1", "next": "design_package"}});
    let ok = json!({"status": 202, "json": {"run": "r-1"}});

    let Some(out) = open(
        "r-1",
        body,
        vec![],
        vec![checked, ok],
        vec![
            json!({"click": "[data-design-files-needed] [data-resume]"}),
            json!({"click": "[data-confirm]"}),
        ],
    ) else {
        return;
    };

    let before = &out["frames"][1];
    assert_eq!(count(before, "[data-design-files-needed]"), 1, "{out}");
    assert_eq!(count(before, "[data-design-review]"), 0, "{out}");
    assert_eq!(count(before, "[data-design-needed-file]"), 3, "{out}");
    let shown = texts(before);
    for text in [
        "Design files needed",
        "designs/login/references/mobile.png",
        "designs/login/DESIGN.md",
        "designs/login/assets/logo.svg",
        "Viewport: 390 × 844",
    ] {
        position(&shown, text);
    }
    assert_eq!(
        count(before, "[data-design-files-needed] [data-resume]"),
        1,
        "{out}"
    );
    let posts: Vec<&Value> = out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "POST")
        .collect();
    assert_eq!(
        posts.iter().map(|r| r["url"].clone()).collect::<Vec<_>>(),
        [json!("/api/projects/alpha/runs/r-1/resume")],
        "{out}"
    );
}

#[test]
fn the_page_policy_lets_images_show_from_the_blobs_the_page_makes_and_from_nowhere_else() {
    use std::io::{Read, Write};
    let s = setup("design-image-policy");
    let state = std::sync::Arc::new(orq_remote::ServerState {
        roots: s.roots.clone(),
        config: RemoteConfig {
            token: "tok".into(),
        },
        project_id: "p-0a1b2c3d".into(),
        project_name: "orq-cli".into(),
    });
    let server =
        orq_remote::Server::start(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), state)
            .unwrap();
    let mut stream = std::net::TcpStream::connect(server.addr()).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let reply = String::from_utf8_lossy(&raw).to_string();
    let policy = reply
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with("content-security-policy:")
                .then(|| line.split_once(':').unwrap().1.trim().to_string())
        })
        .unwrap_or_else(|| panic!("no policy: {reply}"));
    let images: Vec<&str> = policy
        .split(';')
        .map(str::trim)
        .filter(|part| part.starts_with("img-src"))
        .collect();
    assert_eq!(images, ["img-src 'self' blob:"], "{policy}");
}

/// A real 1x1 PNG, so Chromium lays the images out as images.
const REAL_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

fn laid_out(chromium: &Path, width: u32, body: &Value, wait: &str, boxes: &[&str]) -> Value {
    let replies = [
        web_chromium::get("/api/projects/alpha/runs/r-1", body.clone()),
        web_chromium::get("/api/projects", projects()["json"].clone()),
        web_chromium::get(
            "/api/projects/alpha/runs/r-1/design/image*",
            json!({"content_type": "image/png", "data": REAL_PNG}),
        ),
    ];
    let mut steps = vec![json!({"wait": wait})];
    steps.extend(boxes.iter().map(|selector| json!({"boxes": selector})));
    let measured = web_chromium::measure(
        chromium,
        width,
        "/projects/alpha/orchestrations/r-1",
        &replies,
        Value::Array(steps),
    )
    .result;
    assert!(measured.get("error").is_none(), "{measured}");
    assert!(
        measured["scrollWidth"].as_u64() <= measured["clientWidth"].as_u64(),
        "the page scrolls sideways at {width}: {measured}"
    );
    assert_eq!(
        measured["offenders"],
        json!([]),
        "text past the edge at {width}: {measured}"
    );
    measured
}

#[test]
fn at_1280_the_reference_sits_beside_its_capture_and_at_390_above_it_with_no_sideways_scroll() {
    let Some(chromium) = web_chromium::binary() else {
        return;
    };
    let s = setup("design-panel-layout");
    one_round_run(
        &s,
        "r-1",
        "changes_required",
        json!([issue("button-colour", "Header", "major", "open")]),
    );
    let body = page(&s, "/api/projects/alpha/runs/r-1");
    let pair = ["[data-design-reference] img", "[data-design-capture] img"];

    let wide = laid_out(&chromium, 1280, &body, "[data-design-capture] img", &pair);
    let (reference, capture) = (&wide["boxes"][pair[0]][0], &wide["boxes"][pair[1]][0]);
    assert_eq!(
        reference["top"], capture["top"],
        "side by side at 1280: {wide}"
    );
    assert!(
        reference["right"].as_i64() <= capture["left"].as_i64(),
        "the reference comes first, beside the capture: {wide}"
    );

    let narrow = laid_out(&chromium, 390, &body, "[data-design-capture] img", &pair);
    let (reference, capture) = (&narrow["boxes"][pair[0]][0], &narrow["boxes"][pair[1]][0]);
    assert!(
        reference["bottom"].as_i64() <= capture["top"].as_i64(),
        "one above the other at 390: {narrow}"
    );
    assert!(capture["right"].as_i64() <= Some(390), "{narrow}");
}

#[test]
fn the_files_needed_panel_fits_a_phone() {
    let Some(chromium) = web_chromium::binary() else {
        return;
    };
    let s = setup("design-files-needed-layout");
    files_needed_run(&s, "r-1");
    let mut body = page(&s, "/api/projects/alpha/runs/r-1");
    body["remote_actions"] = json!(true);

    laid_out(&chromium, 390, &body, "[data-design-needed-file]", &[]);
}

#[test]
fn without_actions_the_files_needed_panel_offers_no_resume() {
    let s = setup("design-panel-files-needed-quiet");
    files_needed_run(&s, "r-1");
    let mut body = page(&s, "/api/projects/alpha/runs/r-1");
    body["remote_actions"] = json!(false);

    let Some(out) = open("r-1", body, vec![], vec![], vec![]) else {
        return;
    };

    assert_eq!(count(last(&out), "[data-design-files-needed]"), 1, "{out}");
    assert_eq!(
        count(last(&out), "[data-design-files-needed] [data-resume]"),
        0,
        "{out}"
    );
}
