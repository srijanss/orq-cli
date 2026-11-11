//! The web's feature editor saves the way the popup's Add and Edit do: an add appends a draft entry, an edit
//! changes only the fields that differ and keeps every other byte of the ledger. The save goes ahead only if
//! the ledger still has the version the editor read, so nobody's change is overwritten.

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

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    Setup { scan: base.join("code"), roots }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn post(s: &Setup, path: &str, body: Value) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let body = body.to_string();
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body.as_bytes(), &provider);
    (r.status, r.body)
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

const LEDGER: &str = r#"[
  {"featureName":"f-base","status":"completed","note":"kept"},
  {"featureName":"f-one","status":"pending","description":"One thing","dependsOn":["f-base"],"testFile":"tests/one.rs","targetFiles":["src/one.rs"]}
]
"#;

#[test]
fn an_edit_with_the_version_read_changes_only_the_fields_that_differ() {
    let s = setup("edit");
    let alpha = git_repo(&s.scan, "alpha");
    let file = alpha.join(".tdd-features.json");
    std::fs::write(&file, LEDGER).unwrap();
    let edit = json!({"fields": {"name": "f-one", "description": "One thing, reworded", "depends_on": ["f-base"], "parallelizable": true, "test_file": "tests/one.rs", "target_files": ["src/one.rs"]}, "version": sha256(LEDGER.as_bytes())});
    assert_eq!(post(&s, "/api/projects/alpha/features/f-one", edit), (200, json!({"feature": "f-one"})));
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains(r#"{"featureName":"f-base","status":"completed","note":"kept"}"#), "{saved}");
    let entries: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(entries[1]["description"], "One thing, reworded");
    assert_eq!(entries[1]["parallelizable"], true);
    assert_eq!(entries[1]["status"], "pending");
}

#[test]
fn a_ledger_changed_since_it_was_read_is_a_conflict_and_nothing_is_overwritten() {
    let s = setup("conflict");
    let alpha = git_repo(&s.scan, "alpha");
    let file = alpha.join(".tdd-features.json");
    std::fs::write(&file, LEDGER).unwrap();
    let stale = sha256(b"what the editor read earlier");
    let changed = json!({"error": "The ledger changed since you opened it; nothing was written"});
    let edit = json!({"fields": {"description": "Mine"}, "version": stale});
    assert_eq!(post(&s, "/api/projects/alpha/features/f-one", edit), (409, changed.clone()));
    let add = json!({"fields": {"name": "f-two"}, "version": stale});
    assert_eq!(post(&s, "/api/projects/alpha/features", add), (409, changed));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), LEDGER);
}

#[test]
fn an_add_appends_a_draft_and_refused_values_are_said_in_the_ledgers_words() {
    let s = setup("add");
    let alpha = git_repo(&s.scan, "alpha");
    let file = alpha.join(".tdd-features.json");

    // No ledger yet: the add creates it.
    assert_eq!(post(&s, "/api/projects/alpha/features", json!({"fields": {"name": "f-base"}})), (200, json!({"feature": "f-base"})));
    let first = std::fs::read(&file).unwrap();
    let add = json!({"fields": {"name": "f-two", "description": "Two", "depends_on": ["f-base"], "target_files": []}, "version": sha256(&first)});
    assert_eq!(post(&s, "/api/projects/alpha/features", add).0, 200);
    let entries: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(entries[1], json!({"featureName": "f-two", "description": "Two", "dependsOn": ["f-base"], "status": "draft"}));

    let before = std::fs::read(&file).unwrap();
    let refused = |name: &str, depends: &str| json!({"fields": {"name": name, "depends_on": [depends]}});
    assert_eq!(post(&s, "/api/projects/alpha/features", refused("f-two", "f-base")), (422, json!({"error": "featureName \"f-two\" is repeated"})));
    assert_eq!(post(&s, "/api/projects/alpha/features", refused("f-three", "f-nope")), (422, json!({"error": "feature \"f-three\" depends on \"f-nope\", which does not exist"})));
    assert_eq!(post(&s, "/api/projects/alpha/features", refused("../f-four", "f-base")), (422, json!({"error": "a feature name may use only letters, digits, '-', '_' and '.', and may not start with '.'"})));
    assert_eq!(std::fs::read(&file).unwrap(), before, "a refused save writes nothing");
}

#[test]
fn an_unknown_feature_or_project_is_not_found_and_an_unreadable_ledger_only_failed() {
    let s = setup("unknown");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    std::fs::write(alpha.join(".tdd-features.json"), LEDGER).unwrap();
    let edit = json!({"fields": {"description": "x"}, "version": sha256(LEDGER.as_bytes())});
    assert_eq!(post(&s, "/api/projects/alpha/features/f-none", edit.clone()).0, 404);
    assert_eq!(post(&s, "/api/projects/beta/features/f-one", edit.clone()).0, 404, "beta has no ledger");
    assert_eq!(post(&s, "/api/projects/gamma/features/f-one", edit.clone()).0, 404);
    std::fs::write(beta.join(".tdd-features.json"), "not json").unwrap();
    assert_eq!(post(&s, "/api/projects/beta/features", json!({"fields": {"name": "f-new"}})), (500, json!({"error": "the feature could not be saved"})));
}
