//! The app shell: sidebar entries, place in the address, and a phone drawer that behaves like a dialog.

mod web_deps;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn run(scenario: Value) -> Option<Value> {
    web_deps::ready().then(|| {
        let output = Command::new("node").arg(crate_dir().join("tests/web_shell_run.mjs")).arg(scenario.to_string()).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    })
}

fn names(frame: &Value) -> Vec<String> {
    frame["links"].as_array().unwrap().iter().map(|l| l["text"].as_str().unwrap().to_string()).collect()
}

fn current(frame: &Value) -> Vec<String> {
    frame["links"].as_array().unwrap().iter().filter(|l| l["current"] == "page").map(|l| l["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn without_a_project_the_sidebar_offers_the_dashboard_and_settings_only() {
    let Some(out) = run(json!({"path": "/dashboard"})) else { return };
    assert_eq!(names(&out["frames"][0]), ["Dashboard", "Settings"], "{out}");
}

#[test]
fn a_chosen_project_adds_its_features_orchestrations_and_reviews() {
    let Some(out) = run(json!({"projectId": "my app", "path": "/projects/my%20app/reviews"})) else { return };
    let frame = &out["frames"][0];
    assert_eq!(names(frame), ["Dashboard", "Features", "Orchestrations", "Reviews", "Settings"], "{out}");
    let hrefs: Vec<&str> = frame["links"].as_array().unwrap().iter().map(|l| l["href"].as_str().unwrap()).collect();
    assert_eq!(hrefs, ["/dashboard", "/projects/my%20app/features", "/projects/my%20app/orchestrations", "/projects/my%20app/reviews", "/settings"]);
    assert_eq!(current(frame), ["Reviews"], "the place comes from the address: {out}");
}

#[test]
fn a_change_of_address_moves_the_highlight() {
    let Some(out) = run(json!({"path": "/dashboard", "steps": [{"go": "/settings"}]})) else { return };
    let frame = &out["frames"][1];
    assert_eq!(frame["path"], "/settings");
    assert_eq!(current(frame), ["Settings"], "{out}");
}

#[test]
fn the_phone_drawer_opens_locks_scroll_and_traps_focus() {
    let Some(out) = run(json!({"path": "/dashboard", "steps": [
        {"click": "[data-menu-button]"},
        {"press": "Tab", "shift": true},
        {"press": "Tab"}
    ]})) else { return };
    let open = &out["frames"][1];
    assert_eq!(open["menuExpanded"], "true");
    assert_eq!(open["drawerOpen"], true);
    assert_eq!(open["scrimPresent"], true);
    assert_eq!(open["bodyOverflow"], "hidden", "page scroll is locked: {out}");
    assert_eq!(open["focus"], "close", "focus moves into the drawer: {out}");
    // Shift+Tab from the first control wraps to the last, and Tab from the last wraps back.
    assert_eq!(out["frames"][2]["focus"], "link:Settings", "{out}");
    assert_eq!(out["frames"][3]["focus"], "close", "{out}");
}

#[test]
fn escape_scrim_and_close_button_each_close_the_drawer_and_give_focus_back() {
    for closer in [json!({"press": "Escape"}), json!({"click": "[data-scrim]"}), json!({"click": "[data-close-button]"})] {
        let Some(out) = run(json!({"path": "/dashboard", "steps": [{"click": "[data-menu-button]"}, closer.clone()]})) else { return };
        let closed = &out["frames"][2];
        assert_eq!(closed["drawerOpen"], false, "{closer}: {out}");
        assert_eq!(closed["menuExpanded"], "false", "{closer}: {out}");
        assert_eq!(closed["bodyOverflow"], "", "scroll is released: {closer}: {out}");
        assert_eq!(closed["scrimPresent"], false, "{closer}: {out}");
        assert_eq!(closed["focus"], "menu", "focus returns to the trigger: {closer}: {out}");
    }
}

#[test]
fn the_desktop_rail_collapses_and_expands() {
    let Some(out) = run(json!({"path": "/dashboard", "steps": [{"click": "[data-rail-button]"}, {"click": "[data-rail-button]"}]})) else { return };
    assert_eq!(out["frames"][0]["railCollapsed"], false);
    assert_eq!(out["frames"][1]["railCollapsed"], true, "{out}");
    assert_eq!(out["frames"][2]["railCollapsed"], false, "{out}");
}

#[test]
fn the_rail_is_64px_wide_when_collapsed_and_the_drawer_has_a_phone_layout() {
    let css = std::fs::read_to_string(crate_dir().join("web/app.css")).unwrap();
    assert!(css.contains(".shell.collapsed") && css.contains("64px"), "{css}");
    assert!(css.contains("@media (max-width: 700px)"), "a phone layout exists");
}

#[test]
fn the_served_page_is_framed_by_the_shell_only_after_signing_in() {
    if !web_deps::ready() {
        return;
    }
    let scenario = json!({"responses": [{"status": 200, "json": {"reviews": []}}], "steps": [{"submit": "token"}]});
    let output = Command::new("node")
        .arg(crate_dir().join("tests/web_react_harness.mjs"))
        .arg(crate_dir().join("web/app.react.js"))
        .arg(scenario.to_string())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(out["frames"][0]["settled"]["shell"], false, "no navigation before signing in: {out}");
    assert_eq!(out["frames"][1]["settled"]["shell"], true, "the signed-in page sits inside the shell: {out}");
}

#[test]
fn choosing_a_destination_closes_the_drawer_and_releases_scroll() {
    for link in ["nav a:nth-child(1)", "nav a:nth-child(2)"] {
        let Some(out) = run(json!({"path": "/dashboard", "steps": [{"click": "[data-menu-button]"}, {"click": link}]})) else { return };
        let after = &out["frames"][2];
        assert_eq!(after["drawerOpen"], false, "{link}: {out}");
        assert_eq!(after["bodyOverflow"], "", "{link}: {out}");
        assert_eq!(after["scrimPresent"], false, "{link}: {out}");
    }
}

#[test]
fn growing_past_the_phone_width_closes_an_open_drawer() {
    let Some(out) = run(json!({"path": "/dashboard", "steps": [{"click": "[data-menu-button]"}, {"viewport": "desktop"}]})) else { return };
    let after = &out["frames"][2];
    assert_eq!(after["drawerOpen"], false, "{out}");
    assert_eq!(after["bodyOverflow"], "", "scroll is released: {out}");
    assert_eq!(after["menuExpanded"], "false", "{out}");
}

#[test]
fn destinations_without_a_page_say_so_instead_of_showing_the_inbox() {
    if !web_deps::ready() {
        return;
    }
    for hash in ["/dashboard", "/settings", "/projects/app/features"] {
        let scenario = json!({"responses": [{"status": 200, "json": {"reviews": []}}], "steps": [{"submit": "token"}, {"go": hash}]});
        let output = Command::new("node")
            .arg(crate_dir().join("tests/web_react_harness.mjs"))
            .arg(crate_dir().join("web/app.react.js"))
            .arg(scenario.to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let out: Value = serde_json::from_slice(&output.stdout).unwrap();
        let shown = out["frames"][2]["settled"]["texts"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect::<Vec<_>>().join(" | ");
        assert!(shown.contains("not available yet"), "{hash}: {shown}");
        assert!(!shown.to_lowercase().contains("no reviews"), "{hash}: the inbox must not show under another destination: {shown}");
    }
}
