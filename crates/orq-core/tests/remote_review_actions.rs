//! `POST /api/reviews/{run}/resolve`, `.../dismiss` and `.../reopen` (and the same under
//! `/api/projects/{id}/reviews/{run}/`) let a person end a review that has no PR/MR. They answer only
//! while remote actions are on, take `{"head": <sha>}` and nothing else, refuse a head that is no longer
//! the latest report's, record a run event through the provider, and leave an audit line each time.

use std::sync::Mutex;

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectsProvider, RemoteConfig, Request, ReviewScope};
use orq_core::review_inbox::{self, ReviewAction};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use serde_json::{json, Value};

const OLD: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OURS: &str = "ours-0a1b2c3d";
const OTHER: &str = "other-11223344";

#[derive(Default)]
struct Fake {
    acted: Mutex<Vec<(String, String, ReviewAction, String)>>,
    /// Why recording a run event fails, when it does.
    acted_fails: Option<String>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }
    fn review_scope(&self, id: &str) -> Result<Option<ReviewScope>, String> {
        Ok(match id {
            "ours" => Some(ReviewScope { review_key: OURS.into(), name: "ours".into() }),
            "other" => Some(ReviewScope { review_key: OTHER.into(), name: "other".into() }),
            _ => None,
        })
    }
    fn review_acted(&self, project: &str, run: &str, action: ReviewAction, head: &str) -> Result<(), String> {
        self.acted.lock().unwrap().push((project.into(), run.into(), action, head.into()));
        self.acted_fails.clone().map_or(Ok(()), Err)
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-review-actions-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    let body = br#"{"status":"current","risks":[]}"#;
    store.save_review(&roots.data, "run-1", OURS, "feat-1", OLD, body).unwrap();
    store.save_review(&roots.data, "run-1", OURS, "feat-1", HEAD, body).unwrap();
    store.save_review(&roots.data, "run-2", OURS, "feat-2", HEAD, body).unwrap();
    roots
}

fn actions_on(roots: &StorageRoots) {
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, path: &str, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, OURS, "ours", &request, body.as_bytes(), fake);
    (r.status, r.body)
}

fn row(roots: &StorageRoots, fake: &Fake, run: &str) -> Value {
    let (_, body) = send(roots, fake, "GET", "/api/reviews", "");
    body["reviews"].as_array().unwrap().iter().find(|r| r["run_id"] == run).cloned().unwrap()
}

fn audit(roots: &StorageRoots) -> Vec<(String, String)> {
    remote_audit::read(roots, 20).unwrap().iter().rev().map(|l| (l["action"].as_str().unwrap().to_string(), l["outcome"].as_str().unwrap().to_string())).collect()
}

const AT_HEAD: &str = r#"{"head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;

#[test]
fn a_review_is_resolved_dismissed_and_reopened_from_the_web_only_while_actions_are_on() {
    let roots = roots("act");
    let fake = Fake::default();
    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/run-1/resolve", AT_HEAD).0, 404, "off by default");
    assert_eq!(row(&roots, &fake, "run-1")["status"], "ready");
    actions_on(&roots);

    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/run-1/resolve", AT_HEAD), (200, json!({"run": "run-1", "status": "resolved", "head": HEAD})));
    assert_eq!(row(&roots, &fake, "run-1")["status"], "resolved");
    assert_eq!(send(&roots, &fake, "POST", "/api/projects/ours/reviews/run-1/reopen", AT_HEAD).1["status"], "ready");
    assert_eq!(send(&roots, &fake, "POST", "/api/projects/ours/reviews/run-2/dismiss", "").1["status"], "closed", "the head is optional");
    assert_eq!(row(&roots, &fake, "run-2")["status"], "closed");

    let acted = fake.acted.lock().unwrap().clone();
    let one = |run: &str, action| (OURS.to_string(), run.to_string(), action, HEAD.to_string());
    assert_eq!(acted, [one("run-1", ReviewAction::Resolve), one("run-1", ReviewAction::Reopen), one("run-2", ReviewAction::Dismiss)]);
    let line = |action: &str, outcome: &str| (action.to_string(), outcome.to_string());
    assert_eq!(audit(&roots), [line("review.resolve", "refused"), line("review.resolve", "allowed"), line("review.reopen", "allowed"), line("review.dismiss", "allowed")]);
}

#[test]
fn an_older_head_an_unknown_run_another_project_a_bad_body_or_a_merged_review_is_refused() {
    let roots = roots("refuse");
    let fake = Fake::default();
    actions_on(&roots);

    let stale = send(&roots, &fake, "POST", "/api/reviews/run-1/resolve", &format!(r#"{{"head": "{OLD}"}}"#));
    assert_eq!(stale.0, 409, "{stale:?}");
    assert_eq!(stale.1["latest"], HEAD, "the page can reload the newer report");
    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/run-9/resolve", AT_HEAD).0, 404);
    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/..%2Fx/resolve", AT_HEAD).0, 404);
    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/run-1/approve", AT_HEAD).0, 405, "only the three actions write");
    assert_eq!(send(&roots, &fake, "POST", "/api/projects/other/reviews/run-1/resolve", AT_HEAD).0, 404, "run-1 is not other's");
    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/run-1/resolve", r#"{"head": 1, "who": "x"}"#).0, 400);
    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/run-1/resolve", r#"{"head": 7}"#).0, 400, "a head is a sha");

    review_inbox::record_local_merge(&Store::open(&roots.database()).unwrap(), "run-1", HEAD).unwrap();
    assert_eq!(send(&roots, &fake, "POST", "/api/reviews/run-1/dismiss", AT_HEAD).0, 409);
    assert_eq!(row(&roots, &fake, "run-1")["status"], "merged");
    assert!(fake.acted.lock().unwrap().is_empty(), "nothing refused is recorded as done");
}

#[test]
fn a_locally_merged_review_says_where_it_was_merged_and_offers_no_link() {
    let roots = roots("merged");
    let fake = Fake::default();
    review_inbox::record_local_merge(&Store::open(&roots.database()).unwrap(), "run-1", HEAD).unwrap();
    let listed = row(&roots, &fake, "run-1");
    assert_eq!((listed["status"].as_str(), listed["merged_locally"].as_str(), &listed["link"]), (Some("merged"), Some(HEAD), &Value::Null));
    assert_eq!(row(&roots, &fake, "run-2")["merged_locally"], Value::Null);
    let (status, detail) = send(&roots, &fake, "GET", "/api/reviews/run-1", "");
    assert_eq!((status, detail["merged_locally"].as_str(), detail["status"].as_str()), (200, Some(HEAD), Some("merged")));
}

#[test]
fn a_decision_whose_run_event_cannot_be_written_still_stands_and_the_reason_stays_off_the_wire() {
    let roots = roots("event-fails");
    actions_on(&roots);
    let fake = Fake { acted_fails: Some("cannot write /home/dana/.local/state/orq/runs/run-1/events.jsonl".into()), ..Fake::default() };

    let (status, body) = send(&roots, &fake, "POST", "/api/reviews/run-1/resolve", AT_HEAD);

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "resolved", "{body}");
    assert!(!body.to_string().contains("events.jsonl") && !body.to_string().contains("/home/"), "an internal error is not sent: {body}");
    assert_eq!(fake.acted.lock().unwrap().len(), 1, "the event was attempted");
    assert_eq!(row(&roots, &fake, "run-1")["status"], "resolved", "the decision is committed");
    assert_eq!(audit(&roots).last().map(|(_, outcome)| outcome.as_str()), Some("allowed"));
}
