//! On the runs list a frontend run's Frontend badge is as wide as its text, left-aligned under the run name,
//! on desktop and on a phone; the Run cell is a flex column, so the badge opts out of its stretch.

mod web_chromium;

use serde_json::{json, Value};
use web_chromium::{get, measure, Reply};

fn run_row(id: &str, feature: &str, kind: &str) -> Value {
    json!({"run_id": id, "feature": feature, "target_branch": "main", "state": "running", "stage": null, "started_at": "2026-11-02T10:00:00Z", "updated_at": "2026-11-02T10:30:00Z", "feature_kind": kind})
}

fn replies() -> Vec<Reply> {
    let runs = json!({"runs": [run_row("run-p", "a-frontend-feature-with-a-long-name", "frontend"), run_row("run-e", "engine", "standard")], "next_cursor": null, "total": 2, "running": 2});
    vec![
        get("/api/projects", json!({"projects": [{"id": "demo", "name": "demo", "available": true, "features": 2, "active_runs": 2, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]})),
        get("/api/projects/demo/features", json!({"features": [], "remote_actions": false})),
        get("/api/projects/demo/runs*", runs),
    ]
}

fn width(rect: &Value) -> i64 {
    rect["right"].as_i64().unwrap() - rect["left"].as_i64().unwrap()
}

const BADGE: &str = ".run-table [data-frontend-badge]";
const CELL: &str = ".run-table tr.run-row td:first-child";
const NAME: &str = ".run-table .run-open";

#[test]
fn the_runs_list_badge_is_as_wide_as_its_text_and_left_aligned_under_the_run_name_on_desktop_and_phone() {
    let Some(chromium) = web_chromium::binary() else { return };
    for screen in [1280, 390] {
        let steps = json!([{"wait": "[data-frontend-badge]"}, {"boxes": BADGE}, {"boxes": CELL}, {"boxes": NAME}]);
        let out = measure(&chromium, screen, "/projects/demo/orchestrations", &replies(), steps);
        let result = &out.result;
        assert!(result.get("error").is_none(), "{screen}: {result}\nunanswered: {:?}", out.unanswered);
        let badges = result["boxes"][BADGE].as_array().unwrap();
        assert_eq!(badges.len(), 1, "{screen}: a badge for the frontend run and none for the standard one: {result}");
        let (badge, cell, name) = (&badges[0], &result["boxes"][CELL][0], &result["boxes"][NAME][0]);
        assert!(width(badge) < 120, "{screen}: the badge is the width of \"Frontend\", not the cell's {}: {result}", width(cell));
        assert!(width(badge) < width(cell) / 2, "{screen}: the badge does not stretch across the cell: {result}");
        assert_eq!(badge["left"], name["left"], "{screen}: left-aligned under the run name: {result}");
        assert!(badge["top"].as_i64() >= name["bottom"].as_i64(), "{screen}: under the run name: {result}");
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The shell version the build gives the worker: SHA-256 over every file under web/ but the worker and the
/// stamps, as its path, a zero byte, its bytes, a zero byte, in path order; the first 16 hex digits.
fn shell_version() -> String {
    use sha2::{Digest, Sha256};
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut names: Vec<(String, std::path::PathBuf)> = files.into_iter().map(|path| (path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), path)).filter(|(name, _)| name != "sw.js" && !name.ends_with(".stamp")).collect();
    names.sort();
    let mut hasher = Sha256::new();
    for (name, path) in names {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update(std::fs::read(path).unwrap());
        hasher.update([0u8]);
    }
    hex(&hasher.finalize())[..16].to_string()
}

#[test]
fn the_worker_and_the_stamp_are_rebuilt_for_the_new_app_css() {
    use sha2::{Digest, Sha256};
    let worker = web("sw.js");
    assert!(worker.contains("\"orq-shell-\"") && worker.contains(&format!("\"{}\"", shell_version())),"web/sw.js caches the shell under the version of the current app.css: run `node build.mjs` in web-src");
    let stamp = web("app.react.js.stamp");
    assert!(stamp.lines().any(|line| line == format!("sw {}", hex(&Sha256::digest(worker.as_bytes())))), "the stamp carries the rebuilt worker's hash: {stamp:?}");
}

fn web(name: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web").join(name)).unwrap()
}

/// Every rule of app.css whose selector names the badge, as (selector, declarations); app.css keeps one rule a line.
fn badge_rules(css: &str) -> Vec<(String, String)> {
    css.lines()
        .filter_map(|line| line.split_once('{'))
        .filter(|(selector, _)| selector.contains("[data-frontend-badge]"))
        .map(|(selector, body)| (selector.trim().to_string(), body.trim_end_matches(['}', ' ']).to_string()))
        .collect()
}

#[test]
fn app_css_keeps_the_runs_list_badge_off_the_cell_stretch_and_nothing_widens_it() {
    let rules = badge_rules(&web("app.css"));
    assert!(
        rules.iter().any(|(selector, body)| selector.split(',').any(|s| s.trim() == ".run-table [data-frontend-badge]") && body.contains("align-self: flex-start")),
        "app.css gives .run-table [data-frontend-badge] align-self: flex-start: {rules:?}"
    );
    for (selector, body) in &rules {
        for wide in ["align-self: stretch", "width: 100%", "flex: 1", "flex-grow: 1", "display: block"] {
            assert!(!body.contains(wide), "{selector} makes the badge stretch with {wide}");
        }
    }
}
