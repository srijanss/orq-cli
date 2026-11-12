//! `POST /api/projects/{id}/settings` saves project `id`'s own agents and delivery. Like every action
//! it is there only while remote actions are on, takes only an offered harness, model and effort and a
//! known delivery, saves only while the revision presented is current, and leaves a project.settings
//! audit line for each attempt. The body names the whole record; a null role is reset.

use std::cell::RefCell;

use orq_core::project_settings::{Delivery, ProjectAgent, ProjectSettings};
use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, ProjectSettingsSave, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake {
    saved: RefCell<Vec<(String, u64, ProjectSettings)>>,
    options: Result<Option<Value>, String>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn setting_options(&self) -> Result<Option<Value>, String> {
        self.options.clone()
    }

    fn save_project_settings(&self, id: &str, revision: u64, settings: &ProjectSettings) -> Result<ProjectSettingsSave, String> {
        self.saved.borrow_mut().push((id.to_string(), revision, settings.clone()));
        match (id, revision) {
            ("alpha", 4) => Ok(ProjectSettingsSave::Saved(json!({"revision": 5}))),
            ("alpha", _) => Ok(ProjectSettingsSave::Conflict),
            ("broken", _) => Err("cannot write /home/me/.local/share/orq/orq.db".into()),
            _ => Ok(ProjectSettingsSave::Unknown),
        }
    }
}

fn roots(tag: &str, on: bool) -> StorageRoots {
    let base = std::env::temp_dir().join(format!("orq-projectsettingssave-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), format!("remote_actions = {on}\n")).unwrap();
    roots
}

fn fake() -> Fake {
    Fake { saved: RefCell::new(Vec::new()), options: Ok(Some(json!([{"harness": "claude", "models": ["opus", "sonnet"], "efforts": ["low", "high"]}, {"harness": "pi", "models": ["kimi"]}]))) }
}

fn save(roots: &StorageRoots, fake: &Fake, id: &str, body: Value) -> (u16, Value) {
    let path = format!("/api/projects/{id}/settings");
    let request = Request { method: "POST", path: &path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let reply = handle_with_body(roots, &RemoteConfig { token: "tok".into() }, "p", "p", &request, body.to_string().as_bytes(), fake);
    (reply.status, reply.body)
}

fn audit(roots: &StorageRoots) -> Vec<(Value, Value, Value)> {
    remote_audit::read(roots, 100).unwrap().into_iter().map(|line| (line["action"].clone(), line["project"].clone(), line["outcome"].clone())).collect()
}

fn whole() -> Value {
    json!({"implementer": {"harness": "claude", "model": "sonnet", "effort": "high"}, "reviewer": null, "delivery": "push", "revision": 4})
}

#[test]
fn a_whole_record_at_the_current_revision_is_saved_and_answered_with_the_settings_read_back() {
    let roots = roots("saved", true);
    let fake = fake();
    assert_eq!(save(&roots, &fake, "alpha", whole()), (200, json!({"settings": {"revision": 5}})));
    let implementer = ProjectAgent { harness: "claude".into(), model: "sonnet".into(), effort: Some("high".into()) };
    assert_eq!(fake.saved.into_inner(), vec![("alpha".to_string(), 4, ProjectSettings { implementer: Some(implementer), reviewer: None, delivery: Delivery::Push })]);
    assert_eq!(audit(&roots), vec![(json!("project.settings"), json!("alpha"), json!("allowed"))]);
}

#[test]
fn with_actions_off_or_a_body_that_is_not_a_whole_record_nothing_is_saved_and_the_refusal_is_audited() {
    let off = roots("off", false);
    let fake = fake();
    assert_eq!(save(&off, &fake, "alpha", whole()), (404, json!({"error": "not found"})));
    assert_eq!(audit(&off), vec![(json!("project.settings"), json!("alpha"), json!("refused"))]);

    let on = roots("bad-body", true);
    let with = |key: &str, value: Value| {
        let mut body = whole();
        body[key] = value;
        body
    };
    let without = |key: &str| {
        let mut body = whole();
        body.as_object_mut().unwrap().remove(key);
        body
    };
    let bad = [
        without("implementer"),
        without("reviewer"),
        without("delivery"),
        without("revision"),
        with("extra", json!(1)),
        with("delivery", json!("deploy")),
        with("revision", json!(-1)),
        with("revision", json!("4")),
        with("implementer", json!("sonnet")),
        with("implementer", json!({"harness": "claude"})),
        with("implementer", json!({"harness": "claude", "model": "sonnet", "profile": "x"})),
        with("implementer", json!({"harness": "claude", "model": "sonnet", "effort": 3})),
        json!([1]),
    ];
    for body in &bad {
        assert_eq!(save(&on, &fake, "alpha", body.clone()), (400, json!({"error": "the request body is not one this action takes"})), "{body}");
    }
    assert!(fake.saved.into_inner().is_empty());
    assert_eq!(audit(&on).len(), bad.len());
    assert!(audit(&on).iter().all(|line| line == &(json!("project.settings"), json!("alpha"), json!("refused"))));
}

#[test]
fn only_an_offered_harness_model_and_effort_are_saved_and_a_reset_needs_no_options() {
    let roots = roots("offered", true);
    let fake = fake();
    let role = |agent: Value| {
        let mut body = whole();
        body["reviewer"] = agent;
        body
    };
    let refused = [
        (role(json!({"harness": "codex", "model": "sonnet"})), "That harness or model is not one this machine offers"),
        (role(json!({"harness": "pi", "model": "sonnet"})), "That harness or model is not one this machine offers"),
        (role(json!({"harness": "claude", "model": "opus", "effort": "max"})), "That effort is not one this harness takes"),
        (role(json!({"harness": "pi", "model": "kimi", "effort": "low"})), "That effort is not one this harness takes"),
    ];
    for (body, why) in &refused {
        assert_eq!(save(&roots, &fake, "alpha", body.clone()), (422, json!({"error": why})), "{body}");
    }
    assert!(fake.saved.borrow().is_empty());
    assert_eq!(save(&roots, &fake, "alpha", role(json!({"harness": "pi", "model": "kimi"}))).0, 200);

    let reset = json!({"implementer": null, "reviewer": null, "delivery": "review", "revision": 4});
    let none = Fake { saved: RefCell::new(Vec::new()), options: Ok(None) };
    assert_eq!(save(&roots, &none, "alpha", reset).0, 200, "resetting both roles names no agent to check");
    assert_eq!(save(&roots, &none, "alpha", whole()), (404, json!({"error": "not found"})), "no options, no agent can be chosen");
    let failing = Fake { saved: RefCell::new(Vec::new()), options: Err("cannot run /usr/bin/pi".into()) };
    assert_eq!(save(&roots, &failing, "alpha", whole()), (500, json!({"error": "the options could not be read"})));
    let outcomes: Vec<Value> = audit(&roots).into_iter().rev().map(|line| line.2).collect();
    assert_eq!(outcomes, [vec![json!("refused"); 4], vec![json!("allowed"); 2], vec![json!("refused"); 2]].concat());
}

#[test]
fn a_stale_revision_an_unknown_project_and_a_failure_on_this_machine_save_nothing_and_are_audited() {
    let roots = roots("outcomes", true);
    let fake = fake();
    let mut stale = whole();
    stale["revision"] = json!(3);
    assert_eq!(save(&roots, &fake, "alpha", stale), (409, json!({"error": "The project's settings changed since they were read"})));
    assert_eq!(save(&roots, &fake, "nope", whole()), (404, json!({"error": "not found"})));
    assert_eq!(save(&roots, &fake, "broken", whole()), (500, json!({"error": "the project's settings could not be saved"})));
    let lines = remote_audit::read(&roots, 100).unwrap();
    assert_eq!(lines.iter().rev().map(|line| (line["project"].clone(), line["outcome"].clone())).collect::<Vec<_>>(), vec![(json!("alpha"), json!("refused")), (json!("nope"), json!("refused")), (json!("broken"), json!("refused"))]);
    assert!(!serde_json::to_string(&lines).unwrap().contains("/home/me"), "{lines:?}");
}
