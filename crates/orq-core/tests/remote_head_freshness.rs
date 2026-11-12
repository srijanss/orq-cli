//! Every report carries a freshness the server decides: `current` only when the provider head was
//! checked and equals the reviewed commit, `stale` on a mismatch, `unverified` when the check
//! failed or never ran. A provider link is allowed for `current` alone.

use orq_core::remote_inbox::{handle, RemoteConfig, Request};
use orq_core::review_lifecycle::{Observation, ObservationError, ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use serde_json::{json, Value};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const URL: &str = "https://example.test/o/r/-/merge_requests/7";

fn roots(tag: &str, lifecycle: Option<ReviewLifecycle>) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-head-freshness-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    let body = serde_json::to_vec(&json!({"status": "current", "summary": {"files_changed": 1, "added": 1, "deleted": 0}, "files": [], "tests": {"status": "passed", "commands": []}, "risks": [], "rounds": [], "gaps": []})).unwrap();
    store.save_review(&roots.data, "run-1", "ours", "feat", HEAD, &body).unwrap();
    if let Some(l) = lifecycle {
        store.save_lifecycle("run-1", &l).unwrap();
    }
    roots
}

fn get(roots: &StorageRoots, path: &str) -> Value {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle(roots, &config, "ours", "ours", &request);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

fn seen(head: &str) -> Result<Observation, ObservationError> {
    Ok(Observation { state: ProviderState::Open, head_sha: head.into() })
}

fn failed() -> Result<Observation, ObservationError> {
    Err(ObservationError::Unavailable("timeout".into()))
}

fn associated(url: &str) -> ReviewLifecycle {
    ReviewLifecycle::associate("gitlab", "7", url, HEAD)
}

/// The detail and the list row of the run must say the same thing.
fn both(roots: &StorageRoots) -> (Value, Value) {
    let detail = get(roots, "/api/reviews/run-1");
    let list = get(roots, "/api/reviews");
    (detail, list["reviews"][0].clone())
}

#[test]
fn a_head_checked_and_equal_to_the_reviewed_commit_is_current_and_allows_the_link() {
    let r = roots("current", Some(associated(URL).apply(seen(HEAD))));
    for body in {
        let (d, l) = both(&r);
        [d, l]
    } {
        assert_eq!(body["freshness"], "current", "{body}");
        assert_eq!(body["link_allowed"], true, "{body}");
        assert_eq!(body["link"], URL, "{body}");
    }
    let detail = get(&r, "/api/reviews/run-1");
    assert_eq!((&detail["reviewed_head_sha"], &detail["provider_head_sha"]), (&json!(HEAD), &json!(HEAD)), "{detail}");
    let at = detail["checked_at"].as_str().unwrap_or_else(|| panic!("{detail}"));
    assert!(at.len() == 20 && at.ends_with('Z') && at.starts_with("20"), "{at}");
}

#[test]
fn a_verified_mismatch_is_stale_names_both_commits_and_hides_the_link() {
    let r = roots("stale", Some(associated(URL).apply(seen(OTHER))));
    let (d, l) = both(&r);
    for body in [&d, &l] {
        assert_eq!((&body["freshness"], &body["link_allowed"], &body["link"]), (&json!("stale"), &json!(false), &Value::Null), "{body}");
    }
    assert_eq!((&d["reviewed_head_sha"], &d["provider_head_sha"]), (&json!(HEAD), &json!(OTHER)), "{d}");
    assert!(d["checked_at"].is_string(), "{d}");
}

#[test]
fn a_check_that_failed_or_never_ran_is_unverified_with_no_link() {
    let never = roots("never", None);
    let failed_first = roots("failed-first", Some(associated(URL).apply(failed())));
    let failed_after_current = roots("failed-after", Some(associated(URL).apply(seen(HEAD)).apply(failed())));
    for r in [&never, &failed_first, &failed_after_current] {
        let (d, l) = both(r);
        for body in [&d, &l] {
            assert_eq!((&body["freshness"], &body["link_allowed"], &body["link"]), (&json!("unverified"), &json!(false), &Value::Null), "{body}");
        }
        assert_eq!(d["provider_head_sha"], Value::Null, "an unverified head is not shown as a provider head: {d}");
        assert_eq!(d["reviewed_head_sha"], HEAD, "{d}");
    }
    assert_eq!(get(&never, "/api/reviews/run-1")["checked_at"], Value::Null);
}

#[test]
fn a_failed_check_after_a_mismatch_stays_stale() {
    let r = roots("stale-then-failed", Some(associated(URL).apply(seen(OTHER)).apply(failed())));
    let (d, l) = both(&r);
    assert_eq!((&d["freshness"], &l["freshness"], &d["link"]), (&json!("stale"), &json!("stale"), &Value::Null), "{d}");
}

#[test]
fn a_link_that_is_not_plain_https_is_never_allowed_even_for_a_current_review() {
    let r = roots("unsafe", Some(associated("http://example.test/x").apply(seen(HEAD))));
    let (d, _) = both(&r);
    assert_eq!((&d["freshness"], &d["link_allowed"], &d["link"]), (&json!("current"), &json!(false), &Value::Null), "{d}");
}

#[test]
fn an_association_that_was_never_checked_is_unverified_whatever_it_stores() {
    let r = roots("association-only", Some(associated(URL)));
    let (d, l) = both(&r);
    for body in [&d, &l] {
        assert_eq!((&body["freshness"], &body["link_allowed"], &body["link"]), (&json!("unverified"), &json!(false), &Value::Null), "{body}");
    }
    assert_eq!(d["provider_head_sha"], Value::Null, "{d}");
}

#[test]
fn a_lifecycle_that_cannot_be_read_is_unverified_and_allows_no_link() {
    let r = roots("corrupt", Some(associated(URL).apply(seen(HEAD))));
    damage_the_stored_state(&r);
    let (d, l) = both(&r);
    for body in [&d, &l] {
        assert_eq!((&body["freshness"], &body["link_allowed"], &body["link"]), (&json!("unverified"), &json!(false), &Value::Null), "{body}");
    }
}

fn damage_the_stored_state(roots: &StorageRoots) {
    let conn = rusqlite::Connection::open(roots.database()).unwrap();
    conn.execute("UPDATE review_lifecycles SET state = 'bogus'", []).unwrap();
}

#[test]
fn a_check_time_that_cannot_be_read_makes_an_observed_matching_review_unverified() {
    let r = roots("bad-time", Some(associated(URL).apply(seen(HEAD))));
    rusqlite::Connection::open(r.database()).unwrap().execute("UPDATE review_lifecycles SET last_checked = 'not a time'", []).unwrap();
    let (d, l) = both(&r);
    for body in [&d, &l] {
        assert_eq!((&body["freshness"], &body["link_allowed"], &body["link"]), (&json!("unverified"), &json!(false), &Value::Null), "{body}");
    }
    assert_eq!((&d["checked_at"], &d["provider_head_sha"]), (&Value::Null, &Value::Null), "{d}");
}
