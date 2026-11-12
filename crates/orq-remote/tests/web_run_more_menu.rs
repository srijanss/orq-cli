//! On a wide screen a run's row keeps one action in sight (Pause on a running run, Resume or Review again on
//! one that can go on) and puts the rest behind a ⋯ button that opens them as a menu, each still asking first.
//! A row with a single action shows it with no menu. On a phone the ⋯ button is hidden and every action shows
//! in the row as before. The run detail header keeps all its actions in sight.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 7] = [
    ".run-actions > .run-more > button[data-run-more][aria-haspopup=menu][aria-expanded=false] svg",
    ".run-actions > .run-action [data-pause]",
    ".run-more > .run-more-menu [data-stop]",
    ".run-more.open > button[data-run-more][aria-expanded=true]",
    ".run-more-menu .confirm [data-confirm]",
    ".run-actions > .run-action [data-stop]",
    "[data-run-more]",
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

fn one(id: &str, state: &str) -> Value {
    json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"})
}

fn runs_page(rows: &[(&str, &str)], steps: Vec<Value>) -> Option<Value> {
    let body = json!({"runs": rows.iter().map(|(id, state)| one(id, state)).collect::<Vec<_>>(), "next_cursor": null, "total": rows.len(), "running": 1, "remote_actions": true});
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    run(json!({"url": "/projects/alpha/orchestrations", "responses": [{"status": 200, "json": body}, projects()], "steps": all, "selectors": SELECTORS}))
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap()
}

#[test]
fn a_running_row_shows_pause_and_keeps_stop_behind_a_dots_button_that_opens_it_and_stop_still_asks() {
    let Some(out) = runs_page(&[("r-run", "running")], vec![]) else { return };
    let shown = last(&out);
    assert_eq!(count(shown, SELECTORS[0]), 1, "{out}");
    assert_eq!(count(shown, SELECTORS[1]), 1, "pause stays in sight: {out}");
    assert_eq!(count(shown, SELECTORS[2]), 1, "stop is in the menu: {out}");
    assert_eq!(count(shown, SELECTORS[5]), 0, "not beside pause: {out}");

    let Some(out) = runs_page(&[("r-run", "running")], vec![json!({"click": "[data-run-more]"}), json!({"click": "[data-stop]"})]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[2], SELECTORS[3]), 1, "the button says the menu is open: {out}");
    assert_eq!(count(last(&out), SELECTORS[4]), 1, "stop asks first, within the menu: {out}");
}

#[test]
fn a_row_with_one_action_shows_it_without_a_menu() {
    let Some(out) = runs_page(&[("r-ask", "pause_requested")], vec![]) else { return };
    assert_eq!(count(last(&out), SELECTORS[6]), 0, "{out}");
    assert_eq!(count(last(&out), SELECTORS[5]), 1, "{out}");
}

#[test]
fn the_menu_is_closed_until_opened_on_a_wide_screen_and_a_phone_hides_the_dots_and_shows_every_action() {
    let css = css();
    assert!(css.contains(".run-more-menu { display: none;"), "closed until opened");
    assert!(css.contains(".run-more.open .run-more-menu { display: flex;"), "open shows it");
    let phone = css.split("@media (max-width: 700px) {").skip(1).map(|block| &block[..block.find("\n}").unwrap()]).find(|block| block.contains(".run-more")).expect("a phone rule for the menu");
    assert!(phone.contains(".run-more-toggle { display: none; }"), "{phone}");
    assert!(phone.contains("display: contents"), "{phone}");
}
