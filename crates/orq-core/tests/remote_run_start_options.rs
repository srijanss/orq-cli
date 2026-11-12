//! `POST /api/projects/{id}/features/{name}/runs` takes an optional body `{"implementer", "reviewer"}`, the
//! popup's launch picker: an empty body keeps the launch defaults, a body names both profiles and nothing
//! else. The provider refuses a profile it does not list; an allowed start names the profiles in its audit line.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, LaunchProfiles, ProjectsProvider, RemoteConfig, Request, RunStart};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    asked: Mutex<Vec<(String, Option<(String, String)>)>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn start_run(&self, _id: &str, feature: &str) -> Result<RunStart, String> {
        self.asked.lock().unwrap().push((feature.into(), None));
        Ok(RunStart::Started("r-default".into()))
    }

    fn start_run_with(&self, _id: &str, feature: &str, profiles: &LaunchProfiles) -> Result<RunStart, String> {
        self.asked.lock().unwrap().push((feature.into(), Some((profiles.implementer.clone(), profiles.reviewer.clone()))));
        if profiles.implementer == "nope" {
            return Ok(RunStart::Refused("the profile nope is not defined in the model profiles".into()));
        }
        Ok(RunStart::Started("r-chosen".into()))
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-runstartopts-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    roots
}

fn send(roots: &StorageRoots, fake: &Fake, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "POST", path: "/api/projects/p-1/features/f-1/runs", peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(roots, &config, "p", "p", &request, body.as_bytes(), fake);
    (r.status, r.body)
}

fn audit(roots: &StorageRoots) -> Vec<(String, String)> {
    remote_audit::read(roots, 20).unwrap().iter().rev().map(|l| (l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string())).collect()
}

#[test]
fn an_empty_body_keeps_the_defaults_and_a_body_naming_both_profiles_starts_with_them() {
    let roots = roots("chosen");
    let fake = Fake::default();
    assert_eq!(send(&roots, &fake, ""), (202, json!({"run": "r-default"})));
    assert_eq!(send(&roots, &fake, "{}"), (202, json!({"run": "r-default"})));
    assert_eq!(send(&roots, &fake, r#"{"implementer": "opus", "reviewer": "sonnet"}"#), (202, json!({"run": "r-chosen"})));
    assert_eq!(send(&roots, &fake, r#"{"implementer": "nope", "reviewer": "sonnet"}"#), (409, json!({"error": "the profile nope is not defined in the model profiles"})));
    let asked = fake.asked.lock().unwrap().clone();
    assert_eq!(asked[0], ("f-1".to_string(), None));
    assert_eq!(asked[1], ("f-1".to_string(), None));
    assert_eq!(asked[2], ("f-1".to_string(), Some(("opus".to_string(), "sonnet".to_string()))));
    let row = |o: &str, r: &str| (o.to_string(), r.to_string());
    assert_eq!(audit(&roots), [row("allowed", ""), row("allowed", ""), row("allowed", "implementer opus, reviewer sonnet"), row("refused", "the profile nope is not defined in the model profiles")]);
}

#[test]
fn a_body_must_name_both_profiles_and_nothing_else_or_nothing_is_started() {
    let roots = roots("bad");
    let fake = Fake::default();
    for body in [
        r#"{"implementer": "opus"}"#,
        r#"{"reviewer": "sonnet"}"#,
        r#"{"implementer": "", "reviewer": "sonnet"}"#,
        r#"{"implementer": "opus", "reviewer": "  "}"#,
        r#"{"implementer": 1, "reviewer": "sonnet"}"#,
        r#"{"implementer": "opus", "reviewer": "sonnet", "mcp": []}"#,
        r#"{"implementer": "opus", "reviewer": "sonnet", "extra": "x"}"#,
        r#"["opus", "sonnet"]"#,
        "not json",
    ] {
        assert_eq!(send(&roots, &fake, body), (400, json!({"error": "the request body is not one this action takes"})), "{body}");
    }
    assert!(fake.asked.lock().unwrap().is_empty());
    assert!(audit(&roots).iter().all(|(o, r)| o == "refused" && r == "bad request body"), "{:?}", audit(&roots));
}
