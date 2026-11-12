//! `POST /api/projects/{id}/runs/{run}/model` with `{"role", "profile"}` records that a paused run continues
//! `role` on another model profile when it resumes, the way the popup's Switch model does. It is there only
//! when remote actions are on; the body must name a role and a profile and nothing else. A refusal is the
//! orchestrator's own words; every attempt is audited as run.model, with the switch asked for.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectsProvider, RemoteConfig, Request, RunControlOutcome};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};
use std::sync::Mutex;

struct Fake {
    asked: Mutex<Vec<(String, String, String)>>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn switch_model(&self, _id: &str, run: &str, role: &str, profile: &str) -> Result<RunControlOutcome, String> {
        self.asked.lock().unwrap().push((run.into(), role.into(), profile.into()));
        match (run, profile) {
            (_, "nope") => Ok(RunControlOutcome::Refused("the profile nope is not defined in the model profiles".into())),
            ("r-paused", _) => Ok(RunControlOutcome::Done(run.into())),
            ("r-run", _) => Ok(RunControlOutcome::Refused("r-run is running; only a paused run can switch models".into())),
            ("r-broken", _) => Err("cannot read /home/me/.local/state/orq/runs/r-broken".into()),
            _ => Ok(RunControlOutcome::Unknown),
        }
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!("orq-runmodel-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, run: &str, body: &str) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/p-1/runs/{run}/model");
    let request = Request { method, path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&roots, &config, "p", "p", &request, body.as_bytes(), fake);
    (r.status, r.body)
}

fn audit(roots: &StorageRoots) -> Vec<(String, String, String)> {
    remote_audit::read(roots, 20)
        .unwrap()
        .iter()
        .rev()
        .map(|l| (l["action"].as_str().unwrap().to_string(), l["outcome"].as_str().unwrap().to_string(), l["reason"].as_str().unwrap_or("").to_string()))
        .collect()
}

const OPUS: &str = r#"{"role": "implementer", "profile": "opus"}"#;

#[test]
fn a_paused_run_switches_only_with_actions_on_and_every_attempt_is_audited_as_run_model() {
    let roots = roots("switch");
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    assert_eq!(send(&roots, &fake, "POST", "r-paused", OPUS).0, 404, "off by default");
    assert!(fake.asked.lock().unwrap().is_empty());
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();

    assert_eq!(send(&roots, &fake, "POST", "r-paused", OPUS), (202, json!({"run": "r-paused", "role": "implementer", "profile": "opus"})));
    assert_eq!(send(&roots, &fake, "POST", "r-run", OPUS), (409, json!({"error": "r-run is running; only a paused run can switch models"})));
    assert_eq!(send(&roots, &fake, "POST", "r-paused", r#"{"role": "reviewer", "profile": "nope"}"#), (409, json!({"error": "the profile nope is not defined in the model profiles"})));
    assert_eq!(send(&roots, &fake, "POST", "r-broken", OPUS), (500, json!({"error": "the model could not be switched"})));
    assert_eq!(send(&roots, &fake, "POST", "r-none", OPUS).0, 404);
    assert_eq!(fake.asked.lock().unwrap()[0], ("r-paused".to_string(), "implementer".to_string(), "opus".to_string()));

    let row = |o: &str, r: &str| ("run.model".to_string(), o.to_string(), r.to_string());
    assert_eq!(
        audit(&roots),
        [
            row("refused", "remote actions are off"),
            row("allowed", "implementer on opus"),
            row("refused", "r-run is running; only a paused run can switch models"),
            row("refused", "the profile nope is not defined in the model profiles"),
            row("refused", "the model could not be switched"),
            row("refused", "no such project or run"),
        ]
    );
}

#[test]
fn the_body_must_name_a_role_and_a_profile_and_nothing_else() {
    let roots = roots("body");
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let fake = Fake { asked: Mutex::new(Vec::new()) };
    for body in [
        "",
        "{}",
        r#"{"role": "implementer"}"#,
        r#"{"role": "planner", "profile": "opus"}"#,
        r#"{"role": "implementer", "profile": ""}"#,
        r#"{"role": "implementer", "profile": 3}"#,
        r#"{"role": "implementer", "profile": "opus", "harness": "claude"}"#,
        "not json",
    ] {
        assert_eq!(send(&roots, &fake, "POST", "r-paused", body), (400, json!({"error": "the request body is not one this action takes"})), "{body}");
    }
    assert!(fake.asked.lock().unwrap().is_empty());
    assert!(audit(&roots).iter().all(|(action, outcome, reason)| action == "run.model" && outcome == "refused" && reason == "bad request body"));
    assert!(matches!(send(&roots, &fake, "GET", "r-paused", OPUS).0, 404 | 405));
    assert!(fake.asked.lock().unwrap().is_empty(), "only a POST switches");
}
