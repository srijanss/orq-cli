use orq_core::review_inbox::detail_with_links;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn roots() -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-risk-links-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn links_come_from_the_same_report_as_the_risks_and_only_safe_https_links_survive() {
    let roots = roots();
    let store = Store::open(&roots.database()).unwrap();
    let report = serde_json::json!({"status": "current", "risks": [
        {"severity": "high", "claim": "ok", "link": "https://example.invalid/a#L1"},
        {"severity": "high", "claim": "no link", "link": null},
        {"severity": "high", "claim": "plain http", "link": "http://example.invalid/a"},
        {"severity": "high", "claim": "script", "link": "javascript:alert(1)"},
        {"severity": "high", "claim": "spaces", "link": "https://example.invalid/a b"},
        {"severity": "high", "claim": "control", "link": "https://example.invalid/a\u{1b}[31m"},
        {"severity": "high", "claim": "absent"},
    ]});
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", HEAD, report.to_string().as_bytes()).unwrap();

    let (detail, links) = detail_with_links(&roots, "run-1").unwrap();
    assert_eq!(detail.risks.len(), 7);
    assert_eq!(links.len(), detail.risks.len(), "one link slot per risk");
    assert_eq!(links[0].as_deref(), Some("https://example.invalid/a#L1"));
    assert!(links[1..].iter().all(Option::is_none), "{links:?}");
}

#[test]
fn an_unreadable_report_gives_no_links_and_an_error() {
    let roots = roots();
    assert!(detail_with_links(&roots, "run-1").is_err());
    assert!(detail_with_links(&roots, "../x").is_err());
}
