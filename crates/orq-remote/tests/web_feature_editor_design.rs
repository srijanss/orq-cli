//! The feature editor follows the design system. Add feature is a primary button with its plus in the list's
//! toolbar. The form is a grid of labelled fields, each label above its full-width input, with Save as a primary
//! button and its question a primary confirm. "Depends on" stays usable with a large ledger: the chosen features
//! first, then at most eight more that match a search field, never the whole ledger at once.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 8] = [
    ".feature-filters > .feature-editor > button.primary-button[data-feature-add] svg",
    ".feature-editor-fields label.field > .field-label + input[data-field-description]",
    ".feature-editor-actions button.primary-button[data-feature-save]",
    ".confirm .confirm-question + .confirm-actions button.primary-button[data-confirm]",
    "[data-field-depends]",
    "[data-depends-search]",
    "[data-field-depends]:checked",
    "[data-field-depends=\"f12\"]",
];

fn ledger(count: u32) -> Value {
    let feature = |n: u32| json!({"name": format!("f{n:02}"), "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": (1..=count).map(feature).collect::<Vec<_>>(), "remote_actions": true}})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn run(count: u32, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": "/projects/alpha/features", "responses": [ledger(count), projects()], "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} not asked for"))
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn add_is_a_primary_button_in_the_toolbar_and_the_form_is_labelled_fields_with_a_primary_save() {
    let steps = vec![json!({"click": "[data-feature-add]"}), json!({"set": "[data-field-name]", "value": "new-one"}), json!({"click": "[data-feature-save]"})];
    let Some(out) = run(3, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[0]), 1, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 1, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[2]), 1, "{out}");
    assert_eq!(count(&frames[2], SELECTORS[4]), 3, "a small ledger shows every feature: {out}");
    assert_eq!(count(&frames[2], SELECTORS[5]), 0, "and needs no search: {out}");
    assert_eq!(count(last(&out), SELECTORS[3]), 1, "{out}");
}

#[test]
fn a_large_ledger_offers_eight_matches_of_a_search_and_keeps_the_chosen_ones_first() {
    let steps = vec![
        json!({"click": "[data-feature-add]"}),
        json!({"set": "[data-depends-search]", "value": "2"}),
        json!({"click": "[data-field-depends=\"f12\"]"}),
        json!({"set": "[data-depends-search]", "value": "f0"}),
    ];
    let Some(out) = run(20, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[2], SELECTORS[4]), 8, "at most eight before a search: {out}");
    assert_eq!(count(&frames[2], SELECTORS[5]), 1, "{out}");
    assert_eq!(count(&frames[3], SELECTORS[4]), 3, "f02, f12 and f20 match 2: {out}");
    let after = last(&out);
    assert_eq!(count(after, SELECTORS[6]), 1, "{out}");
    assert_eq!(count(after, SELECTORS[7]), 1, "the chosen one stays while the search moves on: {out}");
    assert_eq!(count(after, SELECTORS[4]), 9, "it and eight of f01..f09: {out}");
}
