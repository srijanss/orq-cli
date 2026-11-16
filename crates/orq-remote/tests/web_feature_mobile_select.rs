//! On a phone the features list can select several features: a Select button (or a long press on a row) enters
//! selection mode, where each draft or pending row shows its mark and a tap on the row toggles it instead of
//! opening the feature. While anything is marked a bar fixed at the bottom says how many are selected and holds
//! the same Promote N and Start N as a wide screen. Done, a successful action or a change of filter, state or
//! page clears the marks and leaves the mode.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 17] = [
    "[data-select-mode]",
    "[data-select-mode][aria-pressed=\"true\"]",
    ".selecting [data-mark]",
    "[data-mark]:checked",
    ".marked-actions [data-selected-count][aria-live=\"polite\"]",
    "[data-start-marked]",
    "tr.feature.swiped",
    "[data-confirm]",
    ".selecting",
    "tr.feature.marked",
    "tr.feature.marked:has([data-mark]:checked)",
    "[data-start-marked].primary-button:not(.secondary-button)",
    "[data-promote-marked].secondary-button",
    ".marked-actions > .marked-group",
    "tr.feature.marked [data-mark][aria-label^=\"Mark \"]:checked",
    ".feature-table[aria-multiselectable]",
    "tr.feature[aria-selected]",
];

fn feature(name: &str, status: &str) -> Value {
    json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null})
}

fn ledger_of(features: Vec<Value>) -> Value {
    json!({"status": 200, "json": {"features": features, "remote_actions": true}})
}

fn ledger() -> Value {
    ledger_of(vec![feature("d1", "draft"), feature("p1", "pending"), feature("p2", "pending"), feature("c1", "completed")])
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn run_with(responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    run_on(true, responses, steps)
}

fn run_on(phone: bool, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let scenario = json!({"url": "/projects/alpha/features", "phone": phone, "viewport": if phone { json!({"width": 390, "height": 844}) } else { json!({"width": 1280, "height": 800}) }, "responses": responses, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn run(steps: Vec<Value>) -> Option<Value> {
    run_with(vec![ledger(), projects()], steps)
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

// A finger tap on the row's link: a real click event, which the harness's stubbed link click() would not send.
fn tap(name: &str) -> Value {
    json!({"nav": format!("[data-feature-open=\"{name}\"]")})
}

fn select() -> Value {
    json!({"click": "[data-select-mode]"})
}

#[test]
fn select_shows_the_marks_and_tapping_two_rows_marks_both_without_opening_them() {
    let Some(out) = run(vec![select(), tap("d1"), tap("p1")]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], SELECTORS[0]), 1, "a Select button: {out}");
    assert_eq!(count(&frames[1], SELECTORS[2]), 0, "no marks shown before Select: {out}");
    assert_eq!(count(&frames[2], SELECTORS[1]), 1, "the button is pressed: {out}");
    assert!(texts(&frames[2]).iter().any(|t| t == "Done"), "the button now says Done: {out}");
    assert_eq!(count(&frames[2], SELECTORS[2]), 3, "a mark on each draft and pending row: {out}");
    assert_eq!(count(&frames[4], SELECTORS[3]), 2, "both rows marked: {out}");
    assert_eq!(frames[4]["settled"]["path"], "/projects/alpha/features", "no feature was opened: {out}");
    assert_eq!(count(&frames[4], SELECTORS[4]), 1, "the bar counts the selection: {out}");
    assert!(texts(&frames[4]).iter().any(|t| t == "2 selected"), "{out}");
}

fn posts(out: &Value) -> Vec<Value> {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").cloned().collect()
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

#[test]
fn start_2_from_the_bar_sends_one_group_start_and_a_success_leaves_selection_mode() {
    let profiles = json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}});
    let started = json!({"status": 202, "json": {"run": "run-1"}});
    let steps = vec![select(), tap("p1"), tap("p2"), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run_with(vec![ledger(), projects(), profiles, started, ledger(), projects()], steps) else { return };
    assert!(texts(&out["frames"][4]).iter().any(|t| t == "Start 2"), "{out}");
    let sent = posts(&out);
    assert_eq!(sent.len(), 1, "one start request: {out}");
    assert_eq!(sent[0]["url"], "/api/projects/alpha/features/p1/runs", "{out}");
    assert_eq!(sent[0]["body"], r#"{"group":["p1","p2"],"implementer":"opus","reviewer":"sonnet"}"#, "{out}");
    let end = last(&out);
    assert!(texts(end).iter().any(|t| t == "Started group run run-1 for p1, p2."), "the result shows: {out}");
    assert_eq!(count(end, SELECTORS[8]), 0, "selection mode is left: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "no marks remain: {out}");
    assert!(texts(end).iter().any(|t| t == "Select"), "the button says Select again: {out}");
}

// The marked rows say so to assistive technology through their labelled checkboxes, which stay in the page
// though hidden: the feature table is a native table, where aria-multiselectable and aria-selected mean
// nothing, so neither is set, while selecting or after Done.
#[test]
fn a_marked_row_is_announced_by_its_checked_mark_and_the_table_has_no_selection_aria() {
    let Some(out) = run(vec![select(), tap("d1"), tap("p1"), select()]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[4], SELECTORS[14]), 2, "both tapped rows have a checked, labelled mark: {out}");
    for frame in frames {
        assert_eq!(count(frame, SELECTORS[15]), 0, "no aria-multiselectable on a native table: {out}");
        assert_eq!(count(frame, SELECTORS[16]), 0, "no aria-selected on a native table's rows: {out}");
    }
    assert_eq!(count(last(&out), SELECTORS[3]), 0, "Done clears the marks: {out}");
}

#[test]
fn done_clears_the_marks_and_hides_the_bar() {
    let Some(out) = run(vec![select(), tap("p1"), tap("d1"), select()]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[1]), 0, "the button is no longer pressed: {out}");
    assert_eq!(count(end, SELECTORS[2]), 0, "the marks are hidden again: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "no marks remain: {out}");
    assert_eq!(count(end, SELECTORS[4]), 0, "the bar is gone: {out}");
    assert_eq!(count(end, SELECTORS[5]), 0, "{out}");
    assert!(posts(&out).is_empty(), "{out}");
}

fn press(name: &str, dy: i64) -> Value {
    json!({"press": format!("[data-feature-open=\"{name}\"]"), "ms": 600, "dy": dy})
}

#[test]
fn a_long_press_enters_selection_mode_with_that_row_marked_and_a_moving_finger_does_not() {
    let Some(out) = run(vec![press("p1", 0)]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 1, "selection mode: {out}");
    assert_eq!(count(end, SELECTORS[3]), 1, "the pressed row is marked: {out}");
    assert!(texts(end).iter().any(|t| t == "1 selected"), "{out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/features", "the press opened nothing: {out}");

    let Some(out) = run(vec![press("p1", 30)]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 0, "a scroll is no long press: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "{out}");
}

fn swipe(name: &str) -> Value {
    json!({"swipe": format!("[data-feature-row=\"{name}\"]"), "dx": -90})
}

#[test]
fn select_closes_a_swiped_row_and_a_swipe_while_selecting_does_nothing() {
    let Some(out) = run(vec![swipe("p1"), select(), swipe("p2")]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[2], SELECTORS[6]), 1, "the swipe opened the row: {out}");
    assert_eq!(count(&frames[3], SELECTORS[6]), 0, "Select closed it: {out}");
    assert_eq!(count(&frames[4], SELECTORS[6]), 0, "no swipe while selecting: {out}");
    assert_eq!(count(&frames[4], SELECTORS[3]), 0, "nor a mark: {out}");
}

#[test]
fn a_list_with_nothing_to_mark_has_no_select_button() {
    let only_done = ledger_of(vec![feature("c1", "completed"), feature("c2", "completed")]);
    let Some(out) = run_with(vec![only_done, projects()], vec![]) else { return };
    assert_eq!(count(last(&out), SELECTORS[0]), 0, "{out}");
}

#[test]
fn a_change_of_state_filter_or_page_leaves_selection_mode() {
    let steps = vec![select(), tap("p1"), json!({"set": "[data-feature-status]", "value": "pending"})];
    let Some(out) = run(steps) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 0, "{out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "the marks are cleared: {out}");

    let steps = vec![select(), tap("p1"), json!({"set": "[data-feature-search]", "value": "p"})];
    let Some(out) = run(steps) else { return };
    assert_eq!(count(last(&out), SELECTORS[8]), 0, "{out}");
    assert_eq!(count(last(&out), SELECTORS[3]), 0, "{out}");
}

#[test]
fn the_css_hides_the_marks_outside_selection_and_keeps_the_bar_inside_the_safe_area() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let phone = &css[css.find("@media (max-width: 700px)").expect("a phone query")..];
    assert!(phone.contains("  .feature-mark, .marked-actions { display: none; }"), "a phone hides the marks unless selecting");
    assert!(css.lines().any(|line| line == ".select-mode { display: none; }"), "a wide screen has no Select button");
    let mark = phone.lines().find(|line| line.starts_with("  .selecting .feature-mark {")).expect("a selecting mark rule");
    assert!(mark.contains("display: block"), "{mark}");
    let bar = phone.lines().find(|line| line.starts_with("  .selecting .marked-actions {")).expect("a selecting bar rule");
    assert!(bar.contains("position: fixed") && bar.contains("env(safe-area-inset-bottom)") && bar.contains("display: flex"), "{bar}");
    assert!(phone.contains("  .feature-list.selecting { padding-bottom:"), "the list keeps room under its last row");
}

#[test]
fn a_long_press_on_a_wide_screen_does_not_enter_selection_mode() {
    let Some(out) = run_on(false, vec![ledger(), projects()], vec![press("p1", 0)]) else { return };
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[8]), 0, "a wide screen has no selection mode: {out}");
    assert_eq!(end["settled"]["path"], "/projects/alpha/features/p1", "the press is a plain tap that opens the feature: {out}");
}

// The every-project list puts one Promote and one Start per project in the bar, and Start's question opens
// in it too: the bar wraps them onto more lines and, past a capped height, scrolls rather than pushing a
// button off the screen.
#[test]
fn the_bar_wraps_its_buttons_and_scrolls_past_a_capped_height() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let phone = &css[css.find("@media (max-width: 700px) {\n  .feature-mark").expect("the selection query")..];
    let bar = phone.lines().find(|line| line.starts_with("  .selecting .marked-actions {")).expect("a selecting bar rule");
    assert!(bar.contains("flex-wrap: wrap;") && !bar.contains("nowrap"), "{bar}");
    assert!(bar.contains("max-height:") && bar.contains("overflow-y: auto"), "{bar}");
}

fn gets(out: &Value, url: &str) -> usize {
    out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "GET" && r["url"] == url).count()
}

// A refused start reports the refusal and stays in selection mode with the marks cleared, so the features
// can be marked again and retried; nothing changed, so the ledger is not fetched again.
#[test]
fn a_refused_start_reports_it_and_stays_in_selection_mode_with_the_marks_cleared() {
    let profiles = json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}});
    let refused = json!({"status": 409, "json": {"error": "feature p1 already has a run"}});
    let steps = vec![select(), tap("p1"), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run_with(vec![ledger(), projects(), profiles, refused], steps) else { return };
    let end = last(&out);
    assert_eq!(posts(&out).len(), 1, "{out}");
    assert!(texts(end).iter().any(|t| t.contains("feature p1 already has a run")), "the refusal shows: {out}");
    assert_eq!(count(end, SELECTORS[8]), 1, "still selecting: {out}");
    assert_eq!(count(end, SELECTORS[1]), 1, "the button still says Done: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "the marks are cleared: {out}");
    assert_eq!(count(end, SELECTORS[4]), 0, "with nothing marked the bar is gone: {out}");
    assert_eq!(gets(&out, "/api/projects/alpha/features"), 1, "no refresh: {out}");
}

// A promote of two where one is refused still changed the ledger: it reports both, refreshes the list and
// leaves selection mode.
#[test]
fn a_promote_with_one_refusal_reports_both_refreshes_and_leaves_selection_mode() {
    let drafts = || ledger_of(vec![feature("d1", "draft"), feature("d2", "draft"), feature("p1", "pending")]);
    let ok = json!({"status": 200, "json": {"feature": "d1", "status": "pending"}});
    let refused = json!({"status": 409, "json": {"error": "feature d2: Pending to Pending is not allowed"}});
    let steps = vec![select(), tap("d1"), tap("d2"), json!({"click": "[data-promote-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run_with(vec![drafts(), projects(), ok, refused, drafts(), projects()], steps) else { return };
    let end = last(&out);
    let sent = posts(&out);
    let urls: Vec<&str> = sent.iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["/api/projects/alpha/features/d1/promote", "/api/projects/alpha/features/d2/promote"], "{out}");
    assert!(texts(end).iter().any(|t| t.contains("feature d2: Pending to Pending is not allowed")), "the refusal shows: {out}");
    assert_eq!(count(end, SELECTORS[8]), 0, "selection mode is left: {out}");
    assert_eq!(count(end, SELECTORS[3]), 0, "{out}");
    assert_eq!(gets(&out, "/api/projects/alpha/features"), 2, "the list is refreshed: {out}");
}

// A marked row is told apart by a class the CSS fills, not only by its box; in the bar Start is the primary
// action and Promote the secondary one.
#[test]
fn a_marked_row_carries_the_marked_class_and_the_bar_has_start_as_primary_and_promote_as_secondary() {
    let Some(out) = run(vec![select(), tap("d1"), tap("p1")]) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[2], SELECTORS[9]), 0, "no row marked yet: {out}");
    let end = last(&out);
    assert_eq!(count(end, SELECTORS[9]), 2, "only the two marked rows carry the class: {out}");
    assert_eq!(count(end, SELECTORS[10]), 2, "and they are the checked ones: {out}");
    assert_eq!(count(end, SELECTORS[11]), 1, "Start is the primary button: {out}");
    assert_eq!(count(end, SELECTORS[12]), 1, "Promote is the secondary button: {out}");
}

fn rule<'a>(css: &'a str, start: &str) -> &'a str {
    css.lines().find(|line| line.starts_with(start)).unwrap_or_else(|| panic!("a rule starting {start:?}"))
}

// The phone's selection mode looks like the rest of the phone view: no checkbox and no gutter, the mark stays in
// the page only for assistive technology and the keyboard, and a marked row reads like a quote: an accent bar
// down its left edge, a tint and its name in the text colour, while a row that can be marked shows a faint bar
// (the whole row, at least 44px tall, is the tap target). A text-style Select/Done and a bottom sheet whose
// Start is primary; colours come only from the theme tokens and nothing moves under reduced motion.
#[test]
fn the_phone_selection_mode_marks_a_row_with_a_quote_bar_a_tint_and_a_bottom_sheet_bar() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let from = css.find("@media (max-width: 700px) {\n  .feature-mark").expect("the selection query");
    let phone = &css[from..from + css[from..].find("\n}\n").expect("the query's end")];

    let mark = rule(phone, "  .selecting .feature-mark {");
    for hidden in ["position: absolute", "width: 1px", "height: 1px", "overflow: hidden", "clip-path: inset(50%)", "white-space: nowrap", "min-height: 0", "min-width: 0", "padding: 0"] {
        assert!(mark.contains(hidden), "the mark is visually hidden ({hidden}): {mark}");
    }
    assert!(!mark.contains("display: none") && !mark.contains("22px") && !mark.contains("border-radius"), "the mark stays focusable and draws nothing: {mark}");
    for gone in ["  .selecting .feature-mark:checked", "  .selecting .feature-mark:focus-visible {", "  .selecting .feature-table :is(th, td):first-child {"] {
        assert!(!phone.lines().any(|line| line.starts_with(gone)), "no drawn mark or gutter left ({gone}): {phone}");
    }
    assert!(!phone.contains("padding-left: calc(0.75rem + 2rem)"), "no gutter for the mark");

    let focus = rule(phone, "  .selecting .feature-table tr.feature:has(.feature-mark:focus-visible) {");
    assert!(focus.contains("outline: 3px solid var(--accent)") && focus.contains("outline-offset: -3px"), "the keyboard sees the row it is on: {focus}");

    let row = rule(phone, "  .selecting .feature-table tr.feature {");
    assert!(row.contains("height: 44px") && row.contains("120ms"), "a selecting row is at least 44px tall: {row}");
    let faint = rule(phone, "  .selecting .feature-table tr.feature:has(.feature-mark) > td:first-child {");
    assert!(faint.contains("box-shadow: inset 2px 0 0 var(--border)") && faint.contains("transition: box-shadow 120ms"), "a row that can be marked has a faint bar: {faint}");
    let bar = rule(phone, "  .selecting .feature-table tr.feature.marked > td:first-child {");
    assert!(bar.contains("box-shadow: inset 4px 0 0 var(--accent)"), "a marked row has the accent bar: {bar}");
    let name = rule(phone, "  .selecting .feature-table tr.feature.marked .feature-link {");
    assert!(name.contains("color: var(--text)") && name.contains("font-weight: 600"), "{name}");
    let dim = rule(phone, "  .selecting .feature-table tr.feature:not(:has(.feature-mark)) {");
    assert!(dim.contains("opacity: 0.55") && !dim.contains("box-shadow"), "rows that cannot be marked are dimmed, with no bar: {dim}");
    assert!(rule(phone, "  .selecting .feature-table tr.feature:not(:has(.feature-mark)) {").contains("opacity: 0.55"), "rows that cannot be marked are dimmed");
    assert!(rule(phone, "  .selecting .feature-table tr.feature.marked {").contains("background: color-mix(in srgb, var(--accent) 12%, transparent)"));

    let toggle = rule(phone, "  .select-mode {");
    assert!(toggle.contains("border: 0") && toggle.contains("color: var(--accent)") && toggle.contains("min-height: 44px"), "{toggle}");
    assert!(rule(phone, "  .select-mode[aria-pressed=\"true\"] {").contains("font-weight: 700"), "Done is bold");

    let bar = rule(phone, "  .selecting .marked-actions {");
    assert!(bar.contains("border-radius: 0.75rem 0.75rem 0 0") && bar.contains("animation: marked-actions-in 150ms"), "{bar}");
    assert!(rule(phone, "  .selecting .marked-actions .selected-count {").contains("color: var(--text-muted)"));
    assert!(rule(phone, "  .selecting .marked-group > .run-action {").contains("flex: 1 1 0"), "two buttons share the row equally");
    let secondary = rule(phone, "  .selecting .marked-actions .secondary-button {");
    assert!(secondary.contains("background: transparent") && secondary.contains("border-color: var(--border)"), "{secondary}");
    assert!(css.contains("@keyframes marked-actions-in"), "the bar slides up");

    let calm = &css[css.find("@media (max-width: 700px) and (prefers-reduced-motion: reduce) {").expect("a reduced-motion rule")..];
    let calm = &calm[..calm.find("\n}\n").unwrap()];
    assert!(calm.contains(".selecting .feature-table tr.feature > td:first-child") && calm.contains(".selecting .feature-table tr.feature,") && calm.contains(".selecting .marked-actions") && calm.contains("animation: none") && calm.contains("transition: none"), "{calm}");

    assert!(!phone.contains('#'), "no hard-coded colours outside the tokens: {phone}");
    assert!(css.lines().any(|line| line == ".marked-group { display: contents; }"), "a wide screen keeps its marked-actions layout");

    let Some(out) = run(vec![select(), tap("d1"), tap("p1")]) else { return };
    assert_eq!(count(last(&out), SELECTORS[13]), 1, "the project's pair sits in its own group: {out}");
}

// The sheet is fixed over the list's end and grows with each project's buttons, Start's question or a refusal,
// so the list keeps the sheet's measured height free below its last row rather than a fixed guess.
#[test]
fn the_list_reserves_the_sheets_measured_height_below_its_last_row_while_selecting() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let steps = vec![json!({"submit": TOKEN}), select(), tap("d1"), tap("p1"), select()];
    let scenario = json!({"url": "/projects/alpha/features", "phone": true, "viewport": {"width": 390, "height": 844}, "responses": [ledger(), projects()], "steps": steps, "selectors": SELECTORS, "styles": [".feature-list"], "rects": [{"selector": ".marked-actions", "top": 520, "left": 0, "width": 390, "height": 212}]});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let style = |frame: &Value| frame["settled"]["styles"][".feature-list"].as_str().unwrap_or("").to_string();
    assert!(style(&out["frames"][4]).contains("--marked-actions-height: 212px"), "the sheet's height is kept free: {out}");
    assert!(!style(last(&out)).contains("--marked-actions-height"), "Done drops it: {out}");
    let css = std::fs::read_to_string(format!("{dir}/web/app.css")).unwrap();
    assert!(css.contains("  .feature-list.selecting { padding-bottom: calc(var(--marked-actions-height, 5rem) + 1rem); }"), "the list's padding follows the measured sheet");
}

fn sheet(height: u64) -> Value {
    json!([{"selector": ".marked-actions", "top": 844 - 60 - height, "left": 0, "width": 390, "height": height}])
}

// The sheet grows when Start's question opens in it (or another project's buttons or a refusal join it) and
// shrinks again; the list's reservation follows each resize, and the watch ends with selection mode.
#[test]
fn the_reserved_height_follows_the_sheet_as_it_grows_and_shrinks_and_the_watch_ends_on_done() {
    if !web_deps::ready() {
        return;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let profiles = json!({"status": 200, "json": {"profiles": ["haiku", "opus", "sonnet"], "implementer": "opus", "reviewer": "sonnet"}});
    let steps = vec![json!({"submit": TOKEN}), select(), tap("d1"), tap("p1"), json!({"click": "[data-start-marked]"}), json!({"resize": sheet(300)}), json!({"resize": sheet(120)}), select()];
    let scenario = json!({"url": "/projects/alpha/features", "phone": true, "resize_observer": true, "viewport": {"width": 390, "height": 844}, "responses": [ledger(), projects(), profiles], "steps": steps, "selectors": SELECTORS, "styles": [".feature-list"], "rects": sheet(160)});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let style = |i: usize| out["frames"][i]["settled"]["styles"][".feature-list"].as_str().unwrap_or("").to_string();
    assert!(style(4).contains("--marked-actions-height: 160px"), "measured when it appears: {out}");
    assert_eq!(out["frames"][4]["settled"]["watched"], 1, "the sheet is watched: {out}");
    assert_eq!(count(&out["frames"][5], SELECTORS[7]), 1, "Start's question is open in the sheet: {out}");
    assert!(style(6).contains("--marked-actions-height: 300px"), "the reservation grows with the sheet: {out}");
    assert!(style(7).contains("--marked-actions-height: 120px"), "and shrinks with it: {out}");
    assert!(!style(8).contains("--marked-actions-height"), "Done drops it: {out}");
    assert_eq!(out["frames"][8]["settled"]["watched"], 0, "and stops watching the sheet: {out}");
}
