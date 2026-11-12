//! `GET /api/projects/{id}/features/{name}/fields` reads one feature's editable fields for the web's editor,
//! with a `version` of the ledger as it was read, so a later save can tell whether the ledger changed. It
//! changes nothing, so it is not audited, but it is there only while remote actions are on.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake;

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn feature_fields(&self, id: &str, feature: &str) -> Result<Option<Value>, String> {
        match (id, feature) {
            ("p-1", "f-1") => Ok(Some(json!({"fields": {"name": "f-1", "description": "One", "depends_on": ["f-0"], "parallelizable": true, "test_file": "tests/one.rs", "target_files": ["src/one.rs"]}, "version": "abc123"}))),
            ("p-1", "broken") => Err("could not read /home/someone/ledger.json".into()),
            _ => Ok(None),
        }
    }
}

fn roots(tag: &str, actions: bool) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-featurefields-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    if actions {
        std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    }
    roots
}

fn get(roots: &StorageRoots, method: &str, path: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, "p", "p", &request, b"", &Fake);
    (r.status, r.body)
}

#[test]
fn with_actions_on_the_fields_and_the_version_are_read_and_nothing_is_audited() {
    let roots = roots("on", true);
    let (status, body) = get(&roots, "GET", "/api/projects/p-1/features/f-1/fields");
    assert_eq!(status, 200);
    assert_eq!(body["fields"]["name"], "f-1");
    assert_eq!(body["fields"]["depends_on"], json!(["f-0"]));
    assert_eq!(body["fields"]["target_files"], json!(["src/one.rs"]));
    assert_eq!(body["version"], "abc123");
    assert_eq!(get(&roots, "GET", "/api/projects/p-1/features/nope/fields").0, 404);
    assert_eq!(get(&roots, "GET", "/api/projects/p-2/features/f-1/fields").0, 404);
    assert_eq!(get(&roots, "GET", "/api/projects/p-1/features/broken/fields"), (500, json!({"error": "the feature could not be read"})), "a path on this machine is never shown");
    assert!(remote_audit::read(&roots, 20).unwrap().is_empty(), "a read is not audited");
}

#[test]
fn with_actions_off_it_is_not_there_and_no_write_method_reads_it() {
    let roots = roots("off", false);
    assert_eq!(get(&roots, "GET", "/api/projects/p-1/features/f-1/fields").0, 404);
    let roots = self::roots("on-write", true);
    for method in ["PUT", "DELETE", "PATCH", "POST"] {
        let (status, _) = get(&roots, method, "/api/projects/p-1/features/f-1/fields");
        assert!(status == 404 || status == 405, "{method} answered {status}");
    }
}
