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

/// A stand-in browser: a shell script with `body`, in a fresh private directory.
fn fake_browser(body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = orq_test_support::process_temp_dir().join(format!("orq-fake-browser-{}-{}", std::process::id(), body.len()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("browser");
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[test]
fn the_browser_is_started_headless_and_a_launch_that_fails_is_reported() {
    let browser = fake_browser("printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\necho 'cannot open display' >&2\nexit 1");
    let out = measure(&browser, 390, "/dashboard", &[], json!([]));
    let args = std::fs::read_to_string(browser.with_file_name("args")).unwrap();
    assert!(args.lines().any(|arg| arg == "--headless"), "{args}");
    let error = out.result["error"].as_str().unwrap_or_default();
    assert!(error.contains("exit") && error.contains("cannot open display"), "{}", out.result);
}

#[test]
fn a_browser_that_never_finishes_is_stopped_at_the_deadline() {
    let browser = fake_browser("sleep 30 &\nsleep 30");
    let started = std::time::Instant::now();
    let out = web_chromium::measure_within(&browser, 390, "/dashboard", &[], json!([]), "", std::time::Duration::from_secs(1));
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "waited {:?}", started.elapsed());
    assert!(out.result["error"].as_str().unwrap_or_default().contains("timed out"), "{}", out.result);
}

#[test]
fn a_viewport_wider_than_the_width_asked_for_is_reported_instead_of_measured() {
    let browser = fake_browser("echo '<pre id=\"result\">{\"scrollWidth\":980,\"clientWidth\":980,\"offenders\":[]}</pre>'");
    let out = measure(&browser, 360, "/dashboard", &[], json!([]));
    let error = out.result["error"].as_str().unwrap_or_default();
    assert!(error.contains("980") && error.contains("360"), "{}", out.result);
}

/// A helper that left the browser's process group, by its pid; stopped when dropped.
struct EscapedHelper(libc::pid_t);

impl EscapedHelper {
    /// Waits for the pid the helper wrote once it had left the group.
    fn from_marker(marker: &Path) -> EscapedHelper {
        let pid = std::fs::read_to_string(marker).unwrap_or_else(|error| panic!("the helper never left the group ({error}): is setsid installed?"));
        EscapedHelper(pid.trim().parse().unwrap_or_else(|_| panic!("the helper wrote {pid:?}, not its pid")))
    }

    fn alive(&self) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(self.0, 0) == 0 }
    }
}

impl Drop for EscapedHelper {
    fn drop(&mut self) {
        // SAFETY: sends SIGKILL to the helper this test started.
        unsafe { libc::kill(self.0, libc::SIGKILL) };
    }
}

/// A browser that prints a result and exits, leaving behind a helper in its own session that runs
/// `helper` with the output still open. The helper writes its pid to `<dir>/left` once it has left.
fn browser_leaving_a_helper(helper: &str) -> std::path::PathBuf {
    fake_browser(&format!(
        "left=\"$(dirname \"$0\")/left\"\nrm -f \"$left\"\nsetsid sh -c 'echo $$ > \"$0.tmp\"; mv \"$0.tmp\" \"$0\"; {helper}' \"$left\" &\nwhile [ ! -f \"$left\" ]; do sleep 0.01; done\necho '<pre id=\"result\">{{\"scrollWidth\":390,\"clientWidth\":390,\"offenders\":[]}}</pre>'\nexit 0"
    ))
}

#[test]
fn a_helper_that_leaves_the_browser_group_and_keeps_the_output_open_does_not_hold_up_the_test() {
    let browser = browser_leaving_a_helper("exec sleep 30");
    let started = std::time::Instant::now();
    let out = web_chromium::measure_within(&browser, 390, "/dashboard", &[], json!([]), "", std::time::Duration::from_secs(1));
    let helper = EscapedHelper::from_marker(&browser.with_file_name("left"));
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "waited {:?}", started.elapsed());
    let error = out.result["error"].as_str().unwrap_or_default();
    assert!(error.ends_with(web_chromium::OUTPUT_LEFT_OPEN), "the browser exited but its output stayed open: {}", out.result);
    drop(helper);
}

#[test]
fn a_browser_path_that_is_missing_or_not_executable_is_reported_as_not_started() {
    let missing = orq_test_support::process_temp_dir().join("orq-no-such-browser");
    let plain = fake_browser("exit 0").with_file_name("not-executable");
    std::fs::write(&plain, "#!/bin/sh\nexit 0\n").unwrap();
    for browser in [missing, plain] {
        let out = measure(&browser, 390, "/dashboard", &[], json!([]));
        let error = out.result["error"].as_str().unwrap_or_default();
        assert!(error.contains("could not be started"), "{}: {}", browser.display(), out.result);
    }
}

#[test]
fn once_the_deadline_passes_the_output_is_let_go_so_a_helper_still_writing_to_it_stops() {
    let browser = browser_leaving_a_helper("while :; do echo still-writing; sleep 0.05; done");
    let out = web_chromium::measure_within(&browser, 390, "/dashboard", &[], json!([]), "", std::time::Duration::from_secs(1));
    let helper = EscapedHelper::from_marker(&browser.with_file_name("left"));
    assert!(out.result["error"].as_str().unwrap_or_default().ends_with(web_chromium::OUTPUT_LEFT_OPEN), "{}", out.result);
    // With nobody left reading, the helper's next write fails with a broken pipe and ends it.
    let waited = std::time::Instant::now();
    while helper.alive() && waited.elapsed() < std::time::Duration::from_secs(3) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!helper.alive(), "the output is still being read after run_until returned");
}
