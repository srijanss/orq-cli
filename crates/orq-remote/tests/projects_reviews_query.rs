//! `/api/projects/{id}/reviews` takes the same filter, sort and cursor parameters as the runs list.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn setup(label: &str) -> impl Fn(&str) -> (u16, Value) {
    let base = web_deps::private_temp_dir(label);
    let scan = base.join("code");
    let alpha = git_repo(&scan, "alpha");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let key = orq_git::project_id(&orq_git::resolve_project(&alpha).unwrap());
    let store = Store::open(&roots.database()).unwrap();
    for (n, status) in [(1, "current"), (2, "stale"), (3, "current")] {
        let report = serde_json::to_vec(&json!({"status": status, "summary": {"files_changed": 1, "added": 1, "deleted": 0}, "files": [], "tests": {"status": "passed", "commands": []}, "risks": [], "rounds": [], "gaps": []})).unwrap();
        store.save_review(&roots.data, &format!("run-{n}"), &key, &format!("f{n}"), &format!("{n}").repeat(40), &report).unwrap();
    }
    let provider = StoreProjects::new(roots.clone(), scan);
    let config = RemoteConfig { token: "tok".into() };
    move |path: &str| {
        let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let r = handle_with(&roots, &config, "server-project", "server", &request, &provider);
        (r.status, r.body)
    }
}

fn ids(body: &Value) -> Vec<String> {
    body["reviews"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn reviews_page_with_a_cursor_and_report_the_count_of_the_whole_filtered_set() {
    let ask = setup("reviews-paging");
    let (status, all) = ask("/api/projects/alpha/reviews");
    assert_eq!(status, 200, "{all}");
    let order = ids(&all);
    assert_eq!(order.len(), 3, "{all}");

    let (_, first) = ask("/api/projects/alpha/reviews?limit=2");
    assert_eq!(ids(&first), order[..2], "{first}");
    assert_eq!(first["total"], 3, "{first}");
    let cursor = first["next_cursor"].as_str().expect("more reviews remain").to_string();
    let (_, second) = ask(&format!("/api/projects/alpha/reviews?limit=2&cursor={cursor}"));
    assert_eq!(ids(&second), order[2..], "{second}");
    assert_eq!(second["next_cursor"], Value::Null, "{second}");
}

#[test]
fn reviews_filter_by_status_and_refuse_a_bad_parameter() {
    let ask = setup("reviews-filter");
    let (_, all) = ask("/api/projects/alpha/reviews");
    let statuses: Vec<String> = all["reviews"].as_array().unwrap().iter().map(|r| r["status"].as_str().unwrap_or("?").to_string()).collect();
    let wanted = statuses[0].clone();
    let expected = statuses.iter().filter(|s| **s == wanted).count();
    let (_, body) = ask(&format!("/api/projects/alpha/reviews?status={wanted}"));
    assert_eq!(ids(&body).len(), expected, "{body}");
    assert_eq!(body["total"], expected, "{body}");
    assert_eq!(ask("/api/projects/alpha/reviews?limit=0").0, 400);
    assert_eq!(ask("/api/projects/alpha/reviews?cursor=nope").0, 404);
}
