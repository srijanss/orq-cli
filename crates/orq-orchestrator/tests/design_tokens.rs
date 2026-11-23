//! The design tokens: tokens.css and tokens.json parsed, their values normalised (colours to RGB,
//! sizes to px, weights to numbers), and the `## Token rules` of DESIGN.md read.

use orq_orchestrator::{
    apply_token_issues, check_design_tokens, normalize_token_value, DesignReview, DesignVerdict, IssueStatus,
    ReviewIssue, Severity, parse_token_rules, parse_tokens_css, parse_tokens_json, Rgba,
    RuleStatus, TokenBinding, TokenCheckStatus, TokenRule, TokenValue,
};
use orq_test_support::TempDir;
use serde_json::{json, Value};

/// A run's design folder with `tokens.css`, a DESIGN.md of `rules`, and round 1 measured at desktop.
fn design_folder(tokens: &str, rules: &str, elements: Vec<Value>, selectors: Value) -> TempDir {
    let dir = TempDir::new("design-tokens");
    let package = dir.path().join("package");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("tokens.css"), tokens).unwrap();
    std::fs::write(package.join("DESIGN.md"), format!("# Page\n\n## Token rules\n\n{rules}\n")).unwrap();
    let folder = dir.path().join("round-1").join("desktop");
    std::fs::create_dir_all(&folder).unwrap();
    let page = json!({"document": {"root_font_size": 16}, "elements": elements, "selectors": selectors});
    let measured = json!({"viewport": "desktop", "routes": {"/": page}});
    std::fs::write(folder.join("measurements.json"), measured.to_string()).unwrap();
    dir
}

fn measured(styles: Value) -> Value {
    json!({"box": {"x": 0, "y": 0, "width": 10, "height": 10}, "styles": styles})
}

fn review_issue(id: &str, source: &str, severity: Severity, status: IssueStatus) -> ReviewIssue {
    ReviewIssue {
        id: id.to_string(),
        section: "h1".to_string(),
        viewport: "desktop".to_string(),
        source: source.to_string(),
        expected: "expected".to_string(),
        actual: "actual".to_string(),
        evidence: vec![],
        severity,
        status,
        fix_direction: "fix".to_string(),
        selector: None,
        confidence: None,
    }
}

#[test]
fn the_reviewer_cannot_dismiss_or_downgrade_a_violated_token_rule() {
    let dir = design_folder(
        ":root { --weight-bold: 700; }",
        "h1: font-weight --weight-bold",
        vec![json!({"tag": "h1", "index": 0, "styles": {"font-weight": "600"}})],
        json!({}),
    );
    let check = check_design_tokens(dir.path(), 1);
    let token_issue = check.issues().remove(0);
    let mut review = DesignReview {
        run_id: "run".to_string(),
        reference_hash: "hash".to_string(),
        capture_revision: "capture".to_string(),
        evaluation: "looks right".to_string(),
        verdict: DesignVerdict::Pass,
        checks: vec![],
        issues: vec![
            review_issue(&token_issue.id, "tokens", Severity::Minor, IssueStatus::Resolved),
            review_issue("hero-spacing", "references/desktop.png", Severity::Minor, IssueStatus::Open),
        ],
        suggestions: vec![],
        blockers: vec![],
    };

    apply_token_issues(&mut review, &check.issues());

    assert_eq!(review.verdict, DesignVerdict::ChangesRequired);
    assert_eq!(review.issues.len(), 2, "{:#?}", review.issues);
    let kept = review.issues.iter().find(|i| i.id == token_issue.id).unwrap();
    assert_eq!(kept, &token_issue);
    assert_eq!(kept.severity, Severity::Major);
    assert_eq!(kept.status, IssueStatus::Open);
    assert!(review.issues.iter().any(|i| i.id == "hero-spacing"));

    let mut blocked = review.clone();
    blocked.verdict = DesignVerdict::Blocked;
    apply_token_issues(&mut blocked, &check.issues());
    assert_eq!(blocked.verdict, DesignVerdict::Blocked);
    assert_eq!(blocked.issues.len(), 2);
}

#[test]
fn in_infer_mode_the_prepared_token_mapping_supplies_the_rules_and_a_matching_page_within_tolerance_passes() {
    let dir = design_folder(
        ":root { --font-title: 2rem; --lh: 1.5; --ink: #111111; --pad: 12px; --gap: 8px; }",
        "",
        vec![],
        json!({".hero": [measured(json!({
            "font-size": "32.6px",
            "line-height": "48.5px",
            "color": "rgb(18, 17, 17)",
            "padding-top": "12px", "padding-right": "12.8px", "padding-bottom": "11.5px", "padding-left": "12px",
            "gap": "8px",
        }))]}),
    );
    std::fs::remove_file(dir.path().join("package").join("DESIGN.md")).unwrap();
    let prepared = dir.path().join("prepared");
    std::fs::create_dir_all(&prepared).unwrap();
    std::fs::write(
        prepared.join("tokens-mapping.md"),
        "# Token mapping\n\n## Token rules\n\n.hero: font-size --font-title; line-height --lh; color --ink; padding --pad; gap --gap\n",
    )
    .unwrap();

    let check = check_design_tokens(dir.path(), 1);

    assert_eq!(check.status, TokenCheckStatus::Pass, "{:#?}", check.rules);
    assert_eq!(check.rules.len(), 5);
    assert!(check.rules.iter().all(|r| r.status == RuleStatus::Pass && r.matched == 1));
}

#[test]
fn an_off_token_colour_or_font_size_on_a_landmark_is_a_minor_warning_only() {
    let landmark = |tag: &str, styles: Value| json!({"tag": tag, "index": 0, "styles": styles});
    let dir = design_folder(
        ":root { --ink: #111111; --font-body: 1rem; --surface: #ffffff; }",
        "",
        vec![
            landmark(
                "main",
                json!({"color": "rgb(17, 17, 17)", "background-color": "rgba(0, 0, 0, 0)", "font-size": "16px"}),
            ),
            landmark(
                "h1",
                json!({"color": "rgb(255, 0, 0)", "background-color": "rgb(255, 255, 255)", "font-size": "19px"}),
            ),
        ],
        json!({}),
    );

    let check = check_design_tokens(dir.path(), 1);

    assert_eq!(check.status, TokenCheckStatus::Pass, "{check:?}");
    assert!(check.issues().is_empty());
    let mut warned: Vec<(String, String, String)> = check
        .warnings
        .iter()
        .map(|w| (w.selector.clone(), w.property.clone(), w.measured.clone()))
        .collect();
    warned.sort();
    assert_eq!(
        warned,
        vec![
            ("h1".to_string(), "color".to_string(), "rgb(255, 0, 0)".to_string()),
            ("h1".to_string(), "font-size".to_string(), "19px".to_string()),
        ]
    );
    assert!(check.warnings.iter().all(|w| w.severity == "minor" && w.viewport == "desktop"));
    let written: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("round-1").join("token-check.json")).unwrap()).unwrap();
    assert_eq!(written["warnings"].as_array().unwrap().len(), 2);
}

#[test]
fn a_package_with_no_tokens_file_is_not_assessed_never_passed() {
    let dir = design_folder(":root {}", "h1: font-weight --weight-bold", vec![], json!({}));
    std::fs::remove_file(dir.path().join("package").join("tokens.css")).unwrap();

    let check = check_design_tokens(dir.path(), 1);

    assert_eq!(check.status, TokenCheckStatus::NotAssessed);
    assert!(check.reason.as_deref().is_some_and(|r| r.contains("tokens")), "{:?}", check.reason);
    assert!(check.issues().is_empty());
    let written: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("round-1").join("token-check.json")).unwrap()).unwrap();
    assert_eq!(written["status"], "not_assessed");
}

#[test]
fn a_rule_whose_selector_matches_nothing_is_reported_and_becomes_an_issue() {
    let dir = design_folder(
        ":root { --pending-fg: #b45309; }",
        ".feature-state.state-pending: color --pending-fg",
        vec![measured(json!({"color": "rgb(0, 0, 0)"}))],
        json!({".feature-state.state-pending": []}),
    );

    let check = check_design_tokens(dir.path(), 1);

    assert_eq!(check.status, TokenCheckStatus::Fail);
    assert_eq!(check.rules.len(), 1, "{:?}", check.rules);
    assert_eq!(check.rules[0].status, RuleStatus::NoMatch);
    assert_eq!(check.rules[0].matched, 0);
    assert_eq!(check.rules[0].viewport, "desktop");
    let issues = check.issues();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].source, "tokens");
    assert_eq!(issues[0].selector.as_deref(), Some(".feature-state.state-pending"));
    assert!(issues[0].actual.contains("matches nothing"), "{}", issues[0].actual);
}

#[test]
fn a_2px_radius_against_radius_none_0_fails() {
    let dir = design_folder(
        ":root { --radius-none: 0; }",
        ".card: border-radius --radius-none",
        vec![],
        json!({".card": [measured(json!({"border-radius": "2px"}))]}),
    );

    let check = check_design_tokens(dir.path(), 1);

    assert_eq!(check.status, TokenCheckStatus::Fail);
    assert_eq!(check.rules.len(), 1, "{:?}", check.rules);
    let rule = &check.rules[0];
    assert_eq!(rule.status, RuleStatus::Fail);
    assert_eq!(rule.selector, ".card");
    assert_eq!(rule.property.as_deref(), Some("border-radius"));
    assert_eq!(rule.expected.as_deref(), Some("0px"));
    assert_eq!(rule.measured.as_deref(), Some("2px"));
}

#[test]
fn the_token_rules_section_binds_selectors_to_tokens_one_rule_per_line() {
    let rules = parse_token_rules(
        "# Checkout\n\nh1: font-size --nope\n\n## Token rules\n\n\
         h1: font-size --font-title; font-weight --weight-bold\n\
         - `.feature-state.state-pending: color --pending-fg; background --pending-bg`\n\
         a:hover: color --link-hover\n\
         not a rule\n\
         p: padding 12px\n\n\
         ## Routes\n\n- /\nh2: color --ink\n",
    );

    let binding = |property: &str, token: &str| TokenBinding { property: property.to_string(), token: token.to_string() };
    assert_eq!(
        rules,
        vec![
            TokenRule {
                selector: "h1".to_string(),
                bindings: vec![binding("font-size", "--font-title"), binding("font-weight", "--weight-bold")],
            },
            TokenRule {
                selector: ".feature-state.state-pending".to_string(),
                bindings: vec![binding("color", "--pending-fg"), binding("background", "--pending-bg")],
            },
            TokenRule { selector: "a:hover".to_string(), bindings: vec![binding("color", "--link-hover")] },
        ]
    );
}

fn rgb(r: f64, g: f64, b: f64) -> TokenValue {
    TokenValue::Color(Rgba { r, g, b, a: 1.0 })
}

fn close(value: Option<TokenValue>, expected: TokenValue) {
    let value = value.expect("a value");
    match (&value, &expected) {
        (TokenValue::Color(a), TokenValue::Color(b)) => assert!(
            (a.r - b.r).abs() < 0.6 && (a.g - b.g).abs() < 0.6 && (a.b - b.b).abs() < 0.6 && (a.a - b.a).abs() < 0.01,
            "{value:?} is not {expected:?}"
        ),
        (TokenValue::Px(a), TokenValue::Px(b)) | (TokenValue::Number(a), TokenValue::Number(b)) => {
            assert!((a - b).abs() < 0.001, "{value:?} is not {expected:?}")
        }
        _ => panic!("{value:?} is not {expected:?}"),
    }
}

#[test]
fn tokens_css_custom_properties_of_root_and_theme_blocks_are_parsed_and_normalised() {
    let tokens = parse_tokens_css(
        "/* brand */\n:root {\n  --brand: #1a2b3c;\n  --short: #fff;\n  --ink: rgb(17, 17, 17);\n  --wash: rgba(0 0 0 / 50%);\n  --leaf: hsl(120, 100%, 25%);\n  --font-title: 2rem;\n  --pad: 12px;\n  --weight-bold: 700;\n  --radius-none: 0;\n  --alias: var(--brand);\n}\n[data-theme=\"dark\"] { --surface: #000000; }\n",
    );

    let root = 16.0;
    close(tokens.resolve("--brand", root), rgb(26.0, 43.0, 60.0));
    close(tokens.resolve("--short", root), rgb(255.0, 255.0, 255.0));
    close(tokens.resolve("--ink", root), rgb(17.0, 17.0, 17.0));
    close(tokens.resolve("--wash", root), TokenValue::Color(Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.5 }));
    close(tokens.resolve("--leaf", root), rgb(0.0, 127.5, 0.0));
    close(tokens.resolve("--font-title", root), TokenValue::Px(32.0));
    close(tokens.resolve("--font-title", 20.0), TokenValue::Px(40.0));
    close(tokens.resolve("--pad", root), TokenValue::Px(12.0));
    close(tokens.resolve("--weight-bold", root), TokenValue::Number(700.0));
    close(tokens.resolve("--radius-none", root), TokenValue::Number(0.0));
    close(tokens.resolve("--alias", root), rgb(26.0, 43.0, 60.0));
    close(tokens.resolve("--surface", root), rgb(0.0, 0.0, 0.0));
    assert!(tokens.resolve("--missing", root).is_none());
    close(Some(normalize_token_value("bold", root)), TokenValue::Number(700.0));
}

#[test]
fn tokens_json_flat_and_nested_values_become_custom_property_names_and_are_normalised() {
    let tokens = parse_tokens_json(
        r##"{
          "--brand": "#1a2b3c",
          "ink": "rgb(17, 17, 17)",
          "font": {"title": "2rem", "body": {"$value": "16px"}},
          "weight": {"bold": 700},
          "pending": {"fg": {"value": "hsl(0, 100%, 50%)"}}
        }"##,
    )
    .expect("tokens.json parses");

    let root = 16.0;
    close(tokens.resolve("--brand", root), rgb(26.0, 43.0, 60.0));
    close(tokens.resolve("--ink", root), rgb(17.0, 17.0, 17.0));
    close(tokens.resolve("--font-title", root), TokenValue::Px(32.0));
    close(tokens.resolve("--font-body", root), TokenValue::Px(16.0));
    close(tokens.resolve("--weight-bold", root), TokenValue::Number(700.0));
    close(tokens.resolve("--pending-fg", root), rgb(255.0, 0.0, 0.0));
    assert!(parse_tokens_json("not json").is_err());
}
