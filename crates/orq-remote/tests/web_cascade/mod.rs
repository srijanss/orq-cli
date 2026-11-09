//! The stylesheet's cascade, resolved for a given screen width. There is no layout engine in the
//! test setup, so this picks the rule that wins (media condition, importance, specificity, order).
#![allow(dead_code)]

use std::path::Path;

pub fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap().to_lowercase()
}

pub fn css() -> String {
    read("web/app.css")
}

pub struct Rule {
    pub media: Option<String>,
    pub selectors: Vec<String>,
    pub declarations: Vec<(String, String)>,
}

/// Every rule in source order with the @media condition it sits in (None at the top level).
pub fn parse(css: &str, media: Option<&str>) -> Vec<Rule> {
    if css.contains("/*") {
        return parse(&strip_comments(css), media);
    }
    let mut rules = Vec::new();
    let mut at = 0;
    while let Some(open) = css[at..].find('{').map(|i| i + at) {
        let head = css[at..open].trim();
        let mut depth = 1;
        let mut close = open + 1;
        for (i, c) in css[open + 1..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                close = open + 1 + i;
                break;
            }
        }
        let inner = &css[open + 1..close];
        if let Some(condition) = head.strip_prefix("@media") {
            rules.extend(parse(inner, Some(condition.trim())));
        } else {
            rules.push(Rule {
                media: media.map(str::to_string),
                selectors: head.split(',').map(|s| s.trim().to_string()).collect(),
                declarations: inner.split(';').filter_map(|d| d.split_once(':')).map(|(p, v)| (p.trim().to_string(), v.trim().to_string())).collect(),
            });
        }
        at = close + 1;
    }
    rules
}

/// Whether a media condition holds at `width` pixels. Colour-scheme and reduced-motion conditions hold no widths and
/// are skipped; any other condition this does not understand fails the test rather than hiding.
pub fn applies(media: &Option<String>, width: f64) -> bool {
    let Some(condition) = media else { return true };
    if condition.contains("prefers-color-scheme") || condition.contains("prefers-reduced-motion") {
        return false;
    }
    condition.split(" and ").all(|part| {
        let part = part.trim().trim_start_matches('(').trim_end_matches(')');
        let (feature, value) = part.split_once(':').unwrap_or_else(|| panic!("media condition {condition:?} is not understood"));
        let px: f64 = value.trim().strip_suffix("px").and_then(|v| v.parse().ok()).unwrap_or_else(|| panic!("media condition {condition:?} is not understood"));
        match feature.trim() {
            "max-width" => width <= px,
            "min-width" => width >= px,
            _ => panic!("media condition {condition:?} is not understood"),
        }
    })
}

/// The last compound selector (the part that names the element itself).
pub fn subject(selector: &str) -> &str {
    selector.rsplit([' ', '>', '+', '~']).next().unwrap_or(selector)
}

/// Whether a rule's selector can match an element that `target` (a class or a tag) names. A
/// selector that adds ancestors or other classes is assumed to match, which is the safe direction.
pub fn can_match(selector: &str, target: &str) -> bool {
    let compound = subject(selector);
    if let Some(class) = target.strip_prefix('.') {
        compound.split('.').skip(1).any(|c| c.split([':', '[', '#']).next() == Some(class))
    } else {
        compound.split(['.', ':', '[', '#']).next() == Some(target)
    }
}

/// (ids, classes/attributes/pseudo-classes, tags) as one comparable number.
pub fn specificity(selector: &str) -> usize {
    let mut score = 0;
    for compound in selector.split([' ', '>', '+', '~']).filter(|c| !c.is_empty()) {
        score += compound.matches('#').count() * 10_000 + (compound.matches('.').count() + compound.matches('[').count() + compound.matches(':').count()) * 100;
        if compound.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            score += 1;
        }
    }
    score
}

/// What an element named by `target` ends up with for `property` on a screen `width` pixels wide:
/// every rule whose media condition holds and whose selector can match, ordered by importance,
/// specificity and source order, the last one winning.
pub fn effective_at(css: &str, target: &str, property: &str, width: f64) -> String {
    let mut best: Option<((bool, usize, usize), String)> = None;
    let mut order = 0;
    for rule in parse(css, None).iter().filter(|r| applies(&r.media, width)) {
        for selector in rule.selectors.iter().filter(|s| can_match(s, target)) {
            for (_, v) in rule.declarations.iter().filter(|(p, _)| p == property) {
                order += 1;
                let key = (v.contains("!important"), specificity(selector), order);
                if best.as_ref().map_or(true, |(k, _)| key > *k) {
                    best = Some((key, v.replace("!important", "").trim().to_string()));
                }
            }
        }
    }
    best.unwrap_or_else(|| panic!("no {property} for {target} at {width}px")).1
}

pub const WIDTHS: [f64; 3] = [320.0, 360.0, 430.0];

/// The value on every phone width, which must be one value.
pub fn effective(css: &str, target: &str, property: &str) -> String {
    let values: Vec<String> = WIDTHS.iter().map(|w| effective_at(css, target, property, *w)).collect();
    assert!(values.iter().all(|v| *v == values[0]), "{target} {property} differs between phone widths: {values:?}");
    values[0].clone()
}

/// The stylesheet without its comments, so a comment never becomes part of a selector.
pub fn strip_comments(css: &str) -> String {
    let mut out = String::new();
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..].split_once("*/").map_or("", |(_, after)| after);
    }
    out.push_str(rest);
    out
}
