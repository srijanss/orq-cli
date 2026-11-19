//! Cleaning up after a run's MR merged, on the web: the run page's Clean up card with each step and
//! why one did not finish, its Clean up button (a passkey session, asked first) that sends
//! `{confirm:true}`, the dashboard card's runs waiting to be cleaned up and its commits not pushed,
//! the project page's commits not pushed, and the project setting "After an MR merges".

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 9] = [
    "[data-cleanup]",
    "[data-cleanup-step]",
    "[data-run-cleanup]",
    "[data-cleanup-needs-passkey]",
    "[data-confirm]",
    "[data-card-cleanup]",
    "[data-unpushed]",
    "[data-project-after-merge]",
    "[data-after-merge]",
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

/// `url` signed in with the token.
fn as_token(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    harness(json!({"url": url, "responses": responses, "steps": all, "selectors": SELECTORS}))
}

/// `url` signed in with a passkey session.
fn as_session(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    let auth = json!({"passkeys": true, "signed_in": true, "csrf": "c-1"});
    harness(
        json!({"url": url, "responses": responses, "auth": auth, "steps": steps, "selectors": SELECTORS}),
    )
}

fn project(extra: Value) -> Value {
    let mut card = json!({"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24, "queue": null});
    for (key, value) in extra.as_object().unwrap() {
        card[key] = value.clone();
    }
    card
}

fn projects(extra: Value) -> Value {
    json!({"status": 200, "json": {"projects": [project(extra)]}})
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
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

fn shows(frame: &Value, wanted: &str) -> bool {
    texts(frame).iter().any(|t| t.contains(wanted))
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
        .map(|r| {
            (
                r["url"].as_str().unwrap().to_string(),
                serde_json::from_str(r["body"].as_str().unwrap_or("null")).unwrap(),
            )
        })
        .collect()
}

fn cleanup(states: [(&str, Option<&str>); 4]) -> Value {
    let names = [
        "complete_feature",
        "delete_worktree",
        "delete_branch",
        "pull_target",
    ];
    json!({"steps": names.iter().zip(states).map(|(step, (state, reason))| json!({"step": step, "state": state, "reason": reason})).collect::<Vec<_>>()})
}

fn run_page(card: Value) -> Value {
    let shown = json!({"run_id": "run-7", "feature": "COM-01", "state": "completed", "stage": null, "started_at": null, "updated_at": null, "target_branch": "main", "cleanup": card});
    json!({"status": 200, "json": {"run": shown, "events": [], "next_cursor": null, "remote_actions": true}})
}

const PENDING: [(&str, Option<&str>); 4] = [
    ("pending", None),
    ("pending", None),
    ("pending", None),
    ("pending", None),
];

#[test]
fn a_merged_run_shows_its_clean_up_steps_and_clean_up_asks_then_sends_confirm() {
    let done = cleanup([
        ("done", None),
        ("done", None),
        ("done", None),
        (
            "done",
            Some("main fast-forwarded from 1111aaaa to 2222bbbb"),
        ),
    ]);
    let hold = json!({"status": 200, "stream": [], "end": "hold"});
    let responses = vec![
        run_page(cleanup(PENDING)),
        projects(json!({})),
        hold.clone(),
        json!({"status": 200, "json": done.clone()}),
        run_page(done),
        hold,
    ];
    let steps = vec![
        json!({"click": "[data-run-cleanup]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let Some(out) = as_session("/projects/alpha/orchestrations/run-7", responses, steps) else {
        return;
    };
    let first = &out["frames"].as_array().unwrap()[0];
    assert_eq!(
        (
            count(first, "[data-cleanup]"),
            count(first, "[data-cleanup-step]"),
            count(first, "[data-run-cleanup]")
        ),
        (1, 4, 1),
        "{out}"
    );
    for label in [
        "Mark COM-01 completed",
        "Delete the worktree",
        "Delete the branch",
        "Fast-forward main",
    ] {
        assert!(shows(first, label), "{label}: {:?}", texts(first));
    }
    assert_eq!(
        posts(&out),
        vec![(
            "/api/projects/alpha/runs/run-7/cleanup".to_string(),
            json!({"confirm": true})
        )],
        "{out}"
    );
    let frame = last(&out);
    assert_eq!(
        count(frame, "[data-run-cleanup]"),
        0,
        "nothing is left to clean up: {out}"
    );
    assert!(
        shows(frame, "main fast-forwarded from 1111aaaa to 2222bbbb"),
        "{:?}",
        texts(frame)
    );
}

#[test]
fn a_step_that_did_not_finish_says_why_and_the_token_sign_in_cannot_clean_up() {
    let stopped = cleanup([
        ("done", None),
        (
            "skipped",
            Some("the worktree has uncommitted changes: notes.txt"),
        ),
        ("skipped", Some("deleting the worktree did not finish")),
        ("skipped", Some("deleting the worktree did not finish")),
    ]);
    let hold = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = as_token(
        "/projects/alpha/orchestrations/run-7",
        vec![run_page(stopped), projects(json!({})), hold],
        vec![],
    ) else {
        return;
    };
    let frame = last(&out);
    assert!(
        shows(frame, "the worktree has uncommitted changes: notes.txt"),
        "{:?}",
        texts(frame)
    );
    assert_eq!(
        (
            count(frame, "[data-run-cleanup]"),
            count(frame, "[data-cleanup-needs-passkey]")
        ),
        (0, 1),
        "{out}"
    );
}

#[test]
fn the_dashboard_card_shows_runs_to_clean_up_and_commits_not_pushed() {
    let unpushed = json!({"target": "main", "count": 2, "message": "2 commits on main not pushed", "subjects": ["rotate [REDACTED]", "tidy readme"]});
    let extra = json!({"cleanup": [{"run": "run-7", "feature": "COM-01", "review": "!12"}], "unpushed": unpushed});
    let Some(out) = as_token("/dashboard", vec![projects(extra)], vec![]) else {
        return;
    };
    let frame = last(&out);
    assert_eq!(
        (
            count(frame, "[data-card-cleanup]"),
            count(frame, "[data-unpushed]")
        ),
        (1, 1),
        "{out}"
    );
    assert!(
        shows(frame, "COM-01 merged in !12: clean up"),
        "{:?}",
        texts(frame)
    );
    assert!(
        shows(frame, "2 commits on main not pushed"),
        "{:?}",
        texts(frame)
    );

    let Some(out) = as_token(
        "/dashboard",
        vec![projects(json!({"cleanup": [], "unpushed": null}))],
        vec![],
    ) else {
        return;
    };
    assert_eq!(
        (
            count(last(&out), "[data-card-cleanup]"),
            count(last(&out), "[data-unpushed]")
        ),
        (0, 0),
        "{out}"
    );
}

#[test]
fn the_project_page_shows_commits_not_pushed_with_their_subjects() {
    let unpushed = json!({"target": "main", "count": 2, "message": "2 commits on main not pushed", "subjects": ["rotate [REDACTED]", "tidy readme"]});
    let queue = json!({"status": 200, "json": {"queue": {"entries": [], "count": 0, "paused": null, "unpushed": unpushed}, "remote_actions": true}});
    let Some(out) = as_token(
        "/projects/alpha/queue",
        vec![queue, projects(json!({}))],
        vec![],
    ) else {
        return;
    };
    let frame = last(&out);
    assert_eq!(count(frame, "[data-unpushed]"), 1, "{out}");
    assert!(
        shows(frame, "2 commits on main not pushed"),
        "{:?}",
        texts(frame)
    );
    assert!(shows(frame, "rotate [REDACTED]"), "{:?}", texts(frame));
}

fn settings(after_merge: Option<&str>) -> Value {
    let mut settings = json!({
        "implementer": {"project": null, "fallback": null},
        "reviewer": {"project": null, "fallback": null},
        "delivery": "review",
        "max_active_runs": null,
        "min_free_disk_gb": null,
        "queue_hours": null,
        "queue_stop_on_failure": false,
        "queue_wait_for_review": true,
        "pull_target_before_runs": true,
        "target_branch": "main",
        "revision": 3
    });
    if let Some(after_merge) = after_merge {
        settings["after_merge"] = json!(after_merge);
    }
    json!({"status": 200, "json": {"settings": settings}})
}

#[test]
fn settings_offer_after_merge_ask_by_default_and_save_sends_auto() {
    let Some(out) = as_token(
        "/projects/alpha/settings",
        vec![settings(None), projects(json!({}))],
        vec![],
    ) else {
        return;
    };
    let frame = last(&out);
    assert!(shows(frame, "After an MR merges"), "{:?}", texts(frame));
    assert_eq!(count(frame, "[data-project-after-merge]"), 1, "{out}");
    assert!(
        shows(frame, "Offers Clean up"),
        "an unsaid setting asks: {:?}",
        texts(frame)
    );

    let machine = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": true, "version": "v-1"}}});
    let options =
        json!({"status": 200, "json": {"options": [{"harness": "claude", "models": ["opus"]}]}});
    let ok = json!({"status": 200, "json": {"settings": {}}});
    let steps = vec![
        json!({"click": "[data-project-settings-edit]"}),
        json!({"click": "[data-after-merge]"}),
        json!({"click": "[data-project-settings-save]"}),
        json!({"click": "[data-confirm]"}),
    ];
    let responses = vec![
        settings(Some("ask")),
        projects(json!({})),
        machine,
        options,
        ok,
        settings(Some("auto")),
    ];
    let Some(out) = as_token("/projects/alpha/settings", responses, steps) else {
        return;
    };
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "{out}");
    assert_eq!(sent[0].1["after_merge"], "auto", "{out}");
    assert!(
        shows(last(&out), "Cleans up on its own and tells you"),
        "{:?}",
        texts(last(&out))
    );
}
