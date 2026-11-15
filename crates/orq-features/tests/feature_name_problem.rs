//! One rule for a feature name, shared by the web, the CLI and the popup: letters, digits, '-', '_' and '.',
//! not starting with '.'. A refused name says which character it refused and where, so a look-alike dash or
//! a hidden space can be found.

use orq_features::feature_name_problem;

const RULE: &str = "is not allowed in a feature name; use letters, digits, '-', '_' or '.'";

#[test]
fn a_plain_name_has_no_problem() {
    for name in ["a-b_c.d", "F1", "x.", "web-feature-name-says-which-character"] {
        assert_eq!(feature_name_problem(name), None, "{name}");
    }
}

#[test]
fn an_empty_name_or_a_leading_dot_keeps_its_own_message() {
    assert_eq!(feature_name_problem("").as_deref(), Some("a feature name is required"));
    assert_eq!(feature_name_problem(".x").as_deref(), Some("a feature name may not start with '.'"));
    assert_eq!(feature_name_problem("../x").as_deref(), Some("a feature name may not start with '.'"));
}

#[test]
fn a_visible_character_is_quoted_with_its_code_point_and_position() {
    assert_eq!(feature_name_problem("caf\u{e9}"), Some(format!("\"\u{e9}\" (U+00E9) at position 4 {RULE}")));
    assert_eq!(feature_name_problem("a/b"), Some(format!("\"/\" (U+002F) at position 2 {RULE}")));
    // The position counts characters, not bytes, and names the first bad one.
    assert_eq!(feature_name_problem("\u{e9}\u{e9}/"), Some(format!("\"\u{e9}\" (U+00E9) at position 1 {RULE}")));
    assert_eq!(feature_name_problem("ab\u{1f600}"), Some(format!("\"\u{1f600}\" (U+1F600) at position 3 {RULE}")));
}

#[test]
fn a_look_alike_dash_suggests_a_hyphen() {
    for dash in ['\u{2014}', '\u{2013}', '\u{2010}', '\u{2011}', '\u{2212}'] {
        let name = format!("a{dash}b");
        assert_eq!(feature_name_problem(&name), Some(format!("\"{dash}\" (U+{:04X}) at position 2 {RULE} (use '-')", dash as u32)), "{name}");
    }
}

#[test]
fn a_space_is_named_and_suggests_a_hyphen_between_words() {
    assert_eq!(feature_name_problem("my feature"), Some(format!("a space at position 3 {RULE} (use '-' between words)")));
    assert_eq!(feature_name_problem("a\u{a0}b"), Some(format!("U+00A0 at position 2 {RULE} (use '-' between words)")));
}

#[test]
fn an_invisible_or_control_character_is_shown_only_by_its_code_point() {
    assert_eq!(feature_name_problem("a\u{200b}b"), Some(format!("U+200B at position 2 {RULE}")));
    assert_eq!(feature_name_problem("a\u{feff}b"), Some(format!("U+FEFF at position 2 {RULE}")));
    assert_eq!(feature_name_problem("a\u{202e}b"), Some(format!("U+202E at position 2 {RULE}")));
    assert_eq!(feature_name_problem("a\u{1b}[31mb"), Some(format!("U+001B at position 2 {RULE}")));
    assert_eq!(feature_name_problem("a\tb"), Some(format!("U+0009 at position 2 {RULE} (use '-' between words)")));
    let shown = feature_name_problem("a\u{7}b").unwrap();
    assert!(!shown.chars().any(char::is_control), "{shown:?}");
}
