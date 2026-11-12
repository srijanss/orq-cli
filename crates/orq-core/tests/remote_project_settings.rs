//! `GET /api/projects/{id}/settings` gives project `id`'s own settings, for its Settings page. Like
//! `/api/settings` it is a read, so it is there with remote actions off and is not audited. A failure
//! on this machine is only said to have failed, since its reason can name a path here.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake;

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn project_settings(&self, id: &str) -> Result<Option<Value>, String> {
        match id {
            "alpha" => Ok(Some(json!({"delivery": "push", "revision": 3}))),
            "broken" => Err("cannot read /home/me/code/alpha/.orq.toml".into()),
            _ => Ok(None),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-projectsettings-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, method: &str, id: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/{id}/settings");
    let request = Request { method, path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(roots, &config, "p", "p", &request, &Fake);
    (r.status, r.body)
}

#[test]
fn a_project_s_settings_are_read_with_actions_off_and_a_failure_names_no_path() {
    let roots = roots("read");
    assert_eq!(send(&roots, "GET", "alpha"), (200, json!({"settings": {"delivery": "push", "revision": 3}})));
    assert_eq!(send(&roots, "GET", "broken"), (500, json!({"error": "the project's settings could not be read"})));
    assert_eq!(send(&roots, "GET", "nope").0, 404);
    assert!(remote_audit::read(&roots, 20).unwrap().is_empty(), "reading settings is not audited");
}

#[test]
fn a_project_s_settings_need_the_token() {
    let roots = roots("token");
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path: "/api/projects/alpha/settings", peer: "127.0.0.1".parse().unwrap(), authorization: None };
    assert_eq!(handle_with(&roots, &config, "p", "p", &request, &Fake).status, 401);
}
