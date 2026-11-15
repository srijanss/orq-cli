//! `POST /api/projects/{id}/features/{name}/runs` with `"group": [names...]` starts one group run of those
//! features, in the order given, instead of a run of `{name}` alone; the path names the group's first feature.
//! The chosen profiles may come with it. A provider that cannot start a group run refuses it.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, LaunchProfiles, ProjectsProvider, RemoteConfig, Request, RunStart};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

type Asked = (Vec<String>, Option<(String, String)>);

#[derive(Default)]
struct Fake {
    groups: Mutex<Vec<Asked>>,
    singles: Mutex<Vec<String>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn start_run(&self, _id: &str, feature: &str) -> Result<RunStart, String> {
        self.singles.lock().unwrap().push(feature.into());
        Ok(RunStart::Started("r-single".into()))
    }

    fn start_group_run(&self, _id: &str, features: &[String], profiles: Option<&LaunchProfiles>) -> Result<RunStart, String> {
        self.groups.lock().unwrap().push((features.to_vec(), profiles.map(|p| (p.implementer.clone(), p.reviewer.clone()))));
        Ok(RunStart::Started("r-group".into()))
    }
}

struct Plain;

impl ProjectsProvider for Plain {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-grouprunstart-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    roots
}

fn send(roots: &StorageRoots, provider: &dyn ProjectsProvider, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path: "/api/projects/p-1/features/a/runs", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, "p", "p", &request, body.as_bytes(), provider);
    (r.status, r.body)
}

fn audit(roots: &StorageRoots) -> Vec<(String, String, String)> {
    remote_audit::read(roots, 20)
        .unwrap()
        .iter()
        .rev()
        .map(|l| (l["outcome"].as_str().unwrap().to_string(), l["run"].as_str().unwrap_or("").to_string(), l["reason"].as_str().unwrap_or("").to_string()))
        .collect()
}

#[test]
fn a_group_body_starts_one_group_run_of_its_features_in_order_with_or_without_profiles() {
    let roots = roots("group");
    let fake = Fake::default();
    assert_eq!(send(&roots, &fake, r#"{"group": ["a", "c", "b"]}"#), (202, json!({"run": "r-group"})));
    assert_eq!(send(&roots, &fake, r#"{"group": ["a", "b"], "implementer": "opus", "reviewer": "haiku"}"#), (202, json!({"run": "r-group"})));
    let groups = fake.groups.lock().unwrap().clone();
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(groups, [(names(&["a", "c", "b"]), None), (names(&["a", "b"]), Some(("opus".to_string(), "haiku".to_string())))]);
    assert!(fake.singles.lock().unwrap().is_empty());
    let row = |o: &str, run: &str, r: &str| (o.to_string(), run.to_string(), r.to_string());
    assert_eq!(audit(&roots), [row("allowed", "r-group", "group a, c, b"), row("allowed", "r-group", "group a, b, implementer opus, reviewer haiku")]);
}

#[test]
fn a_group_of_only_the_paths_feature_is_a_plain_run() {
    let roots = roots("one");
    let fake = Fake::default();
    assert_eq!(send(&roots, &fake, r#"{"group": ["a"]}"#), (202, json!({"run": "r-single"})));
    assert_eq!(fake.singles.lock().unwrap().clone(), ["a"]);
    assert!(fake.groups.lock().unwrap().is_empty());
}

#[test]
fn a_group_that_does_not_lead_with_the_paths_feature_or_names_a_bad_or_repeated_feature_starts_nothing() {
    let roots = roots("bad");
    let fake = Fake::default();
    for body in [
        r#"{"group": []}"#,
        r#"{"group": ["b", "a"]}"#,
        r#"{"group": ["a", "a"]}"#,
        r#"{"group": ["a", "b", "a"]}"#,
        r#"{"group": ["a", "../x"]}"#,
        r#"{"group": ["a", ".hidden"]}"#,
        r#"{"group": ["a", ""]}"#,
        r#"{"group": ["a", 1]}"#,
        r#"{"group": "a,b"}"#,
        r#"{"group": ["a", "b"], "implementer": "opus"}"#,
    ] {
        assert_eq!(send(&roots, &fake, body), (400, json!({"error": "the request body is not one this action takes"})), "{body}");
    }
    assert!(fake.groups.lock().unwrap().is_empty() && fake.singles.lock().unwrap().is_empty());
    assert!(audit(&roots).iter().all(|(o, _, r)| o == "refused" && r == "bad request body"), "{:?}", audit(&roots));
}

#[test]
fn a_provider_that_cannot_start_a_group_run_refuses_it() {
    let roots = roots("plain");
    assert_eq!(send(&roots, &Plain, r#"{"group": ["a", "b"]}"#), (409, json!({"error": "this server cannot start a group run"})));
}
