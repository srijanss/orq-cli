//! Last checked, Last activity and Reviewed at are separate labels. Each time is shown relative
//! to now with the absolute local time in its title, so a refresh never implies the content changed.

mod web_deps;

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const RUN: &str = "/projects/alpha/orchestrations/run-1";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

/// The UTC time `seconds` ago, as the server writes it (no external `date` command).
fn ago(seconds: u64) -> String {
    let at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() - seconds;
    let (days, rest) = ((at / 86400) as i64, at % 86400);
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let day_of_era = z.rem_euclid(146097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 { month_part + 3 } else { month_part - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha")]}})
}

/// The `<time>` elements of a frame whose label is `label`.
fn times<'a>(frame: &'a Value, label: &str) -> Vec<&'a Value> {
    frame["settled"]["times"].as_array().unwrap().iter().filter(|t| t["label"] == label).collect()
}

fn only<'a>(out: &'a Value, frame: usize, label: &str) -> &'a Value {
    let found = times(&out["frames"][frame], label);
    assert_eq!(found.len(), 1, "{label}: {out}");
    found[0]
}

#[test]
fn a_run_shows_last_activity_and_start_as_relative_times_with_the_absolute_time_in_a_title() {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "running", "stage": null, "started_at": ago(2 * 3600), "updated_at": ago(5 * 60)});
    let timeline = json!({"status": 200, "json": {"run": header, "events": [{"event_id": "e1", "kind": "run.started", "stage": null, "attempt": 1, "occurred_at": ago(3 * 86400)}], "next_cursor": null}});
    let stream = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = run(RUN, json!([timeline, projects(), stream]), json!([{"submit": TOKEN}])) else { return };
    let activity = only(&out, 1, "Last activity");
    assert_eq!(activity["text"], "5 minutes ago", "{out}");
    assert_eq!(only(&out, 1, "Started")["text"], "2 hours ago", "{out}");
    let event = times(&out["frames"][1], "").into_iter().find(|t| t["text"] == "3 days ago");
    assert!(event.is_some(), "the event time is relative too: {out}");
    for shown in [activity, only(&out, 1, "Started"), event.unwrap()] {
        let title = shown["title"].as_str().unwrap();
        assert!(title.contains("20") && title != shown["text"].as_str().unwrap(), "the title holds the absolute local time: {shown}");
        assert!(shown["datetime"].as_str().unwrap().ends_with('Z'), "{shown}");
    }
}

#[test]
fn a_report_says_reviewed_at_not_generated_and_last_checked_is_a_separate_label() {
    let list = json!({"status": 200, "json": {"reviews": [{"run_id": "run-1", "feature": "f", "project": "p", "current": true, "status": "approved", "risk": "low", "stale": false, "link": null, "generated_at": ago(2 * 3600)}]}});
    let Some(out) = run("/", json!([list]), json!([{"submit": TOKEN}])) else { return };
    let reviewed = only(&out, 1, "Reviewed at");
    assert_eq!(reviewed["text"], "2 hours ago", "{out}");
    assert!(reviewed["title"].as_str().unwrap().contains("20"), "{reviewed}");
    assert!(times(&out["frames"][1], "Generated").is_empty(), "{out}");
    let checked = only(&out, 1, "Last checked");
    assert_eq!(checked["text"], "just now", "{out}");
    assert!(checked["title"].as_str().unwrap().contains("20"), "{checked}");
}

#[test]
fn an_odd_or_future_time_shows_nothing_relative() {
    let header = json!({"run_id": "run-1", "feature": "login", "target_branch": "main", "state": "running", "stage": null, "started_at": "<b>x</b>", "updated_at": "2999-01-01T00:00:00Z"});
    let timeline = json!({"status": 200, "json": {"run": header, "events": [], "next_cursor": null}});
    let stream = json!({"status": 200, "stream": [], "end": "hold"});
    let Some(out) = run(RUN, json!([timeline, projects(), stream]), json!([{"submit": TOKEN}])) else { return };
    assert!(times(&out["frames"][1], "Started").is_empty(), "{out}");
    assert_eq!(only(&out, 1, "Last activity")["text"], "just now", "a time ahead of this clock is not shown as the future: {out}");
}
