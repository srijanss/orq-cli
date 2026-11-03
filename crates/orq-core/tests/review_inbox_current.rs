use orq_core::review_inbox::inbox;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const OURS: &str = "orq-cli-0a1b2c3d";
const SAME_NAME: &str = "orq-cli-deadbeef";
const BODY: &[u8] = br#"{"status":"current","summary":{},"files":[],"tests":{},"risks":[],"rounds":[]}"#;

fn roots() -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-current-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

#[test]
fn only_the_current_projects_id_marks_a_row_current_even_when_another_project_shares_its_name() {
    let roots = roots();
    let store = Store::open(&roots.database()).unwrap();
    let head = "a".repeat(40);
    store.save_review(&roots.data, "run-1", OURS, "COM-01", &head, BODY).unwrap();
    store.save_review(&roots.data, "run-2", SAME_NAME, "COM-02", &head, BODY).unwrap();
    let rows = inbox(&roots, OURS, "orq-cli").unwrap();
    let ours = rows.iter().find(|r| r.run_id == "run-1").unwrap();
    let twin = rows.iter().find(|r| r.run_id == "run-2").unwrap();
    assert!(ours.current && !twin.current);
    assert_eq!(ours.project, twin.project, "both display as the same name");
}
