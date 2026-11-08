//! `/api/projects/{id}/features`: that project's ledger features, read-only, behind the same checks.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    Setup { scan: base.join("code"), roots }
}

fn ask(setup: &Setup, method: &str, path: &str, authorization: Option<&str>) -> (u16, Value) {
    let provider = StoreProjects::new(setup.roots.clone(), setup.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization };
    let r = handle_with(&setup.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

fn get(setup: &Setup, path: &str) -> (u16, Value) {
    ask(setup, "GET", path, Some("Bearer tok"))
}

#[test]
fn a_project_lists_its_own_ledger_features_in_order_with_status_and_dependencies() {
    let s = setup("features-list");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    std::fs::write(alpha.join(".tdd-features.json"), json!([
        {"featureName": "first", "description": "does one", "dependsOn": [], "status": "completed", "recordedAt": "2026-11-01T00:00:00Z"},
        {"featureName": "second", "description": "does two", "dependsOn": ["first"], "status": "pending"},
        {"featureName": "odd", "dependsOn": [], "status": "waiting"}
    ]).to_string()).unwrap();
    std::fs::write(beta.join(".tdd-features.json"), json!([{"featureName": "only-beta", "status": "draft"}]).to_string()).unwrap();

    let (status, body) = get(&s, "/api/projects/alpha/features");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["features"], json!([
        {"name": "first", "description": "does one", "status": "completed", "depends_on": [], "recorded_at": "2026-11-01T00:00:00Z"},
        {"name": "second", "description": "does two", "status": "pending", "depends_on": ["first"], "recorded_at": null},
        {"name": "odd", "description": null, "status": "waiting", "depends_on": [], "recorded_at": null}
    ]), "{body}");
    let (_, beta_body) = get(&s, "/api/projects/beta/features");
    assert_eq!(beta_body["features"].as_array().unwrap().len(), 1, "{beta_body}");
}

#[test]
fn a_project_without_a_ledger_has_no_features_and_an_unknown_project_is_not_found() {
    let s = setup("features-none");
    git_repo(&s.scan, "alpha");
    let (status, body) = get(&s, "/api/projects/alpha/features");
    assert_eq!((status, &body["features"]), (200, &json!([])), "{body}");
    assert_eq!(get(&s, "/api/projects/gamma/features").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/features/extra").0, 404);
}

#[test]
fn a_ledger_that_cannot_be_read_is_an_error_that_does_not_name_the_file() {
    let s = setup("features-bad");
    let alpha = git_repo(&s.scan, "alpha");
    std::fs::write(alpha.join(".tdd-features.json"), "{ not json").unwrap();
    let (status, body) = get(&s, "/api/projects/alpha/features");
    assert_eq!(status, 500, "{body}");
    assert!(!body.to_string().contains(s.scan.to_str().unwrap()), "{body}");
}

#[test]
fn the_same_token_peer_and_method_checks_apply() {
    let s = setup("features-auth");
    git_repo(&s.scan, "alpha");
    assert_eq!(ask(&s, "GET", "/api/projects/alpha/features", None).0, 401);
    assert_eq!(ask(&s, "GET", "/api/projects/alpha/features", Some("Bearer wrong")).0, 401);
    assert_eq!(ask(&s, "POST", "/api/projects/alpha/features", Some("Bearer tok")).0, 405);
    assert_eq!(ask(&s, "HEAD", "/api/projects/alpha/features", Some("Bearer tok")).0, 200);
}

/// Asks for alpha's features and fails if the answer takes more than a few seconds, so a ledger
/// that blocks the server cannot hang the suite.
fn get_within_a_bound(s: Setup) -> (u16, Value) {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(get(&s, "/api/projects/alpha/features"));
    });
    receive.recv_timeout(std::time::Duration::from_secs(10)).expect("the ledger blocked the request")
}

#[test]
fn a_ledger_that_is_a_link_a_pipe_too_large_or_not_text_is_refused_without_naming_anything() {
    use std::os::unix::fs::symlink;
    let outside = web_deps::private_temp_dir("features-outside");
    let secret = outside.join("secret.json");
    std::fs::write(&secret, json!([{"featureName": "leaked", "status": "pending"}]).to_string()).unwrap();

    let cases: Vec<(&str, Box<dyn Fn(&Path)>)> = vec![
        ("link", Box::new(move |ledger| symlink(&secret, ledger).unwrap())),
        ("pipe", Box::new(|ledger| assert!(Command::new("mkfifo").arg(ledger).status().unwrap().success()))),
        ("large", Box::new(|ledger| std::fs::write(ledger, vec![b' '; 1024 * 1024 + 1]).unwrap())),
        ("binary", Box::new(|ledger| std::fs::write(ledger, [0xff, 0xfe, 0x00]).unwrap())),
    ];
    for (name, make) in cases {
        let s = setup(&format!("features-hostile-{name}"));
        let alpha = git_repo(&s.scan, "alpha");
        make(&alpha.join(".tdd-features.json"));
        let scan = s.scan.to_str().unwrap().to_string();
        let (status, body) = get_within_a_bound(s);
        assert_eq!(status, 500, "{name}: {body}");
        let text = body.to_string();
        assert!(!text.contains("leaked") && !text.contains(&scan) && !text.contains("secret"), "{name}: {text}");
    }
}
