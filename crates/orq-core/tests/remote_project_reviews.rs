//! `/api/projects/{id}/reviews` and `/api/projects/{id}/reviews/{run}` serve the review list and
//! report of one project. They sit behind the same peer, token and method checks as every path,
//! and a report of another project is never served through a project it does not belong to.

use std::cell::Cell;
use std::net::IpAddr;

use orq_core::remote_inbox::{handle, handle_with, ProjectsProvider, RemoteConfig, Request, ReviewScope};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use serde_json::{json, Value};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OURS: &str = "ours-0a1b2c3d";
const OTHER: &str = "other-11223344";

struct Fixed {
    asked: Cell<usize>,
}

impl ProjectsProvider for Fixed {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }
    fn review_scope(&self, id: &str) -> Result<Option<ReviewScope>, String> {
        self.asked.set(self.asked.get() + 1);
        match id {
            "other" => Ok(Some(ReviewScope { review_key: OTHER.into(), name: "other".into() })),
            "broken" => Err("cannot resolve".into()),
            _ => Ok(None),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-project-reviews-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let store = Store::open(&roots.database()).unwrap();
    let body = serde_json::to_vec(&json!({"status": "current", "summary": {"files_changed": 1, "added": 1, "deleted": 0}, "files": [], "tests": {"status": "passed", "commands": []}, "risks": [], "rounds": [], "gaps": []})).unwrap();
    store.save_review(&roots.data, "run-ours", OURS, "feat-ours", HEAD, &body).unwrap();
    store.save_review(&roots.data, "run-other", OTHER, "feat-other", HEAD, &body).unwrap();
    roots
}

fn ask(roots: &StorageRoots, provider: Option<&Fixed>, method: &str, path: &str, auth: Option<&str>, peer: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let peer: IpAddr = peer.parse().unwrap();
    let request = Request { method, path, peer, authorization: auth };
    let r = match provider {
        Some(p) => handle_with(roots, &config, OURS, "ours", &request, p),
        None => handle(roots, &config, OURS, "ours", &request),
    };
    (r.status, r.body)
}

fn get(roots: &StorageRoots, p: &Fixed, path: &str) -> (u16, Value) {
    ask(roots, Some(p), "GET", path, Some("Bearer tok"), "127.0.0.1")
}

#[test]
fn a_project_lists_only_its_own_reviews() {
    let roots = roots("list");
    let p = Fixed { asked: Cell::new(0) };
    let (status, body) = get(&roots, &p, "/api/projects/other/reviews");
    assert_eq!(status, 200, "{body}");
    let rows = body["reviews"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!((rows[0]["run_id"].as_str(), rows[0]["project"].as_str(), rows[0]["current"].as_bool()), (Some("run-other"), Some("other"), Some(true)));
}

#[test]
fn a_report_is_served_only_through_the_project_it_belongs_to() {
    let roots = roots("one");
    let p = Fixed { asked: Cell::new(0) };
    let (status, body) = get(&roots, &p, "/api/projects/other/reviews/run-other");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["run_id"], "run-other");
    assert_eq!(get(&roots, &p, "/api/projects/other/reviews/run-ours").0, 404);
    assert_eq!(get(&roots, &p, "/api/projects/other/reviews/..%2Fx").0, 400);
}

#[test]
fn an_unknown_project_is_not_found_and_a_provider_failure_is_an_error() {
    let roots = roots("unknown");
    let p = Fixed { asked: Cell::new(0) };
    assert_eq!(get(&roots, &p, "/api/projects/nope/reviews").0, 404);
    assert_eq!(get(&roots, &p, "/api/projects/nope/reviews/run-other").0, 404);
    assert_eq!(get(&roots, &p, "/api/projects/broken/reviews").0, 500);
    assert_eq!(get(&roots, &p, "/api/projects/other/reviews/a/b").0, 404);
    assert_eq!(get(&roots, &p, "/api/projects//reviews").0, 404);
}

#[test]
fn a_refused_request_never_reaches_the_provider() {
    let roots = roots("auth");
    let p = Fixed { asked: Cell::new(0) };
    let path = "/api/projects/other/reviews";
    assert_eq!(ask(&roots, Some(&p), "GET", path, None, "127.0.0.1").0, 401);
    assert_eq!(ask(&roots, Some(&p), "GET", path, Some("Bearer wrong"), "127.0.0.1").0, 401);
    assert_eq!(ask(&roots, Some(&p), "GET", path, Some("Bearer tok"), "8.8.8.8").0, 401);
    assert_eq!(ask(&roots, Some(&p), "POST", path, Some("Bearer tok"), "127.0.0.1").0, 405);
    assert_eq!(p.asked.get(), 0);
}

#[test]
fn without_a_provider_the_project_paths_do_not_exist_and_the_old_paths_still_work() {
    let roots = roots("single");
    assert_eq!(ask(&roots, None, "GET", "/api/projects/other/reviews", Some("Bearer tok"), "127.0.0.1").0, 404);
    let (status, body) = ask(&roots, None, "GET", "/api/reviews", Some("Bearer tok"), "127.0.0.1");
    assert_eq!(status, 200);
    assert_eq!(body["reviews"].as_array().unwrap().len(), 2, "{body}");
    assert_eq!(ask(&roots, None, "GET", "/api/reviews/run-other", Some("Bearer tok"), "127.0.0.1").0, 200);
}
