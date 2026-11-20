use orq_core::review_inbox::detail_with_links;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-risk-items-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn risk_items_carry_each_risks_fields_in_the_order_of_the_risk_lines_with_nulls_for_what_is_missing() {
    let roots = roots("fields");
    let store = Store::open(&roots.database()).unwrap();
    let report = serde_json::json!({"status": "current", "risks": [
        {"severity": "high", "file": "src/a.rs", "line": 7, "claim": "first", "category": "bugs", "reasoning": "because", "unresolved": true, "link": "https://example.invalid/a#L7"},
        {"severity": "low", "claim": "bare", "unresolved": false, "link": "javascript:alert(1)"},
        {"severity": "medium", "file": "src/b.rs", "claim": "no line", "link": "http://example.invalid/b"},
    ]});
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", HEAD, report.to_string().as_bytes()).unwrap();

    let (detail, _) = detail_with_links(&roots, "run-1").unwrap();
    let items = serde_json::Value::Array(detail.risk_items.iter().map(|item| item.to_json()).collect());
    assert_eq!(items.as_array().unwrap().len(), detail.risks.len());
    assert_eq!(items[0], serde_json::json!({"severity": "high", "file": "src/a.rs", "line": 7, "claim": "first", "category": "bugs", "reasoning": "because", "fixed": false, "link": "https://example.invalid/a#L7"}));
    assert_eq!(items[1], serde_json::json!({"severity": "low", "file": null, "line": null, "claim": "bare", "category": null, "reasoning": null, "fixed": true, "link": null}));
    assert_eq!(items[2]["file"], "src/b.rs");
    assert_eq!(items[2]["line"], serde_json::Value::Null);
    assert_eq!(items[2]["fixed"], false);
    assert_eq!(items[2]["link"], serde_json::Value::Null, "only https permalinks");
}
