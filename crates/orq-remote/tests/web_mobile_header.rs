//! On a phone the page header keeps the refresh icon at the top right beside the title, and the
//! connection and last-updated lines share the row below, so no space is lost to a wrapped row.

fn css() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap().to_lowercase()
}

fn rule<'a>(block: &'a str, selector: &str) -> &'a str {
    let start = block.find(selector).unwrap_or_else(|| panic!("no rule for {selector}"));
    &block[start..start + block[start..].find('}').unwrap()]
}

#[test]
fn the_refresh_icon_stays_top_right_and_the_status_lines_share_one_row_on_a_phone() {
    let css = css();
    let start = css.rfind("@media (max-width: 700px) {").unwrap();
    let phone = &css[start..start + css[start..].find("\n}\n").unwrap()];
    let header = rule(phone, ".page-header {");
    assert!(header.contains("display: grid") && header.contains("grid-template-columns: 1fr auto"), "{header}");
    assert!(rule(phone, ".header-status {").contains("display: contents"), "the status children join the header grid");
    let refresh = rule(phone, ".page-header [data-refresh] {");
    assert!(refresh.contains("grid-column: 2") && refresh.contains("grid-row: 1"), "{refresh}");
    let connection = rule(phone, ".page-header .connection {");
    assert!(connection.contains("grid-column: 1") && connection.contains("grid-row: 2"), "{connection}");
    let updated = rule(phone, ".page-header .last-updated {");
    // It spans the row and sits at its right end, so its width never widens the refresh column
    // and a long title cannot push the page wider than the phone.
    assert!(updated.contains("grid-column: 1 / -1") && updated.contains("grid-row: 2") && updated.contains("justify-self: end"), "{updated}");
}
