//! `GET /api/projects/{id}/launch-profiles` lists the model profiles a run of project `id` can be started or
//! switched with, and the implementer and reviewer the popup's launch picker preselects. It changes nothing, so
//! it is not audited, but the web only needs it to act, so it is there only while remote actions are on.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake;

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn launch_profiles(&self, id: &str) -> Result<Option<Value>, String> {
        match id {
            "alpha" => Ok(Some(json!({"profiles": ["opus", "kimi"], "implementer": "opus", "reviewer": null}))),
            "broken" => Err("cannot read /home/me/.config/orq/config.toml".into()),
            _ => Ok(None),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-launchprofiles-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, method: &str, id: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/{id}/launch-profiles");
    let request = Request { method, path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(roots, &config, "p", "p", &request, &Fake);
    (r.status, r.body)
}

#[test]
fn the_profiles_and_defaults_are_listed_only_with_actions_on() {
    let roots = roots("list");
    assert_eq!(send(&roots, "GET", "alpha").0, 404, "off by default");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, "GET", "alpha"), (200, json!({"profiles": ["opus", "kimi"], "implementer": "opus", "reviewer": null})));
    assert_eq!(send(&roots, "GET", "broken"), (500, json!({"error": "the launch profiles could not be read"})), "the path stays on this machine");
    assert_eq!(send(&roots, "GET", "nope").0, 404);
    assert!(send(&roots, "POST", "alpha").0 == 404 || send(&roots, "POST", "alpha").0 == 405);
    assert!(remote_audit::read(&roots, 20).unwrap().is_empty(), "reading the profiles is not audited");
}
