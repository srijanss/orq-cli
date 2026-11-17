//! Long unbroken words (paths, hashes, long feature names) wrap inside their boxes instead of widening
//! the page, measured on the built page in a real headless Chromium.

mod web_chromium;

use serde_json::{json, Value};
use std::path::Path;
use web_chromium::{get, measure, measure_with, Reply};

const PHONES: [u32; 2] = [360, 390];

fn name() -> String {
    format!("feature-{}", "n".repeat(82))
}

fn word() -> String {
    "w".repeat(200)
}

fn path() -> String {
    format!("crates/{}.rs", "p".repeat(111))
}

fn projects() -> Reply {
    get("/api/projects", json!({"projects": [{"id": "demo", "name": "demo", "available": true, "features": 2, "active_runs": 1, "reviews_waiting": 1, "failed_runs": 0, "failed_window_hours": 24}]}))
}

fn feature(description: &str) -> Value {
    json!({"name": name(), "description": description, "status": "pending", "depends_on": [format!("other-{}", "d".repeat(84))], "recorded_at": null, "test_file": path(), "target_files": [path()]})
}

fn features(description: &str) -> Vec<Reply> {
    let fields = json!({"fields": {"name": name(), "description": description, "depends_on": [], "parallelizable": null, "test_file": path(), "target_files": [path()]}, "version": "v1"});
    vec![
        projects(),
        get("/api/projects/demo/features", json!({"features": [feature(description), {"name": format!("other-{}", "d".repeat(84)), "description": word(), "status": "completed", "depends_on": [], "recorded_at": null}], "remote_actions": true})),
        get(&format!("/api/projects/demo/features/{}/fields", name()), fields),
        get("/api/projects/demo/runs*", json!({"runs": [], "next_cursor": null, "total": 0, "running": 0})),
    ]
}

/// The page fits the screen: no sideways scroll, and no text element ends past the right edge.
fn assert_fits(chromium: &Path, width: u32, url: &str, replies: &[Reply], steps: Value) {
    let out = measure(chromium, width, url, replies, steps.clone());
    let result = &out.result;
    assert!(result.get("error").is_none(), "{url} at {width} px, steps {steps}: {result}\nunanswered: {:?}", out.unanswered);
    assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{url} at {width} px, steps {steps}: the page scrolls sideways: {result}");
    assert_eq!(result["offenders"], json!([]), "{url} at {width} px, steps {steps}: text runs past the screen");
}

fn feature_url() -> String {
    format!("/projects/demo/features/{}", name())
}

#[test]
fn the_feature_detail_page_fits_a_phone_on_every_tab() {
    let Some(chromium) = web_chromium::binary() else { return };
    let replies = features(&format!("Has {} in it", word()));
    for width in PHONES {
        for tab in ["Description", "Test file", "Dependencies", "Runs"] {
            assert_fits(&chromium, width, &feature_url(), &replies, json!([{"click": "[role=tab]", "text": tab}]));
        }
    }
}

#[test]
fn the_feature_editor_fits_a_phone() {
    let Some(chromium) = web_chromium::binary() else { return };
    let replies = features(&format!("Has {} in it", word()));
    for width in PHONES {
        assert_fits(&chromium, width, &feature_url(), &replies, json!([{"click": "[data-feature-edit]"}, {"wait": "[data-field-description]"}]));
    }
}

fn run(id: &str, state: &str) -> Value {
    json!({"run_id": id, "feature": name(), "state": state, "stage": format!("stage-{}", "s".repeat(60)), "started_at": "2026-11-01T10:00:00Z", "updated_at": "2026-11-01T11:00:00Z", "target_branch": format!("team/{}", "b".repeat(80)), "delivery": "review", "dirty_files": [path(), format!("{}/x.rs", word())]})
}

fn runs() -> Vec<Reply> {
    let step = format!("step-{}", "s".repeat(120));
    let events = json!([
        {"event_id": "e1", "kind": "stage.started", "stage": step, "attempt": 1, "occurred_at": "2026-11-01T10:00:00Z"},
        {"event_id": "e2", "kind": "stage.failed", "stage": step, "attempt": 1, "occurred_at": "2026-11-01T10:05:00Z"},
        {"event_id": "e3", "kind": "merge.set_aside", "stage": null, "attempt": 1, "occurred_at": "2026-11-01T10:06:00Z", "set_aside": {"stash": "0123abcd".repeat(5), "message": format!("orq set-aside {}", word()), "restored": false, "conflicted": [path()], "note": word()}}
    ]);
    let mut replies = vec![projects()];
    for (id, state) in [("r-blocked", "blocked"), ("r-failed", "failed"), ("r-running", "running")] {
        replies.push(get(&format!("/api/projects/demo/runs/{id}"), json!({"run": run(id, state), "events": events, "next_cursor": null, "remote_actions": true})));
        replies.push(get(&format!("/api/projects/demo/runs/{id}/resume-check"), json!({"run": id, "next": "review"})));
        replies.push(get(&format!("/api/projects/demo/runs/{id}/discard-warning"), json!({"run": id, "warning": format!("its branch team/{} goes with 1 commit not on the target branch", "b".repeat(80))})));
    }
    replies.push(get("/api/projects/demo/runs", json!({"runs": [run("r-blocked", "blocked"), run("r-failed", "failed"), run("r-running", "running")], "next_cursor": null, "total": 3, "running": 1, "remote_actions": true})));
    replies
}

#[test]
fn the_run_detail_page_and_each_of_its_confirms_fit_a_phone() {
    let Some(chromium) = web_chromium::binary() else { return };
    let replies = runs();
    let cases = [
        ("r-blocked", None),
        ("r-blocked", Some("[data-resume]")),
        ("r-blocked", Some("[data-discard]")),
        ("r-blocked", Some("[data-merge-set-aside]")),
        ("r-failed", Some("[data-accept]")),
        ("r-running", Some("[data-pause]")),
        ("r-running", Some("[data-stop]")),
    ];
    for width in PHONES {
        for (id, action) in cases {
            let steps = match action {
                Some(button) => json!([{"click": button}, {"wait": ".confirm [data-confirm]"}]),
                None => json!([{"wait": ".run-step-name"}]),
            };
            assert_fits(&chromium, width, &format!("/projects/demo/orchestrations/{id}"), &replies, steps);
        }
        assert_fits(&chromium, width, "/projects/demo/orchestrations/r-blocked", &replies, json!([{"click": "[data-run-tab=events]"}, {"wait": ".attempt-title"}]));
    }
}

fn reviews() -> Vec<Reply> {
    let long = |n: &str| format!("{n} {} done", word());
    vec![
        projects(),
        get("/api/reviews", json!({"reviews": [{"run_id": "run-1", "feature": name(), "status": "ready", "risk": "high", "stale": false, "freshness": "current", "generated_at": "2026-11-01T10:00:00Z", "project": "demo"}]})),
        get("/api/reviews/run-1", json!({"stale": false, "freshness": "current", "link": null, "status": "ready", "reported_head_sha": "abc1234def", "reviewed_head_sha": "abc1234def", "summary": [long("Summary")], "risks": [long("Risk")], "files": [path()], "tests": [long("Test")], "rounds": [long("Round")], "generated_at": "2026-11-01T10:00:00Z"})),
    ]
}

#[test]
fn the_review_detail_page_and_its_confirms_fit_a_phone() {
    let Some(chromium) = web_chromium::binary() else { return };
    let replies = reviews();
    let open = json!([{"wait": ".review-summary"}, {"click": ".review-section summary", "text": "Files"}, {"click": ".review-section summary", "text": "Tests"}, {"click": ".review-section summary", "text": "Review rounds"}]);
    for width in PHONES {
        assert_fits(&chromium, width, "/reviews/run-1", &replies, open.clone());
        for action in ["resolve", "dismiss"] {
            assert_fits(&chromium, width, "/reviews/run-1", &replies, json!([{"click": format!("[data-review-action={action}]")}, {"wait": ".confirm [data-confirm]"}]));
        }
    }
}

#[test]
fn the_lists_and_the_dashboard_fit_a_phone() {
    let Some(chromium) = web_chromium::binary() else { return };
    let mut replies = runs();
    replies.extend(features(&word()));
    replies.extend(reviews());
    for width in PHONES {
        assert_fits(&chromium, width, "/projects/demo/features", &replies, json!([{"wait": "main li"}]));
        assert_fits(&chromium, width, "/projects/demo/orchestrations", &replies, json!([{"wait": ".run-open"}]));
        assert_fits(&chromium, width, "/reviews", &replies, json!([{"wait": ".review-row-title"}]));
        assert_fits(&chromium, width, "/dashboard", &replies, json!([{"wait": "main h1"}]));
    }
}

fn settings() -> Vec<Reply> {
    let branch = format!("team/{}", "b".repeat(120));
    vec![
        get("/auth/passkeys", json!({"passkeys": [{"label": format!("phone {}", word()), "id": "A".repeat(120), "this_device": true}]})),
        get("/api/settings", json!({"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}})),
        get("/api/storage/worktrees", json!({"projects": [{"id": "demo", "name": "demo", "worktrees": [{"branch": branch, "run": "run-2", "ending": "completed", "reason": format!("the worktree has uncommitted changes in {}", path()), "target": "main", "commits": 2, "files": [{"path": path(), "kind": "untracked"}], "more_files": 0, "files_unreadable": false, "merged": false, "safe": false, "bytes": 700, "size": "700 B", "version": "v-x"}]}], "count": 1, "bytes": 700, "size": "700 B"})),
        get("/api/settings/storage/preview", json!({"lines": ["3 file(s), 2.5 KiB across 2 run(s):", format!("  {}  1.0 KiB  past its retention", path())], "files": 3, "bytes": 2560, "size": "2.5 KiB", "runs": 2, "metadata": 0, "hash": "h1"})),
    ]
}

#[test]
fn settings_and_its_confirms_fit_a_phone() {
    let Some(chromium) = web_chromium::binary() else { return };
    let replies = settings();
    for width in PHONES {
        assert_fits(&chromium, width, "/settings", &replies, json!([{"click": "[data-storage-preview]"}, {"wait": "[data-storage-line]"}, {"click": "[data-storage-prune]"}, {"wait": ".confirm [data-confirm]"}]));
        assert_fits(&chromium, width, "/settings", &replies, json!([{"click": "[data-kept-delete]"}, {"wait": "[data-kept-branch-typed]"}]));
        assert_fits(&chromium, width, "/settings", &replies, json!([{"click": "[data-passkey-remove]"}, {"wait": ".confirm [data-confirm]"}]));
    }
}

/// A stand-in service worker with a new version waiting, so the update banner shows.
const WAITING_WORKER: &str = "Object.defineProperty(navigator, 'serviceWorker', {configurable: true, value: {controller: {}, addEventListener: function () {}, register: function () { return Promise.resolve({waiting: {postMessage: function () {}}, installing: null, addEventListener: function () {}}); }}});";

#[test]
fn the_update_banner_fits_a_phone() {
    let Some(chromium) = web_chromium::binary() else { return };
    let replies = features(&word());
    for width in PHONES {
        let out = measure_with(&chromium, width, &feature_url(), &replies, json!([{"wait": ".update-card"}]), WAITING_WORKER);
        let result = &out.result;
        assert!(result.get("error").is_none(), "{width} px: {result}");
        assert!(result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(), "{width} px: {result}");
        assert_eq!(result["offenders"], json!([]), "{width} px");
    }
}

#[test]
fn on_a_desktop_nothing_scrolls_sideways_and_ordinary_words_stay_on_one_line() {
    let Some(chromium) = web_chromium::binary() else { return };
    let mut long = runs();
    long.extend(features(&format!("Has {} in it", word())));
    long.extend(reviews());
    for url in [feature_url(), "/projects/demo/orchestrations/r-blocked".to_string(), "/reviews/run-1".to_string(), "/projects/demo/features".to_string(), "/dashboard".to_string()] {
        assert_fits(&chromium, 1280, &url, &long, json!([{"wait": "main h1"}]));
    }
    let short = features("Three plain words");
    let out = measure(&chromium, 1280, &feature_url(), &short, json!([{"wait": ".feature-panel p"}, {"lines": ".feature-panel p", "text": "Three plain words"}]));
    assert_eq!(out.result["lines"][".feature-panel p"], 1, "{}", out.result);
}
