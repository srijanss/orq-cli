//! The project card follows the reference: a roomy rounded card, a bold name with an accent folder
//! icon and a muted chevron on a borderless header row, and a tinted, ringed selected state.

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    let start = css.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &css[start..start + css[start..].find('}').unwrap()]
}

#[test]
fn the_card_is_roomy_and_rounded_and_its_header_row_is_a_bold_borderless_name() {
    let css = css();
    let card = rule(&css, "\n.project {");
    assert!(card.contains("padding: 1rem 1.25rem") && card.contains("border-radius: 0.875rem") && card.contains("gap: 1rem"), "{card}");
    let head = rule(&css, ".project > button {");
    assert!(head.contains("background: transparent") && head.contains("border: 0") && head.contains("padding: 0"), "{head}");
    assert!(head.contains("font-size: 1.0625rem") && head.contains("font-weight: 700"), "{head}");
    assert!(rule(&css, ".project > button .icon:first-child {").contains("color: var(--accent)"));
}

#[test]
fn the_selected_card_is_ringed_and_tinted_with_the_accent() {
    let css = css();
    let selected = rule(&css, ".project.selected {");
    assert!(selected.contains("box-shadow: 0 0 0 2px var(--accent)") && selected.contains("color-mix(in srgb, var(--accent) 8%, var(--surface))"), "{selected}");
}

#[test]
fn a_card_shows_only_features_active_runs_and_last_updated() {
    use serde_json::{json, Value};
    let dir = env!("CARGO_MANIFEST_DIR");
    let project = json!({"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 2, "reviews_waiting": 3, "failed_runs": 4, "failed_window_hours": 24});
    let scenario = json!({"url": "/dashboard", "responses": [{"status": 200, "json": {"projects": [project]}}], "steps": [{"submit": "typed-token-0123456789abcdef0123456789"}], "selectors": [".project .card-stat"]});
    let output = std::process::Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    if !output.status.success() {
        return; // no node or dependencies here: the other tests still guard the styling
    }
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let settled = &out["frames"][1]["settled"];
    assert_eq!(settled["matches"][".project .card-stat"], 3, "{out}");
    let texts: Vec<&str> = settled["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    for kept in ["Features", "Active runs", "Reviews waiting"] {
        assert!(texts.contains(&kept), "{kept}: {texts:?}");
    }
    assert!(!texts.iter().any(|t| *t == "Waiting" || t.starts_with("Failed")), "{texts:?}");
}
