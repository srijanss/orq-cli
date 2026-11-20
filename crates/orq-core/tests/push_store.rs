//! What web push keeps in orq.db: the subscribed devices, the notification kinds switched off, the
//! projects muted, and when a (run, kind) was last notified.

use orq_core::storage::StorageRoots;
use orq_core::store::push::NewSubscription;
use orq_core::store::Store;

fn open(tag: &str) -> Store {
    let base = orq_test_support::process_temp_dir().join(format!("orq-push-store-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    Store::open(&roots.database()).unwrap()
}

fn device(endpoint: &str, label: &str) -> NewSubscription {
    NewSubscription { endpoint: endpoint.to_string(), p256dh: "key".to_string(), auth: "secret".to_string(), label: label.to_string() }
}

#[test]
fn subscriptions_are_stored_once_per_endpoint_and_removed_by_id() {
    let store = open("subscriptions");
    assert!(store.push_subscriptions().unwrap().is_empty());

    let first = store.add_push_subscription(&device("https://push.example/a", "Firefox on Linux"), 1_000).unwrap();
    let second = store.add_push_subscription(&device("https://push.example/b", "Safari on iPhone"), 2_000).unwrap();
    assert_ne!(first, second);
    // Subscribing again from the same endpoint is the same device: same id, label and keys refreshed, created time kept.
    let again = NewSubscription { p256dh: "newer".into(), ..device("https://push.example/a", "Firefox 2 on Linux") };
    assert_eq!(store.add_push_subscription(&again, 3_000).unwrap(), first);

    let listed = store.push_subscriptions().unwrap();
    assert_eq!(listed.len(), 2);
    let a = listed.iter().find(|s| s.id == first).unwrap();
    assert_eq!((a.endpoint.as_str(), a.p256dh.as_str(), a.label.as_str(), a.created_at), ("https://push.example/a", "newer", "Firefox 2 on Linux", 1_000));

    assert!(store.remove_push_subscription(first).unwrap());
    assert!(!store.remove_push_subscription(first).unwrap());
    assert_eq!(store.push_subscriptions().unwrap().len(), 1);
}

#[test]
fn kinds_and_projects_are_on_until_switched_off() {
    let store = open("switches");
    assert!(store.push_kind_choices().unwrap().is_empty() && store.push_muted_projects().unwrap().is_empty());

    store.set_push_kind("started", false).unwrap();
    store.set_push_kind("failed", false).unwrap();
    store.set_push_kind("started", true).unwrap();
    // Only what was chosen is kept, so a kind that is off unless switched on can be told from one never touched.
    assert_eq!(store.push_kind_choices().unwrap(), vec![("failed".to_string(), false), ("started".to_string(), true)]);

    store.set_push_project_muted("p-1", true).unwrap();
    store.set_push_project_muted("p-2", true).unwrap();
    store.set_push_project_muted("p-1", false).unwrap();
    assert_eq!(store.push_muted_projects().unwrap(), vec!["p-2".to_string()]);
}

#[test]
fn a_run_and_kind_is_claimed_once_per_window() {
    let store = open("claims");
    let window = 300;
    assert!(store.claim_push("run-1", "needs_input", 1_000, window).unwrap());
    assert!(!store.claim_push("run-1", "needs_input", 1_299, window).unwrap(), "inside the window");
    assert!(store.claim_push("run-1", "failed", 1_299, window).unwrap(), "another kind");
    assert!(store.claim_push("run-2", "needs_input", 1_299, window).unwrap(), "another run");
    assert!(store.claim_push("run-1", "needs_input", 1_300, window).unwrap(), "the window is over");
    assert!(!store.claim_push("run-1", "needs_input", 1_400, window).unwrap(), "and counts from the new claim");
}
