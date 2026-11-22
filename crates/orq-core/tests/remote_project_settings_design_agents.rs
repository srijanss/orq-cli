//! `POST /api/projects/{id}/settings` takes a project's design preparer, design reviewer and design verifier
//! next to its implementer and reviewer, each null (unset) or `{harness, model, effort}`, and refuses one that
//! is not a design agent: a harness Orq does not run read-only for review, a model that doesn't accept images
//! (for pi, as its `pi --list-models` says) or a thinking level its harness doesn't take.

use std::cell::{Cell, RefCell};

use orq_core::project_settings::{ProjectAgent, ProjectSettings};
use orq_core::remote_inbox::{
    handle_with_body, ProjectSettingsSave, ProjectsProvider, RemoteConfig, Request,
};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

const PI_MODELS: &str = "provider  model          context  max-out  thinking  images\nopenai    gpt-6.1-sol    400K     128K     yes       yes\nkimi      kimi-text      128K     32K      no        no\n";

struct Fake {
    saved: RefCell<Vec<ProjectSettings>>,
    listed: Cell<u32>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn setting_options(&self) -> Result<Option<Value>, String> {
        Ok(Some(json!([
            {"harness": "claude", "models": ["opus", "sonnet"], "efforts": ["low", "medium", "high"]},
            {"harness": "pi", "models": ["openai/gpt-6.1-sol", "kimi/kimi-text"], "efforts": ["off", "low", "medium", "high"]},
            {"harness": "codex", "models": ["gpt-5"]}
        ])))
    }

    fn pi_models(&self) -> Result<Option<String>, String> {
        self.listed.set(self.listed.get() + 1);
        Ok(Some(PI_MODELS.to_string()))
    }

    fn save_project_settings(
        &self,
        _id: &str,
        _revision: u64,
        settings: &ProjectSettings,
    ) -> Result<ProjectSettingsSave, String> {
        self.saved.borrow_mut().push(settings.clone());
        Ok(ProjectSettingsSave::Saved(json!({"revision": 1})))
    }
}

fn roots(tag: &str) -> StorageRoots {
    let base = orq_test_support::process_temp_dir().join(format!(
        "orq-designagentsinbox-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    roots
}

fn fake() -> Fake {
    Fake {
        saved: RefCell::new(Vec::new()),
        listed: Cell::new(0),
    }
}

fn save(roots: &StorageRoots, fake: &Fake, design: Value) -> (u16, Value) {
    let mut body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": null, "revision": 0});
    for (key, value) in design.as_object().unwrap() {
        body[key] = value.clone();
    }
    let request = Request {
        method: "POST",
        path: "/api/projects/alpha/settings",
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let reply = handle_with_body(
        roots,
        &RemoteConfig {
            token: "tok".into(),
        },
        "p",
        "p",
        &request,
        body.to_string().as_bytes(),
        fake,
    );
    (reply.status, reply.body)
}

fn agent(harness: &str, model: &str, effort: Option<&str>) -> ProjectAgent {
    ProjectAgent {
        harness: harness.into(),
        model: model.into(),
        effort: effort.map(str::to_string),
    }
}

#[test]
fn the_three_design_roles_are_passed_on_to_be_saved_and_a_body_without_them_leaves_them_unset() {
    let roots = roots("saved");
    let fake = fake();
    let design = json!({
        "design_preparer": {"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "medium"},
        "design_reviewer": {"harness": "claude", "model": "opus", "effort": null},
        "design_verifier": {"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "high"}
    });
    assert_eq!(
        save(&roots, &fake, design),
        (200, json!({"settings": {"revision": 1}}))
    );
    assert_eq!(
        fake.listed.get(),
        1,
        "pi is asked for its models once per save"
    );
    assert_eq!(save(&roots, &fake, json!({})).0, 200);
    assert_eq!(
        fake.listed.get(),
        1,
        "a save naming no pi design role does not ask pi"
    );
    let saved = fake.saved.into_inner();
    assert_eq!(
        saved[0].design_preparer,
        Some(agent("pi", "openai/gpt-6.1-sol", Some("medium")))
    );
    assert_eq!(
        saved[0].design_reviewer,
        Some(agent("claude", "opus", None))
    );
    assert_eq!(
        saved[0].design_verifier,
        Some(agent("pi", "openai/gpt-6.1-sol", Some("high")))
    );
    assert_eq!(
        (
            saved[1].design_preparer.clone(),
            saved[1].design_reviewer.clone(),
            saved[1].design_verifier.clone()
        ),
        (None, None, None)
    );
}

#[test]
fn a_design_role_that_is_not_a_design_agent_is_refused_with_why_and_nothing_is_saved() {
    let roots = roots("refused");
    let fake = fake();
    let refusals = [
        (
            json!({"design_reviewer": {"harness": "pi", "model": "kimi/kimi-text"}}),
            "kimi/kimi-text doesn't accept images, which the Design reviewer needs",
        ),
        (
            json!({"design_verifier": {"harness": "pi", "model": "openai/gpt-7", "effort": "high"}}),
            "pi doesn't list openai/gpt-7",
        ),
        (
            json!({"design_preparer": {"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": "turbo"}}),
            "turbo is not a thinking level pi takes",
        ),
        (
            json!({"design_reviewer": {"harness": "codex", "model": "gpt-5"}}),
            "The Design reviewer must use pi or claude, which Orq runs read-only for review",
        ),
    ];
    for (design, why) in refusals {
        assert_eq!(
            save(&roots, &fake, design.clone()),
            (422, json!({"error": why})),
            "{design}"
        );
    }
    assert_eq!(
        save(&roots, &fake, json!({"design_reviewer": "pi"})).0,
        400,
        "a role is null or an agent"
    );
    assert!(fake.saved.into_inner().is_empty(), "nothing was saved");
}
