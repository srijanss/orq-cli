//! Archiving completed features from the web: a dry run counts what would move, the real action moves it
//! under the data directory, both need remote actions and leave an audit line, and the archive is read back
//! as a read-only list.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str, actions: bool) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    if actions {
        std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    }
    Setup { scan: base.join("code"), roots }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn call(s: &Setup, method: &'static str, path: &str, body: &[u8]) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &provider);
    (r.status, r.body)
}

/// 105 completed features c000..c104 and a pending one that depends on the oldest.
fn big_ledger(repo: &Path) {
    let mut entries: Vec<Value> = (0..105).map(|i| json!({"featureName": format!("c{i:03}"), "status": "completed"})).collect();
    entries.push(json!({"featureName": "next", "status": "pending", "dependsOn": ["c000"]}));
    std::fs::write(repo.join(".tdd-features.json"), serde_json::to_string(&entries).unwrap()).unwrap();
}

fn ledger_len(repo: &Path) -> usize {
    let ledger: Value = serde_json::from_str(&std::fs::read_to_string(repo.join(".tdd-features.json")).unwrap()).unwrap();
    ledger.as_array().unwrap().len()
}

#[test]
fn a_dry_run_counts_and_changes_nothing_then_the_real_action_moves_the_old_features() {
    let s = setup("archive-api", true);
    let alpha = git_repo(&s.scan, "alpha");
    big_ledger(&alpha);

    let (status, dry) = call(&s, "POST", "/api/projects/alpha/features/archive", br#"{"dry_run": true}"#);
    assert_eq!((status, &dry["archived"], &dry["dry_run"]), (200, &json!(5), &json!(true)), "{dry}");
    assert_eq!(dry["stripped"], json!([{"feature": "next", "names": ["c000"]}]));
    assert_eq!(ledger_len(&alpha), 106);

    let (status, done) = call(&s, "POST", "/api/projects/alpha/features/archive", br#"{"dry_run": false}"#);
    assert_eq!((status, &done["archived"], &done["dry_run"]), (200, &json!(5), &json!(false)), "{done}");
    assert_eq!(ledger_len(&alpha), 101);

    let (status, archived) = call(&s, "GET", "/api/projects/alpha/features/archived", b"");
    assert_eq!(status, 200);
    let names: Vec<&str> = archived["archived"].as_array().unwrap().iter().filter_map(|e| e["name"].as_str()).collect();
    assert_eq!(&names[..5], ["c000", "c001", "c002", "c003", "c004"]);

    let audit = orq_core::remote_audit::read(&s.roots, 10).unwrap();
    assert!(audit.iter().any(|line| line.to_string().contains("features.archive")), "{audit:?}");
}

#[test]
fn adding_a_feature_that_depends_on_an_archived_one_is_accepted_and_the_dependency_dropped() {
    let s = setup("archive-add-dep", true);
    let alpha = git_repo(&s.scan, "alpha");
    big_ledger(&alpha);
    assert_eq!(call(&s, "POST", "/api/projects/alpha/features/archive", br#"{"dry_run": false}"#).0, 200);

    let body = br#"{"fields": {"name": "later", "depends_on": ["c001"]}}"#;
    let (status, reply) = call(&s, "POST", "/api/projects/alpha/features", body);
    assert_eq!(status, 200, "{reply}");
    let ledger: Value = serde_json::from_str(&std::fs::read_to_string(alpha.join(".tdd-features.json")).unwrap()).unwrap();
    let later = ledger.as_array().unwrap().iter().find(|e| e["featureName"] == "later").unwrap();
    assert!(later.get("dependsOn").map_or(true, |d| d.as_array().unwrap().is_empty()), "{later}");
}

#[test]
fn archiving_needs_remote_actions_and_a_known_project() {
    let off = setup("archive-off", false);
    let alpha = git_repo(&off.scan, "alpha");
    big_ledger(&alpha);
    assert_eq!(call(&off, "POST", "/api/projects/alpha/features/archive", b"{}").0, 404);
    assert_eq!(ledger_len(&alpha), 106);

    let on = setup("archive-unknown", true);
    assert_eq!(call(&on, "POST", "/api/projects/nope/features/archive", b"{}").0, 404);
    assert_eq!(call(&on, "POST", "/api/projects/nope/features/archive", br#"{"keep": 1}"#).0, 400);
    // The archived list is a read, there with actions off, and empty before anything was archived.
    assert_eq!(call(&off, "GET", "/api/projects/alpha/features/archived", b"").1["archived"], json!([]));
}
