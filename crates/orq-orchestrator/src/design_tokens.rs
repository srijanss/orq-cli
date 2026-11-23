//! A frontend run's design token check: a deterministic step, no agent, after every capture. The
//! package's tokens (`tokens.css` or `tokens.json`) are normalised (colours to RGB, sizes to px,
//! weights to numbers) and the page's measured styles are compared with them through the
//! `## Token rules` of DESIGN.md. The package is data: its values are parsed, never acted on.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::design_review::{DesignReview, DesignVerdict, IssueStatus, ReviewIssue, Severity};

const PACKAGE_DIR: &str = "package";
/// Infer mode's prepared documents, frozen by the design preparer step.
const PREPARED_DIR: &str = "prepared";
const DESIGN_DOC: &str = "DESIGN.md";
const TOKENS_CSS: &str = "tokens.css";
const TOKENS_JSON: &str = "tokens.json";
const MEASUREMENTS_FILE: &str = "measurements.json";
const TOKEN_CHECK_FILE: &str = "token-check.json";
/// The root font size a page that did not record its own is measured with.
const DEFAULT_ROOT_PX: f64 = 16.0;
/// How far a measured size may be from its token, in px.
const SIZE_TOLERANCE_PX: f64 = 1.0;
/// How far a measured colour may be from its token (CIE76 delta E), and its alpha.
const COLOR_TOLERANCE_DELTA_E: f64 = 2.0;
const ALPHA_TOLERANCE: f64 = 0.02;
/// How near a font size must be to a token to count as that token's, for the off-token warnings.
const OFF_TOKEN_SIZE_TOLERANCE_PX: f64 = 0.5;
/// The most off-token warnings one round lists.
const MAX_WARNINGS: usize = 500;
/// The largest `measurements.json` read.
const MAX_MEASUREMENTS: u64 = 64 * 1024 * 1024;

/// How the token check of a round ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCheckStatus {
    Pass,
    Fail,
    /// The check could not take place (no tokens file, no measurements): never a pass.
    NotAssessed,
}

/// How one rule's binding fared at one viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    Pass,
    Fail,
    /// The selector matched no element on any route at the viewport: one result for the rule.
    NoMatch,
}

/// One rule's binding at one viewport: `token-check.json`'s `rules`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuleResult {
    pub selector: String,
    pub viewport: String,
    pub property: Option<String>,
    pub token: Option<String>,
    /// The token's value, normalised.
    pub expected: Option<String>,
    /// The value measured, as the page computed it: the first that differs, else the first.
    pub measured: Option<String>,
    /// The route the measured value is from.
    pub route: Option<String>,
    /// How many elements the selector matched at the viewport, over every route.
    pub matched: usize,
    pub status: RuleStatus,
}

/// A colour or font size measured on a landmark that equals no token at all: a hard-coded value,
/// seen even where no rule binds it. Minor, never an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenWarning {
    pub viewport: String,
    pub route: String,
    /// The landmark's tag.
    pub selector: String,
    pub property: String,
    pub measured: String,
    pub severity: String,
}

/// `design/round-<n>/token-check.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokenCheck {
    pub round: u32,
    pub status: TokenCheckStatus,
    /// Why the check was not assessed.
    pub reason: Option<String>,
    pub rules: Vec<RuleResult>,
    pub warnings: Vec<TokenWarning>,
}

impl TokenCheck {
    /// Each violated rule as a design issue: source `tokens`, severity major, confidence high.
    pub fn issues(&self) -> Vec<ReviewIssue> {
        self.rules
            .iter()
            .filter(|rule| rule.status != RuleStatus::Pass)
            .map(|rule| {
                let property = rule.property.clone().unwrap_or_default();
                let token = rule.token.clone().unwrap_or_default();
                if rule.status == RuleStatus::NoMatch {
                    return ReviewIssue {
                        id: format!("tokens-{}-{}-missing", rule.viewport, slug(&rule.selector)),
                        section: rule.selector.clone(),
                        viewport: rule.viewport.clone(),
                        source: "tokens".to_string(),
                        expected: format!("an element matching {} (a token rule binds it)", rule.selector),
                        actual: format!("{} matches nothing on the page", rule.selector),
                        evidence: vec![format!("round-{}/{TOKEN_CHECK_FILE}", self.round)],
                        severity: Severity::Major,
                        status: IssueStatus::Open,
                        fix_direction: format!("render the element {} the design's token rules name", rule.selector),
                        selector: Some(rule.selector.clone()),
                        confidence: Some("high".to_string()),
                    };
                }
                ReviewIssue {
                    id: format!("tokens-{}-{}-{}", rule.viewport, slug(&rule.selector), property),
                    section: rule.selector.clone(),
                    viewport: rule.viewport.clone(),
                    source: "tokens".to_string(),
                    expected: format!(
                        "{property} {token} ({})",
                        rule.expected.as_deref().unwrap_or("unknown")
                    ),
                    actual: format!(
                        "{property} {} on {}",
                        rule.measured.as_deref().unwrap_or("unknown"),
                        rule.route.as_deref().unwrap_or("the page")
                    ),
                    evidence: vec![format!("round-{}/{TOKEN_CHECK_FILE}", self.round)],
                    severity: Severity::Major,
                    status: IssueStatus::Open,
                    fix_direction: format!("set {property} of {} to var({token})", rule.selector),
                    selector: Some(rule.selector.clone()),
                    confidence: Some("high".to_string()),
                }
            })
            .collect()
    }
}

/// Puts the token check's `issues` into the round's `review` as the check made them: a reviewer's
/// issue of the same id (dismissed, resolved or downgraded) is replaced, and a review that passed
/// with a violated rule requires changes. A blocked review stays blocked.
pub fn apply_token_issues(review: &mut DesignReview, issues: &[ReviewIssue]) {
    review.issues.retain(|issue| !issues.iter().any(|token| token.id == issue.id));
    review.issues.extend(issues.iter().cloned());
    if !issues.is_empty() && review.verdict == DesignVerdict::Pass {
        review.verdict = DesignVerdict::ChangesRequired;
    }
}

/// A selector as a plain id part: letters and digits, the rest `-`.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

/// Checks round `round`'s measurements in `design_dir` (the run's `design/`) against the frozen
/// package's tokens through its token rules, and writes `round-<round>/token-check.json`.
pub fn check_design_tokens(design_dir: &Path, round: u32) -> TokenCheck {
    let round_dir = design_dir.join(format!("round-{round}"));
    let check = match package_tokens(design_dir) {
        Ok(tokens) => compare_round(round, &tokens, &design_rules(design_dir), &read_measurements(&round_dir)),
        Err(reason) => TokenCheck {
            round,
            status: TokenCheckStatus::NotAssessed,
            reason: Some(reason),
            rules: Vec::new(),
            warnings: Vec::new(),
        },
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&check) {
        let _ = std::fs::create_dir_all(&round_dir)
            .and_then(|()| std::fs::write(round_dir.join(TOKEN_CHECK_FILE), bytes));
    }
    check
}

/// The frozen package's tokens: its `tokens.css`, else its `tokens.json`, else (infer mode) the
/// prepared folder's.
fn package_tokens(design_dir: &Path) -> Result<DesignTokens, String> {
    for folder in [PACKAGE_DIR, PREPARED_DIR] {
        let folder = design_dir.join(folder);
        if let Some(text) = read_text(&folder.join(TOKENS_CSS)) {
            return Ok(parse_tokens_css(&text));
        }
        if let Some(text) = read_text(&folder.join(TOKENS_JSON)) {
            return parse_tokens_json(&text);
        }
    }
    Err("the design package has no tokens file (tokens.css or tokens.json)".to_string())
}

/// The token rules of the frozen package's DESIGN.md and, in infer mode, of the prepared DESIGN.md
/// and token mapping (`tokens*.md`), each rule once.
fn design_rules(design_dir: &Path) -> Vec<TokenRule> {
    let prepared = design_dir.join(PREPARED_DIR);
    let mut documents = vec![design_dir.join(PACKAGE_DIR).join(DESIGN_DOC), prepared.join(DESIGN_DOC)];
    let mut mappings: Vec<_> = std::fs::read_dir(&prepared)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tokens") && name.ends_with(".md"))
        })
        .collect();
    mappings.sort();
    documents.extend(mappings);
    let mut rules: Vec<TokenRule> = Vec::new();
    for document in documents {
        for rule in parse_token_rules(&read_text(&document).unwrap_or_default()) {
            if !rules.contains(&rule) && rules.len() < MAX_RULES {
                rules.push(rule);
            }
        }
    }
    rules
}

/// The selectors the run's token rules name, each once, in rule order: what the capture measures
/// besides the landmarks. `design_dir` is the run's `design/`.
pub fn token_rule_selectors(design_dir: &Path) -> Vec<String> {
    let mut selectors: Vec<String> = Vec::new();
    for rule in design_rules(design_dir) {
        if !selectors.contains(&rule.selector) {
            selectors.push(rule.selector);
        }
    }
    selectors
}

fn compare_round(
    round: u32,
    tokens: &DesignTokens,
    rules: &[TokenRule],
    viewports: &[(String, Vec<(String, Value)>)],
) -> TokenCheck {
    let mut results = Vec::new();
    for (viewport, routes) in viewports {
        for rule in rules {
            let matched: Vec<(&str, &Value, f64)> = routes
                .iter()
                .flat_map(|(route, page)| {
                    let root = page["document"]["root_font_size"].as_f64().unwrap_or(DEFAULT_ROOT_PX);
                    matched_styles(page, &rule.selector)
                        .into_iter()
                        .map(move |styles| (route.as_str(), styles, root))
                })
                .collect();
            if matched.is_empty() {
                results.push(RuleResult {
                    selector: rule.selector.clone(),
                    viewport: viewport.clone(),
                    property: None,
                    token: None,
                    expected: None,
                    measured: None,
                    route: None,
                    matched: 0,
                    status: RuleStatus::NoMatch,
                });
                continue;
            }
            for binding in &rule.bindings {
                let root = matched.first().map_or(DEFAULT_ROOT_PX, |m| m.2);
                let Some(expected) = tokens.resolve(&binding.token, root) else {
                    continue;
                };
                let expected = for_property(&binding.property, expected);
                let compared: Vec<(&str, String, bool)> = matched
                    .iter()
                    .map(|(route, styles, root)| {
                        let expected = tokens
                            .resolve(&binding.token, *root)
                            .map_or(expected.clone(), |value| for_property(&binding.property, value));
                        let (measured, ok) = compare(&binding.property, &expected, styles);
                        (*route, measured, ok)
                    })
                    .collect();
                let shown = compared.iter().find(|c| !c.2).or(compared.first());
                results.push(RuleResult {
                    selector: rule.selector.clone(),
                    viewport: viewport.clone(),
                    property: Some(binding.property.clone()),
                    token: Some(binding.token.clone()),
                    expected: Some(show(&expected)),
                    measured: shown.map(|c| c.1.clone()),
                    route: shown.map(|c| c.0.to_string()),
                    matched: matched.len(),
                    status: if compared.iter().all(|c| c.2) { RuleStatus::Pass } else { RuleStatus::Fail },
                });
            }
        }
    }
    let status = if results.iter().any(|r| r.status != RuleStatus::Pass) {
        TokenCheckStatus::Fail
    } else {
        TokenCheckStatus::Pass
    };
    TokenCheck { round, status, reason: None, rules: results, warnings: off_token_values(tokens, viewports) }
}

/// The colours and font sizes measured on landmarks that equal no token, once per landmark tag,
/// property and value at each viewport. A transparent background is no colour.
fn off_token_values(tokens: &DesignTokens, viewports: &[(String, Vec<(String, Value)>)]) -> Vec<TokenWarning> {
    let mut warnings: Vec<TokenWarning> = Vec::new();
    for (viewport, routes) in viewports {
        for (route, page) in routes {
            let root = page["document"]["root_font_size"].as_f64().unwrap_or(DEFAULT_ROOT_PX);
            let values = tokens.all(root);
            for element in page["elements"].as_array().into_iter().flatten() {
                let tag = element["tag"].as_str().unwrap_or("").to_string();
                for property in ["color", "background-color", "font-size"] {
                    let Some(text) = element["styles"][property].as_str().map(str::trim) else {
                        continue;
                    };
                    let known = match normalize_token_value(text, DEFAULT_ROOT_PX) {
                        TokenValue::Color(color) if property != "font-size" => {
                            color.a == 0.0
                                || values.iter().any(|v| matches!(v, TokenValue::Color(t) if color_close(t, &color)))
                        }
                        TokenValue::Px(px) if property == "font-size" => values.iter().any(|v| {
                            matches!(v, TokenValue::Px(t) if (t - px).abs() <= OFF_TOKEN_SIZE_TOLERANCE_PX)
                        }),
                        _ => true,
                    };
                    let seen = warnings.iter().any(|w| {
                        w.viewport == *viewport && w.selector == tag && w.property == property && w.measured == text
                    });
                    if !known && !seen && warnings.len() < MAX_WARNINGS {
                        warnings.push(TokenWarning {
                            viewport: viewport.clone(),
                            route: route.clone(),
                            selector: tag.clone(),
                            property: property.to_string(),
                            measured: text.to_string(),
                            severity: "minor".to_string(),
                        });
                    }
                }
            }
        }
    }
    warnings
}

fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Each viewport's measured routes, from `round-<n>/<viewport>/measurements.json`, by viewport name.
fn read_measurements(round_dir: &Path) -> Vec<(String, Vec<(String, Value)>)> {
    let mut viewports = Vec::new();
    let Ok(entries) = std::fs::read_dir(round_dir) else {
        return viewports;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let file = entry.path().join(MEASUREMENTS_FILE);
        let readable = std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file() && m.len() <= MAX_MEASUREMENTS);
        let Some(measured) = readable
            .then(|| std::fs::read(&file).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let routes = measured["routes"]
            .as_object()
            .map(|routes| routes.iter().map(|(route, page)| (route.clone(), page.clone())).collect())
            .unwrap_or_default();
        viewports.push((name, routes));
    }
    viewports.sort_by(|a, b| a.0.cmp(&b.0));
    viewports
}

/// The computed styles of each element `selector` matched on `page`: the capture's record of the
/// rule's selector, else, for a bare tag name, the landmarks of that tag.
fn matched_styles<'a>(page: &'a Value, selector: &str) -> Vec<&'a Value> {
    if let Some(found) = page["selectors"][selector].as_array() {
        return found.iter().map(|element| &element["styles"]).collect();
    }
    let tag = selector.to_ascii_lowercase();
    if !tag.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Vec::new();
    }
    page["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|element| element["tag"].as_str() == Some(tag.as_str()))
        .map(|element| &element["styles"])
        .collect()
}

/// The measured value of `property` and whether it equals `expected` within the tolerances.
fn compare(property: &str, expected: &TokenValue, styles: &Value) -> (String, bool) {
    let style = |name: &str| styles[name].as_str().unwrap_or("").trim().to_string();
    let parts: Vec<String> = match property {
        "padding" => ["padding-top", "padding-right", "padding-bottom", "padding-left"]
            .iter()
            .map(|side| style(side))
            .collect(),
        "border-radius" | "gap" => style(property).split_whitespace().map(str::to_string).collect(),
        "background" | "background-color" => vec![style("background-color")],
        "border-color" => vec![style("border-top-color")],
        _ => vec![style(property)],
    };
    let shown = if property == "padding" { parts.join(" ") } else { style(css_name(property)) };
    if parts.is_empty() || parts.iter().any(String::is_empty) {
        return (shown, false);
    }
    let font_px = || match normalize_token_value(&style("font-size"), DEFAULT_ROOT_PX) {
        TokenValue::Px(px) => Some(px),
        _ => None,
    };
    let ok = parts.iter().all(|part| {
        let measured = match (property, part.as_str()) {
            ("gap", "normal") => TokenValue::Px(0.0),
            _ => normalize_token_value(part, DEFAULT_ROOT_PX),
        };
        equal(property, expected, &measured, font_px)
    });
    (shown, ok)
}

/// A token's value as `property` reads it: a plain number on a size (`--radius-none: 0`) is px.
fn for_property(property: &str, value: TokenValue) -> TokenValue {
    match (property, value) {
        ("font-weight" | "line-height", value) => value,
        (_, TokenValue::Number(n)) => TokenValue::Px(n),
        (_, value) => value,
    }
}

fn css_name(property: &str) -> &str {
    match property {
        "background" => "background-color",
        "border-color" => "border-top-color",
        other => other,
    }
}

fn equal(property: &str, expected: &TokenValue, measured: &TokenValue, font_px: impl Fn() -> Option<f64>) -> bool {
    match (expected, measured) {
        (TokenValue::Color(a), TokenValue::Color(b)) => color_close(a, b),
        (TokenValue::Number(weight), TokenValue::Number(m)) if property == "font-weight" => weight == m,
        // A unitless line height is a multiple of the font size.
        (TokenValue::Number(factor), TokenValue::Px(m)) if property == "line-height" => {
            font_px().is_some_and(|font| (factor * font - m).abs() <= SIZE_TOLERANCE_PX)
        }
        (TokenValue::Px(e) | TokenValue::Number(e), TokenValue::Px(m)) if property != "font-weight" => {
            (e - m).abs() <= SIZE_TOLERANCE_PX
        }
        (TokenValue::Other(e), TokenValue::Other(m)) => e.eq_ignore_ascii_case(m),
        _ => false,
    }
}

fn color_close(a: &Rgba, b: &Rgba) -> bool {
    (a.a - b.a).abs() <= ALPHA_TOLERANCE && delta_e(a, b) <= COLOR_TOLERANCE_DELTA_E
}

/// CIE76 delta E between two colours, in CIELAB (D65).
fn delta_e(a: &Rgba, b: &Rgba) -> f64 {
    let (l1, a1, b1) = lab(a);
    let (l2, a2, b2) = lab(b);
    ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
}

fn lab(color: &Rgba) -> (f64, f64, f64) {
    let linear = |c: f64| {
        let c = c / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(color.r), linear(color.g), linear(color.b));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// A normalised value as `token-check.json` shows it: `rgb(…)`, `32px`, `700`.
fn show(value: &TokenValue) -> String {
    let number = |n: f64| {
        let rounded = (n * 100.0).round() / 100.0;
        if rounded.fract() == 0.0 {
            format!("{}", rounded as i64)
        } else {
            format!("{rounded}")
        }
    };
    match value {
        TokenValue::Color(c) if c.a >= 1.0 => format!("rgb({}, {}, {})", number(c.r), number(c.g), number(c.b)),
        TokenValue::Color(c) => format!("rgba({}, {}, {}, {})", number(c.r), number(c.g), number(c.b), number(c.a)),
        TokenValue::Px(px) => format!("{}px", number(*px)),
        TokenValue::Number(n) => number(*n),
        TokenValue::Other(text) => text.clone(),
    }
}

/// A colour as RGB channels (0 to 255) and alpha (0 to 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// A token's or a measured style's value, normalised.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenValue {
    Color(Rgba),
    /// A size in CSS pixels.
    Px(f64),
    /// A plain number: a weight, a unitless line height, a unitless 0.
    Number(f64),
    /// Anything else, as written.
    Other(String),
}

/// The package's tokens, by custom property name (`--name`), values as written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesignTokens {
    values: BTreeMap<String, String>,
}

/// How many `var()` references one value may go through.
const MAX_ALIAS_DEPTH: usize = 8;

impl DesignTokens {
    /// The token `name`, normalised with `root_px` as the page's root font size; `None` when the
    /// package has no such token or its references go round in a circle.
    pub fn resolve(&self, name: &str, root_px: f64) -> Option<TokenValue> {
        self.resolve_at(name, root_px, 0)
    }

    /// Every token's value, normalised with `root_px`.
    fn all(&self, root_px: f64) -> Vec<TokenValue> {
        self.values.keys().filter_map(|name| self.resolve(name, root_px)).collect()
    }

    fn resolve_at(&self, name: &str, root_px: f64, depth: usize) -> Option<TokenValue> {
        if depth > MAX_ALIAS_DEPTH {
            return None;
        }
        let value = self.values.get(name)?;
        match var_reference(value) {
            Some((alias, fallback)) => self
                .resolve_at(&alias, root_px, depth + 1)
                .or_else(|| fallback.map(|f| normalize_token_value(&f, root_px))),
            None => Some(normalize_token_value(value, root_px)),
        }
    }
}

/// The custom properties of a `tokens.css`: every `--name: value` in `:root` and theme blocks. A
/// name defined twice keeps its first value, so `:root` stays the reference.
pub fn parse_tokens_css(text: &str) -> DesignTokens {
    let mut tokens = DesignTokens::default();
    for declaration in strip_comments(text).split(['{', '}', ';']) {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.len() > 2 && name.starts_with("--") && !value.is_empty() {
            tokens
                .values
                .entry(name.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    tokens
}

/// One `## Token rules` line: a selector and the tokens its properties must equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRule {
    pub selector: String,
    pub bindings: Vec<TokenBinding>,
}

/// `font-weight --weight-bold`: a property and the token (`--name`) it must equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBinding {
    pub property: String,
    pub token: String,
}

/// The properties a rule may bind, as written in a rule.
pub const RULE_PROPERTIES: [&str; 10] = [
    "font-size",
    "font-weight",
    "line-height",
    "color",
    "background",
    "background-color",
    "border-color",
    "border-radius",
    "padding",
    "gap",
];

/// The most rules one document is read for, and the longest selector.
const MAX_RULES: usize = 200;
const MAX_SELECTOR_LEN: usize = 200;

/// The rules of the `## Token rules` section of `doc`, one per line:
/// `h1: font-size --font-title; font-weight --weight-bold`. A line may be a list item and wrapped in
/// backticks. A binding that is not a known property and a `--token` is left out, and a line left
/// with none is not a rule. Only the rules are read: the rest of the text is never acted on.
pub fn parse_token_rules(doc: &str) -> Vec<TokenRule> {
    let mut rules = Vec::new();
    let mut in_rules = false;
    for line in doc.lines() {
        let line = line.trim();
        // A Markdown heading is `#`s then a space (or nothing); `#hero: color --ink` is a rule.
        let after_marks = line.trim_start_matches('#');
        if line.starts_with('#') && (after_marks.is_empty() || after_marks.starts_with(char::is_whitespace)) {
            in_rules = after_marks.trim().eq_ignore_ascii_case("token rules");
            continue;
        }
        if !in_rules || rules.len() >= MAX_RULES {
            continue;
        }
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("+ "))
            .unwrap_or(line)
            .trim()
            .trim_matches('`')
            .trim();
        if let Some(rule) = parse_rule(line) {
            rules.push(rule);
        }
    }
    rules
}

fn parse_rule(line: &str) -> Option<TokenRule> {
    // The selector ends at the first colon followed by a space, so `a:hover: color --x` keeps its
    // pseudo-class.
    let split = line
        .char_indices()
        .find(|&(i, c)| c == ':' && line[i + 1..].starts_with(char::is_whitespace))?
        .0;
    let selector = line[..split].trim();
    if selector.is_empty() || selector.len() > MAX_SELECTOR_LEN || selector.chars().any(char::is_control) {
        return None;
    }
    let bindings: Vec<TokenBinding> = line[split + 1..]
        .split(';')
        .filter_map(|binding| {
            let mut words = binding.split_whitespace();
            let (property, token) = (words.next()?, words.next()?);
            let known = RULE_PROPERTIES.contains(&property.to_ascii_lowercase().as_str());
            let token_name = token.len() > 2
                && token.starts_with("--")
                && token[2..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            (known && token_name && words.next().is_none()).then(|| TokenBinding {
                property: property.to_ascii_lowercase(),
                token: token.to_string(),
            })
        })
        .collect();
    (!bindings.is_empty()).then(|| TokenRule { selector: selector.to_string(), bindings })
}

/// The most entries a `tokens.json` is read for, and how deep its groups may nest.
const MAX_JSON_TOKENS: usize = 10_000;
const MAX_JSON_DEPTH: usize = 16;

/// The values of a `tokens.json`: a flat key is the token's name (`--` added when missing), a nested
/// group's keys are joined with `-` (`{"font": {"title": "2rem"}}` is `--font-title`), and a leaf may
/// be written as `{"$value": …}` or `{"value": …}`.
pub fn parse_tokens_json(text: &str) -> Result<DesignTokens, String> {
    let root: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("tokens.json is not JSON ({e})"))?;
    let serde_json::Value::Object(map) = root else {
        return Err("tokens.json is not a JSON object".to_string());
    };
    let mut tokens = DesignTokens::default();
    collect_json(&map, "", 0, &mut tokens);
    Ok(tokens)
}

fn collect_json(
    map: &serde_json::Map<String, serde_json::Value>,
    prefix: &str,
    depth: usize,
    tokens: &mut DesignTokens,
) {
    if depth > MAX_JSON_DEPTH {
        return;
    }
    for (key, value) in map {
        if tokens.values.len() >= MAX_JSON_TOKENS {
            return;
        }
        if key.starts_with('$') {
            continue;
        }
        let key = key.trim_start_matches('-');
        let name = if prefix.is_empty() { key.to_string() } else { format!("{prefix}-{key}") };
        let leaf = match value {
            serde_json::Value::Object(inner) => match inner.get("$value").or_else(|| inner.get("value")) {
                Some(leaf) if !leaf.is_object() => leaf,
                _ => {
                    collect_json(inner, &name, depth + 1, tokens);
                    continue;
                }
            },
            leaf => leaf,
        };
        let text = match leaf {
            serde_json::Value::String(text) => text.trim().to_string(),
            serde_json::Value::Number(number) => number.to_string(),
            _ => continue,
        };
        if !name.is_empty() && !text.is_empty() {
            tokens.values.entry(format!("--{name}")).or_insert(text);
        }
    }
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = match rest[start + 2..].find("*/") {
            Some(end) => &rest[start + 2 + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// `var(--name)` or `var(--name, fallback)`: the name and the fallback.
fn var_reference(value: &str) -> Option<(String, Option<String>)> {
    let inner = value.trim().strip_prefix("var(")?.strip_suffix(')')?;
    let (name, fallback) = match inner.split_once(',') {
        Some((name, fallback)) => (name.trim(), Some(fallback.trim().to_string())),
        None => (inner.trim(), None),
    };
    name.starts_with("--").then(|| (name.to_string(), fallback))
}

/// A CSS value normalised: colours (hex, `rgb()`, `hsl()`, a few names) to RGB, lengths (`px`,
/// `rem`, `em` with `root_px`, `pt`) to px, weight keywords and plain numbers to numbers.
pub fn normalize_token_value(value: &str, root_px: f64) -> TokenValue {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if let Some(color) = parse_color(&lower) {
        return TokenValue::Color(color);
    }
    match lower.as_str() {
        "normal" => return TokenValue::Number(400.0),
        "bold" => return TokenValue::Number(700.0),
        _ => {}
    }
    if let Ok(number) = lower.parse::<f64>() {
        return TokenValue::Number(number);
    }
    for (unit, scale) in [("px", 1.0), ("rem", root_px), ("em", root_px), ("pt", 4.0 / 3.0)] {
        if let Some(number) = lower.strip_suffix(unit).and_then(|n| n.trim().parse::<f64>().ok()) {
            return TokenValue::Px(number * scale);
        }
    }
    TokenValue::Other(value.to_string())
}

fn parse_color(value: &str) -> Option<Rgba> {
    let opaque = |r: f64, g: f64, b: f64| Rgba { r, g, b, a: 1.0 };
    match value {
        "transparent" => return Some(Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
        "black" => return Some(opaque(0.0, 0.0, 0.0)),
        "white" => return Some(opaque(255.0, 255.0, 255.0)),
        "red" => return Some(opaque(255.0, 0.0, 0.0)),
        "green" => return Some(opaque(0.0, 128.0, 0.0)),
        "blue" => return Some(opaque(0.0, 0.0, 255.0)),
        "gray" | "grey" => return Some(opaque(128.0, 128.0, 128.0)),
        _ => {}
    }
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex);
    }
    let (function, args) = value.split_once('(')?;
    let args = args.strip_suffix(')')?;
    let (channels, alpha) = match args.split_once('/') {
        Some((channels, alpha)) => (channels, Some(alpha.trim())),
        None => (args, None),
    };
    let mut parts: Vec<&str> = channels
        .split([',', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let alpha = match (alpha, parts.len()) {
        (Some(alpha), 3) => alpha_value(alpha)?,
        (None, 4) => alpha_value(parts.pop()?)?,
        (None, 3) => 1.0,
        _ => return None,
    };
    match function.trim() {
        "rgb" | "rgba" => {
            let channel = |p: &str| -> Option<f64> {
                match p.strip_suffix('%') {
                    Some(percent) => percent.parse::<f64>().ok().map(|v| v * 255.0 / 100.0),
                    None => p.parse::<f64>().ok(),
                }
            };
            Some(Rgba {
                r: channel(parts[0])?.clamp(0.0, 255.0),
                g: channel(parts[1])?.clamp(0.0, 255.0),
                b: channel(parts[2])?.clamp(0.0, 255.0),
                a: alpha,
            })
        }
        "hsl" | "hsla" => {
            let hue = parts[0].strip_suffix("deg").unwrap_or(parts[0]).parse::<f64>().ok()?;
            let percent = |p: &str| -> Option<f64> {
                p.strip_suffix('%').unwrap_or(p).parse::<f64>().ok().map(|v| (v / 100.0).clamp(0.0, 1.0))
            };
            let (r, g, b) = hsl_to_rgb(hue, percent(parts[1])?, percent(parts[2])?);
            Some(Rgba { r, g, b, a: alpha })
        }
        _ => None,
    }
}

fn alpha_value(text: &str) -> Option<f64> {
    let value = match text.strip_suffix('%') {
        Some(percent) => percent.parse::<f64>().ok()? / 100.0,
        None => text.parse::<f64>().ok()?,
    };
    Some(value.clamp(0.0, 1.0))
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|d| f64::from(d * 17));
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(f64::from);
    let (r, g, b, a) = match hex.len() {
        3 => (digit(0)?, digit(1)?, digit(2)?, 255.0),
        4 => (digit(0)?, digit(1)?, digit(2)?, digit(3)?),
        6 => (pair(0)?, pair(2)?, pair(4)?, 255.0),
        8 => (pair(0)?, pair(2)?, pair(4)?, pair(6)?),
        _ => return None,
    };
    Some(Rgba { r, g, b, a: a / 255.0 })
}

fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> (f64, f64, f64) {
    let hue = hue.rem_euclid(360.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let x = chroma * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = lightness - chroma / 2.0;
    let (r, g, b) = match hue {
        h if h < 60.0 => (chroma, x, 0.0),
        h if h < 120.0 => (x, chroma, 0.0),
        h if h < 180.0 => (0.0, chroma, x),
        h if h < 240.0 => (0.0, x, chroma),
        h if h < 300.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    ((r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0)
}
