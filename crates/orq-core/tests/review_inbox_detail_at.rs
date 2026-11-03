use orq_core::review_inbox::detail_at;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HEAD_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn roots() -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-detail-at-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

#[test]
fn the_detail_is_the_report_of_the_named_commit_not_just_the_latest() {
    let roots = roots();
    let store = Store::open(&roots.database()).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", HEAD_A, br#"{"status":"incomplete","risks":[{"severity":"high","claim":"old claim"}]}"#).unwrap();
    store.save_review(&roots.data, "run-1", "p-0a1b2c3d", "F", HEAD_B, br#"{"status":"current","risks":[{"severity":"low","claim":"new claim"}]}"#).unwrap();

    let old = detail_at(&roots, "run-1", HEAD_A).unwrap();
    assert!(old.risks[0].contains("old claim"), "{:?}", old.risks);
    let new = detail_at(&roots, "run-1", HEAD_B).unwrap();
    assert!(new.risks[0].contains("new claim"), "{:?}", new.risks);
    assert!(detail_at(&roots, "run-1", HEAD_C).is_err(), "no report exists for that commit");
    assert!(detail_at(&roots, "../x", HEAD_A).is_err());
}
