use orq_core::review_inbox::inbox;
use orq_core::storage::StorageRoots;
use orq_core::store::Store;

const OURS: &str = "orq-cli-0a1b2c3d";

fn roots() -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-inbox-latest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn head(n: usize) -> String {
    format!("{n:040x}")
}

#[test]
fn many_runs_each_show_their_newest_report_once_in_first_seen_order() {
    let roots = roots();
    let store = Store::open(&roots.database()).unwrap();
    for run in 0..40 {
        for revision in 0..2 {
            let status = if revision == 1 { "current" } else { "incomplete" };
            let body = format!(r#"{{"status":"{status}","summary":{{}},"files":[],"tests":{{}},"risks":[],"rounds":[]}}"#);
            store.save_review(&roots.data, &format!("run-{run:04}"), OURS, &format!("F-{run}"), &head(revision), body.as_bytes()).unwrap();
        }
    }
    let rows = inbox(&roots, OURS, "orq-cli").unwrap();
    assert_eq!(rows.len(), 40);
    assert!(rows.iter().all(|r| r.status == "ready"), "an older report was shown");
    let runs: Vec<&str> = rows.iter().map(|r| r.run_id.as_str()).collect();
    let mut sorted = runs.clone();
    sorted.sort();
    assert_eq!(runs, sorted);
}
