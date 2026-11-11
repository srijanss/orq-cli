//! The web's feature editor reads a feature's fields as the popup's editor does, with a version of the ledger
//! as it was read: the SHA-256 of the file's bytes, so a later save can tell whether anyone changed it.

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

fn fields(s: &Setup, project: &str, feature: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/{project}/features/{feature}/fields");
    let request = Request { method: "GET", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, b"", &provider);
    (r.status, r.body)
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_fields_are_read_with_the_version_of_the_ledger_as_it_was_read() {
    let s = setup("fields");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    let ledger = r#"[{"featureName":"f-base","status":"completed"},{"featureName":"f-one","status":"pending","description":"One thing","dependsOn":["f-base"],"parallelizable":false,"testFile":"tests/one.rs","targetFiles":["src/one.rs","src/two.rs"]},{"featureName":"f-bare","status":"draft"}]"#;
    std::fs::write(alpha.join(".tdd-features.json"), ledger).unwrap();

    let (status, body) = fields(&s, "alpha", "f-one");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["fields"], json!({"name": "f-one", "description": "One thing", "depends_on": ["f-base"], "parallelizable": false, "test_file": "tests/one.rs", "target_files": ["src/one.rs", "src/two.rs"]}));
    assert_eq!(body["version"], sha256(ledger.as_bytes()));
    assert_eq!(fields(&s, "alpha", "f-bare").1["fields"], json!({"name": "f-bare", "description": null, "depends_on": [], "parallelizable": null, "test_file": null, "target_files": []}));

    // Any change to the file changes the version.
    let changed = ledger.replace("One thing", "One thing, reworded");
    std::fs::write(alpha.join(".tdd-features.json"), &changed).unwrap();
    assert_eq!(fields(&s, "alpha", "f-one").1["version"], sha256(changed.as_bytes()));

    // A feature, project or ledger that is not there is not found; an unreadable ledger is only said to have failed.
    assert_eq!(fields(&s, "alpha", "f-none").0, 404);
    assert_eq!(fields(&s, "beta", "f-one").0, 404, "beta has no ledger");
    assert_eq!(fields(&s, "gamma", "f-one").0, 404);
    std::fs::write(beta.join(".tdd-features.json"), "not json").unwrap();
    assert_eq!(fields(&s, "beta", "f-one"), (500, json!({"error": "the feature could not be read"})));
}
