//! A project's three design roles (preparer, reviewer, verifier) are set like its implementer and reviewer:
//! a harness, model and thinking level each, saved in orq.db through `POST /api/projects/{id}/settings` with
//! the revision check, read back by the settings API, and edited in a "Frontend design" group of the project's
//! Settings page. A design role must use pi or claude, a model that accepts images (for pi, its
//! `pi --list-models` says so) and a thinking level its harness takes.

mod web_chromium;
mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

/// What the fake pi prints for `--list-models`: one model that takes images and one that doesn't.
const PI_MODELS: &str = "provider  model          context  max-out  thinking  images\nopenai    gpt-6.1-sol    400K     128K     yes       yes\nopenai    gpt-6.1-terra  400K     128K     yes       yes\nkimi      kimi-text      128K     32K      no        no\n";

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    pi: PathBuf,
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
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let pi = base.join("pi");
    orq_test_support::write_executable(
        &pi,
        &format!("#!/bin/sh\n[ \"$1\" = --list-models ] || exit 2\ncat <<'EOF'\n{PI_MODELS}EOF\n"),
    );
    Setup {
        scan: base.join("code"),
        roots,
        pi,
    }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    std::fs::canonicalize(path).unwrap()
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let options: Arc<orq_remote::projects::AgentOptionsSource> = Arc::new(|| {
        vec![
            (
                "claude".to_string(),
                vec!["opus".to_string(), "sonnet".to_string()],
            ),
            (
                "pi".to_string(),
                vec![
                    "openai/gpt-6.1-sol".to_string(),
                    "openai/gpt-6.1-terra".to_string(),
                    "kimi/kimi-text".to_string(),
                ],
            ),
        ]
    });
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone())
        .with_agent_options(options)
        .with_pi(s.pi.clone());
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method,
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let r = handle_with_body(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        body,
        &provider,
    );
    (r.status, r.body)
}

/// Saves `body` over a record with no agents, no limits and the review delivery, unless it names them.
fn save(s: &Setup, id: &str, extra: Value) -> (u16, Value) {
    let mut body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": null, "revision": 0});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    send(
        s,
        "POST",
        &format!("/api/projects/{id}/settings"),
        body.to_string().as_bytes(),
    )
}

#[test]
fn each_design_role_is_saved_read_back_by_the_api_and_kept_in_orq_db_and_a_stale_save_is_refused() {
    let s = setup("design-agents-round-trip");
    let alpha = git_repo(&s.scan, "alpha");
    let preparer = json!({"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "medium"});
    let reviewer = json!({"harness": "pi", "model": "openai/gpt-6.1-terra", "effort": "medium"});
    let verifier = json!({"harness": "claude", "model": "opus", "effort": "high"});

    let (status, body) = save(
        &s,
        "alpha",
        json!({"design_preparer": preparer, "design_reviewer": reviewer, "design_verifier": verifier}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["design_preparer"], preparer);
    assert_eq!(body["settings"]["design_reviewer"], reviewer);
    assert_eq!(body["settings"]["design_verifier"], verifier);
    assert_eq!(body["settings"]["revision"], 1);
    assert_eq!(
        send(&s, "GET", "/api/projects/alpha/settings", b"").1,
        body,
        "the answer is what a read gives"
    );

    let stored = Store::open(&s.roots.database())
        .unwrap()
        .project_settings(&alpha)
        .unwrap()
        .settings;
    assert_eq!(
        stored
            .design_reviewer
            .as_ref()
            .map(|agent| agent.model.as_str()),
        Some("openai/gpt-6.1-terra")
    );
    assert_eq!(
        stored
            .design_verifier
            .as_ref()
            .map(|agent| agent.harness.as_str()),
        Some("claude")
    );

    let stale = save(&s, "alpha", json!({"design_reviewer": null, "revision": 0}));
    assert_eq!(stale.0, 409, "{stale:?}");
    let (status, cleared) = save(
        &s,
        "alpha",
        json!({"design_preparer": null, "design_reviewer": null, "design_verifier": null, "revision": 1}),
    );
    assert_eq!(status, 200, "{cleared}");
    for role in ["design_preparer", "design_reviewer", "design_verifier"] {
        assert_eq!(
            cleared["settings"][role],
            Value::Null,
            "{role} reset: {cleared}"
        );
    }
}

#[test]
fn a_save_that_leaves_the_design_roles_out_keeps_them_and_only_null_resets_one() {
    let s = setup("design-agents-left-out");
    let alpha = git_repo(&s.scan, "alpha");
    let reviewer = json!({"harness": "pi", "model": "openai/gpt-6.1-terra", "effort": "medium"});
    let verifier = json!({"harness": "claude", "model": "opus", "effort": "high"});
    let (status, body) = save(&s, "alpha", json!({"design_reviewer": reviewer, "design_verifier": verifier}));
    assert_eq!(status, 200, "{body}");

    // An older page knows no design role, so it saves the rest of the settings without them.
    let mut older = s.clone_without_pi();
    let (status, body) = save(&older, "alpha", json!({"delivery": "merge_local", "revision": 1}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["delivery"], "merge_local");
    assert_eq!(body["settings"]["design_reviewer"], reviewer, "kept without asking pi again: {body}");
    assert_eq!(body["settings"]["design_verifier"], verifier);
    assert_eq!(body["settings"]["design_preparer"], Value::Null);
    let stored = Store::open(&s.roots.database()).unwrap().project_settings(&alpha).unwrap();
    assert_eq!(stored.settings.design_reviewer.as_ref().map(|agent| agent.model.as_str()), Some("openai/gpt-6.1-terra"));
    assert_eq!(stored.settings.design_verifier.as_ref().map(|agent| agent.model.as_str()), Some("opus"));

    older.pi = s.pi.clone();
    let (status, body) = save(&older, "alpha", json!({"design_verifier": null, "revision": 2}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["design_verifier"], Value::Null, "null resets: {body}");
    assert_eq!(body["settings"]["design_reviewer"], reviewer, "the role left out is kept: {body}");
}

/// Points the setup at a fake pi running `script`, and saves a pi design reviewer with it.
fn save_with_pi(s: &mut Setup, name: &str, script: &str) -> ((u16, Value), std::time::Duration) {
    s.pi = s.roots.data.join(name);
    orq_test_support::write_executable(&s.pi, script);
    let started = std::time::Instant::now();
    let answer = save(s, "alpha", json!({"design_reviewer": {"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "medium"}}));
    (answer, started.elapsed())
}

#[test]
fn a_pi_whose_descendant_keeps_its_output_open_is_given_up_on_and_the_descendant_is_stopped() {
    let mut s = setup("design-agents-pi-descendant");
    let alpha = git_repo(&s.scan, "alpha");
    let pid_file = s.roots.data.join("descendant.pid");
    let script = format!("#!/bin/sh\nsleep 60 &\necho $! > '{}'\ncat <<'EOF'\n{PI_MODELS}EOF\n", pid_file.display());
    let (answer, took) = save_with_pi(&mut s, "pi-descendant", &script);
    assert_eq!(answer, (422, json!({"error": "pi's models could not be listed"})));
    assert!(took < std::time::Duration::from_secs(10), "given up on in bounded time: {took:?}");
    let pid: libc::pid_t = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    let gone = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        !alive
    });
    assert!(gone, "the descendant holding pi's output was stopped");
    assert_eq!(Store::open(&s.roots.database()).unwrap().project_settings(&alpha).unwrap().revision, 0, "nothing saved");
}

#[test]
fn a_pi_that_hangs_says_too_much_or_fails_after_a_good_table_is_refused_in_bounded_time_and_nothing_is_saved() {
    let mut s = setup("design-agents-pi-failures");
    let alpha = git_repo(&s.scan, "alpha");
    let cases = [
        ("pi-hangs", "#!/bin/sh\nexec sleep 60\n".to_string(), 10),
        // Over 256 KiB and still writing: given up on as soon as the cap is passed, not at the timeout.
        ("pi-floods", format!("#!/bin/sh\ncat <<'EOF'\n{PI_MODELS}EOF\nexec yes 'openai    gpt-6.1-sol    400K     128K     yes       yes'\n"), 3),
        ("pi-fails", format!("#!/bin/sh\ncat <<'EOF'\n{PI_MODELS}EOF\nexit 3\n"), 3),
    ];
    for (name, script, within) in cases {
        let (answer, took) = save_with_pi(&mut s, name, &script);
        assert_eq!(answer, (422, json!({"error": "pi's models could not be listed"})), "{name}");
        assert!(took < std::time::Duration::from_secs(within), "{name} took {took:?}");
    }
    assert_eq!(Store::open(&s.roots.database()).unwrap().project_settings(&alpha).unwrap().revision, 0, "nothing saved");
}

impl Setup {
    /// The same machine, but pi can't be run, so a save that asked pi for its models would be refused.
    fn clone_without_pi(&self) -> Setup {
        Setup { scan: self.scan.clone(), roots: self.roots.clone(), pi: self.roots.data.join("no-such-pi") }
    }
}

#[test]
fn a_design_role_whose_model_pi_says_takes_no_images_or_pi_doesnt_list_or_whose_level_is_not_pis_is_refused_and_nothing_is_saved(
) {
    let mut s = setup("design-agents-refused");
    let alpha = git_repo(&s.scan, "alpha");
    let refusals = [
        (
            json!({"design_reviewer": {"harness": "pi", "model": "kimi/kimi-text", "effort": null}}),
            "kimi/kimi-text doesn't accept images, which the Design reviewer needs",
        ),
        (
            json!({"design_verifier": {"harness": "pi", "model": "openai/gpt-7", "effort": "high"}}),
            "pi doesn't list openai/gpt-7",
        ),
        (
            json!({"design_preparer": {"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "turbo"}}),
            "turbo is not a thinking level pi takes",
        ),
        (
            json!({"design_preparer": {"harness": "claude", "model": "opus", "effort": "off"}}),
            "off is not a thinking level claude takes",
        ),
    ];
    for (design, why) in refusals {
        assert_eq!(
            save(&s, "alpha", design.clone()),
            (422, json!({"error": why})),
            "{design}"
        );
    }
    s.pi = s.roots.data.join("no-such-pi");
    let unlisted = save(
        &s,
        "alpha",
        json!({"design_reviewer": {"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "medium"}}),
    );
    assert_eq!(
        unlisted,
        (422, json!({"error": "pi's models could not be listed"}))
    );
    assert_eq!(
        save(
            &s,
            "alpha",
            json!({"design_reviewer": {"harness": "claude", "model": "sonnet", "effort": "medium"}})
        )
        .0,
        200,
        "claude needs no listing"
    );
    let stored = Store::open(&s.roots.database())
        .unwrap()
        .project_settings(&alpha)
        .unwrap();
    assert_eq!(
        (
            stored.revision,
            stored.settings.design_preparer,
            stored.settings.design_verifier
        ),
        (1, None, None),
        "only the last save was kept"
    );
}

#[test]
fn a_standard_runs_settings_are_the_same_with_or_without_design_roles() {
    let s = setup("design-agents-standard");
    let (alpha, beta) = (git_repo(&s.scan, "alpha"), git_repo(&s.scan, "beta"));
    let standard = json!({"implementer": {"harness": "claude", "model": "sonnet", "effort": "medium"}, "reviewer": {"harness": "pi", "model": "kimi/kimi-text", "effort": "low"}, "delivery": "merge_local", "max_active_runs": 2});
    let mut designed = standard.clone();
    designed["design_reviewer"] =
        json!({"harness": "pi", "model": "openai/gpt-6.1-terra", "effort": "medium"});
    designed["design_verifier"] = json!({"harness": "claude", "model": "opus", "effort": "max"});
    assert_eq!(save(&s, "alpha", standard).0, 200);
    assert_eq!(save(&s, "beta", designed).0, 200);
    let store = Store::open(&s.roots.database()).unwrap();
    let (plain, with_design) = (
        store.project_settings(&alpha).unwrap().settings,
        store.project_settings(&beta).unwrap().settings,
    );
    assert!(with_design.design_reviewer.is_some());
    let standard_part = |settings: orq_core::project_settings::ProjectSettings| {
        orq_core::project_settings::ProjectSettings {
            design_preparer: None,
            design_reviewer: None,
            design_verifier: None,
            ..settings
        }
    };
    assert_eq!(
        standard_part(with_design),
        plain,
        "the design roles change nothing a standard run reads"
    );
    let answer = |id: &str| {
        send(&s, "GET", &format!("/api/projects/{id}/settings"), b"").1["settings"].clone()
    };
    let (a, b) = (answer("alpha"), answer("beta"));
    for key in [
        "implementer",
        "reviewer",
        "delivery",
        "max_active_runs",
        "min_free_disk_gb",
    ] {
        assert_eq!(a[key], b[key], "{key}");
    }
}

// The page, in jsdom and in Chromium.

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const URL: &str = "/projects/alpha/settings";
const GROUP: &str = "[data-design-agents]";
const ROWS: &str = "[data-design-agents] [data-setting-row]";
const LABELS: &str = "[data-design-agents] [data-setting-label]";
const HARNESSES: &str = "[data-design-agents] [data-role-harness]";
const HELP: &str = "Used only by frontend features. The implementer and code reviewer above build and review the code.";

fn page_settings(design_reviewer: Value) -> Value {
    json!({"settings": {
        "implementer": {"project": null, "fallback": {"harness": "claude", "model": "opus", "effort": "high", "source": "machine_default"}},
        "reviewer": {"project": null, "fallback": {"harness": "pi", "model": "kimi", "effort": null, "source": "tdd_config"}},
        "design_preparer": null,
        "design_reviewer": design_reviewer,
        "design_verifier": null,
        "delivery": "review",
        "revision": 2
    }})
}

fn page_options() -> Value {
    json!({"options": [
        {"harness": "claude", "models": ["opus", "sonnet"], "efforts": ["low", "medium", "high", "xhigh", "max"]},
        {"harness": "pi", "models": ["openai/gpt-6.1-sol", "openai/gpt-6.1-terra"], "efforts": ["off", "low", "medium", "high"]},
        {"harness": "codex", "models": ["gpt-5"]}
    ]})
}

fn machine() -> Value {
    json!({"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}})
}

fn harness(responses: Value, steps: Value, selectors: &[&str]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps.as_array().unwrap().iter().cloned());
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario =
        json!({"url": URL, "responses": responses, "steps": all, "selectors": selectors});
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

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str())
        .map(String::from)
        .collect()
}

#[test]
fn the_frontend_design_group_shows_each_role_or_what_it_falls_back_to_and_save_sends_the_roles_picked(
) {
    let ok = |body: Value| json!({"status": 200, "json": body});
    let projects = json!({"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]});
    let steps = json!([
        {"click": "[data-project-settings-edit]"},
        {"set": "[data-role-harness=\"design_preparer\"]", "value": "pi"},
        {"set": "[data-role-effort=\"design_preparer\"]", "value": "medium"},
        {"click": "[data-role-reset=\"design_reviewer\"]"},
        {"click": "[data-project-settings-save]"},
        {"click": "[data-confirm]"},
    ]);
    let reviewer = json!({"harness": "pi", "model": "openai/gpt-6.1-terra", "effort": "medium"});
    let responses = json!([
        ok(page_settings(reviewer.clone())),
        ok(projects),
        ok(machine()),
        ok(page_options()),
        ok(json!({"settings": {}})),
        ok(page_settings(Value::Null))
    ]);
    let selectors = [
        GROUP,
        HARNESSES,
        "[data-design-agents] [data-role-harness] option[value=\"codex\"]",
        "[data-design-suggestion]",
    ];
    let Some(out) = harness(responses, steps, &selectors) else {
        return;
    };
    let frames = out["frames"].as_array().unwrap();
    let viewing = texts(&frames[1]);
    for expected in [
        "Frontend design",
        HELP,
        "Design preparer",
        "Uses the design reviewer",
        "Design reviewer",
        "pi openai/gpt-6.1-terra, effort medium",
        "Design verifier",
        "Uses the design reviewer at thinking high",
    ] {
        assert!(
            viewing.iter().any(|t| t == expected),
            "{expected}: {viewing:?}"
        );
    }
    let editing = &frames[2];
    assert_eq!(
        editing["settled"]["matches"][HARNESSES], 3,
        "each design role offers a harness: {out}"
    );
    assert_eq!(
        editing["settled"]["matches"][selectors[2]], 0,
        "a design role offers only pi and claude: {out}"
    );
    assert_eq!(
        editing["settled"]["matches"]["[data-design-suggestion]"], 2,
        "an unset role suggests a model, as a hint only: {out}"
    );
    let posts: Vec<&Value> = out["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "{out}");
    let sent: Value = serde_json::from_str(posts[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        sent["design_preparer"],
        json!({"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "medium"}),
        "{sent}"
    );
    assert_eq!(sent["design_reviewer"], Value::Null, "{sent}");
    assert_eq!(sent["design_verifier"], Value::Null, "{sent}");
    assert_eq!(sent["revision"], 2);
    let after = texts(out["frames"].as_array().unwrap().last().unwrap());
    assert!(
        after
            .iter()
            .any(|t| t == "Not set: a frontend run needs one"),
        "{after:?}"
    );
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

fn rects(result: &Value, selector: &str) -> Vec<Rect> {
    result["boxes"][selector]
        .as_array()
        .unwrap_or_else(|| panic!("no boxes for {selector}: {result}"))
        .iter()
        .map(|b| Rect {
            left: b["left"].as_i64().unwrap(),
            top: b["top"].as_i64().unwrap(),
            right: b["right"].as_i64().unwrap(),
            bottom: b["bottom"].as_i64().unwrap(),
        })
        .collect()
}

fn inside(inner: Rect, outer: Rect) -> bool {
    inner.left >= outer.left
        && inner.right <= outer.right
        && inner.top >= outer.top
        && inner.bottom <= outer.bottom
}

#[test]
fn the_frontend_design_group_shows_three_rows_like_the_agent_rows_at_390_and_1280_wide() {
    use web_chromium::get;
    let Some(chromium) = web_chromium::binary() else {
        return;
    };
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let replies = [
        get("/api/projects", json!({"projects": [project]})),
        get(
            "/api/projects/alpha/settings",
            page_settings(
                json!({"harness": "pi", "model": "openai/gpt-6.1-terra", "effort": "medium"}),
            ),
        ),
        get("/api/settings", machine()),
        get("/api/settings/options", page_options()),
    ];
    for editing in [false, true] {
        for width in [390, 1280] {
            let mut steps = vec![json!({"wait": GROUP})];
            if editing {
                steps.extend([
                    json!({"click": "[data-project-settings-edit]"}),
                    json!({"wait": HARNESSES}),
                ]);
            }
            for selector in [GROUP, ROWS, LABELS, HARNESSES] {
                steps.push(json!({"boxes": selector}));
            }
            let out = web_chromium::measure(&chromium, width, URL, &replies, Value::Array(steps));
            let result = out.result;
            assert!(
                result.get("error").is_none(),
                "{width} px: {result}\nunanswered: {:?}",
                out.unanswered
            );
            assert!(
                result["scrollWidth"].as_u64() <= result["clientWidth"].as_u64(),
                "{width} px: the page scrolls sideways: {result}"
            );
            assert_eq!(
                result["offenders"],
                json!([]),
                "{width} px: text runs past the screen: {result}"
            );
            let group = rects(&result, GROUP)[0];
            let (rows, labels) = (rects(&result, ROWS), rects(&result, LABELS));
            assert_eq!(
                (rows.len(), labels.len()),
                (3, 3),
                "{width} px, editing {editing}: three rows, each labelled: {result}"
            );
            assert_eq!(
                rects(&result, HARNESSES).len(),
                if editing { 3 } else { 0 },
                "{width} px: a harness per row while editing"
            );
            for at in 0..3 {
                assert!(
                    inside(rows[at], group) && inside(labels[at], rows[at]),
                    "{width} px, editing {editing}: row {at} {:?} in {group:?}",
                    rows[at]
                );
                if at > 0 {
                    assert!(
                        rows[at].top >= rows[at - 1].bottom,
                        "{width} px, editing {editing}: row {at} starts below row {}",
                        at - 1
                    );
                }
            }
            if editing {
                for harness in rects(&result, HARNESSES) {
                    assert!(
                        inside(harness, group),
                        "{width} px: {harness:?} is inside the group {group:?}"
                    );
                }
            }
            let text = result["text"].as_str().unwrap();
            for expected in [
                "Frontend design",
                HELP,
                "Design preparer",
                "Design reviewer",
                "Design verifier",
            ] {
                assert!(
                    text.contains(expected),
                    "{width} px: {expected} is shown: {text}"
                );
            }
        }
    }
}
