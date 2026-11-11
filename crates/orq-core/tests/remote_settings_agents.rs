//! GET /api/settings/options lists the harnesses and the models each allows (the popup's list), and
//! POST /api/settings/agents sets the implementer's and reviewer's harness and model from that list only,
//! while remote actions are on and the settings version presented is current; each attempt is audited
//! as settings.agents.

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, public_settings, ProjectsProvider, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use serde_json::{json, Value};

struct Fake {
    config: std::path::PathBuf,
    options: Result<Option<Value>, String>,
}

impl ProjectsProvider for Fake {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }

    fn settings(&self) -> Result<Option<Value>, String> {
        public_settings(&self.config).map(Some)
    }

    fn setting_options(&self) -> Result<Option<Value>, String> {
        self.options.clone()
    }
}

fn options() -> Value {
    json!([{"harness": "claude", "models": ["opus", "sonnet"]}, {"harness": "pi", "models": ["gpt-5"]}])
}

fn setup(tag: &str, config: &str) -> (StorageRoots, Fake) {
    let base = std::env::temp_dir().join(format!("orq-settingsagents-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), config).unwrap();
    let fake = Fake { config: roots.config_file(), options: Ok(Some(options())) };
    (roots, fake)
}

fn send(roots: &StorageRoots, fake: &Fake, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> (u16, Value) {
    let authorization = token.map(|token| format!("Bearer {token}"));
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: authorization.as_deref() };
    let reply = handle_with_body(roots, &RemoteConfig { token: "tok".into() }, "p", "p", &request, body, fake);
    (reply.status, reply.body)
}

fn version(roots: &StorageRoots, fake: &Fake) -> String {
    let (status, body) = send(roots, fake, "GET", "/api/settings", Some("tok"), b"");
    assert_eq!(status, 200, "{body}");
    body["settings"]["version"].as_str().unwrap().to_string()
}

fn save(roots: &StorageRoots, fake: &Fake, body: Value) -> (u16, Value) {
    send(roots, fake, "POST", AGENTS, Some("tok"), body.to_string().as_bytes())
}

fn agent_lines(roots: &StorageRoots) -> Vec<Value> {
    remote_audit::read(roots, 100).unwrap().into_iter().filter(|line| line["action"] == "settings.agents").collect()
}

const AGENTS: &str = "/api/settings/agents";
const OPTIONS: &str = "/api/settings/options";
const ON: &str = "remote_actions = true\n";

#[test]
fn the_options_are_the_providers_list_for_a_signed_in_read_only() {
    let (roots, fake) = setup("options", ON);
    assert_eq!(send(&roots, &fake, "GET", OPTIONS, Some("tok"), b""), (200, json!({"options": options()})));
    assert_eq!(send(&roots, &fake, "GET", OPTIONS, None, b"").0, 401);
    assert_eq!(send(&roots, &fake, "POST", OPTIONS, Some("tok"), b"").0, 405);

    let none = Fake { config: roots.config_file(), options: Ok(None) };
    assert_eq!(send(&roots, &none, "GET", OPTIONS, Some("tok"), b"").0, 404, "a provider without options has none to give");
}

#[test]
fn saving_one_role_writes_its_harness_and_model_drops_its_profile_and_keeps_everything_else() {
    let config = "# mine\nremote_actions = true\n\n[implementer]\nprofile = \"fast\"\nharness = \"pi\"\nmodel = \"gpt-5\"\neffort = \"high\"\n\n[reviewer]\nharness = \"pi\"\nmodel = \"gpt-5\"\n";
    let (roots, fake) = setup("one", config);

    let (status, reply) = save(&roots, &fake, json!({"implementer": {"harness": "claude", "model": "sonnet"}, "version": version(&roots, &fake)}));
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["settings"]["implementer"], json!({"harness": "claude", "model": "sonnet", "effort": "high"}));
    assert_eq!(reply["settings"]["reviewer"], json!({"harness": "pi", "model": "gpt-5", "effort": null}), "a role left out is not touched");
    assert_eq!(reply["settings"]["version"].as_str(), Some(version(&roots, &fake).as_str()), "the reply carries the new version");

    let written = std::fs::read_to_string(roots.config_file()).unwrap();
    assert!(written.starts_with("# mine\nremote_actions = true\n"), "{written}");
    assert!(!written.contains("profile"), "{written}");
    assert!(written.contains("[reviewer]\nharness = \"pi\"\nmodel = \"gpt-5\"\n"), "{written}");

    let lines = agent_lines(&roots);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((&lines[0]["outcome"], &lines[0]["project"]), (&json!("allowed"), &json!("settings")));
}

#[test]
fn both_roles_can_be_set_in_one_save_even_when_neither_was_set_before() {
    let (roots, fake) = setup("both", ON);
    let body = json!({"implementer": {"harness": "pi", "model": "gpt-5"}, "reviewer": {"harness": "claude", "model": "opus"}, "version": version(&roots, &fake)});
    let (status, reply) = save(&roots, &fake, body);
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["settings"]["implementer"], json!({"harness": "pi", "model": "gpt-5", "effort": null}));
    assert_eq!(reply["settings"]["reviewer"], json!({"harness": "claude", "model": "opus", "effort": null}));

    // A role sent as null stays as it is.
    let body = json!({"implementer": null, "reviewer": {"harness": "claude", "model": "sonnet"}, "version": version(&roots, &fake)});
    let (status, reply) = save(&roots, &fake, body);
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["settings"]["implementer"]["harness"], "pi");
    assert_eq!(reply["settings"]["reviewer"]["model"], "sonnet");
}

#[test]
fn a_harness_or_model_outside_the_options_is_refused_with_422_and_nothing_is_written() {
    let (roots, fake) = setup("outside", ON);
    let current = version(&roots, &fake);
    let outside = [
        json!({"implementer": {"harness": "codex", "model": "opus"}, "version": current}),
        json!({"implementer": {"harness": "claude", "model": "gpt-5"}, "version": current}),
        json!({"reviewer": {"harness": "pi", "model": "opus"}, "version": current}),
        json!({"implementer": {"harness": "claude", "model": "opus"}, "reviewer": {"harness": "claude", "model": "haiku"}, "version": current}),
        json!({"implementer": {"harness": "claude", "model": " opus"}, "version": current}),
    ];
    for body in &outside {
        let (status, reply) = save(&roots, &fake, body.clone());
        assert_eq!(status, 422, "{body}: {reply}");
        assert_eq!(reply, json!({"error": "That harness or model is not one this machine offers"}), "{body}");
    }
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), ON, "nothing refused was written");

    let none = Fake { config: roots.config_file(), options: Ok(None) };
    let (status, _) = save(&roots, &none, json!({"implementer": {"harness": "claude", "model": "opus"}, "version": current}));
    assert_eq!(status, 404, "without options nothing can be checked, so nothing is saved");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), ON);

    let lines = agent_lines(&roots);
    assert_eq!(lines.len(), outside.len() + 1, "{lines:?}");
    assert!(lines.iter().all(|line| line["outcome"] == "refused"), "{lines:?}");
}

#[test]
fn a_stale_version_is_refused_with_409_and_nothing_is_written() {
    let (roots, fake) = setup("stale", ON);
    let stale = version(&roots, &fake);
    std::fs::write(roots.config_file(), "remote_actions = true\n# edited by hand\n").unwrap();

    let (status, reply) = save(&roots, &fake, json!({"implementer": {"harness": "claude", "model": "opus"}, "version": stale}));
    assert_eq!((status, reply), (409, json!({"error": "Settings changed since you opened them"})));
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), "remote_actions = true\n# edited by hand\n");
    let lines = agent_lines(&roots);
    assert_eq!((&lines[0]["outcome"], &lines[0]["reason"]), (&json!("refused"), &json!("settings changed")));
}

#[test]
fn with_remote_actions_off_the_save_is_not_found_and_nothing_is_written() {
    let (roots, fake) = setup("off", "remote_actions = false\n");
    let (status, reply) = save(&roots, &fake, json!({"implementer": {"harness": "claude", "model": "opus"}, "version": version(&roots, &fake)}));
    assert_eq!((status, reply), (404, json!({"error": "not found"})));
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), "remote_actions = false\n");
    let lines = agent_lines(&roots);
    assert_eq!((&lines[0]["outcome"], &lines[0]["reason"]), (&json!("refused"), &json!("remote actions are off")));
}

#[test]
fn only_a_signed_in_post_with_the_right_body_reaches_the_save() {
    let (roots, fake) = setup("bodies", ON);
    let current = version(&roots, &fake);
    let good = json!({"implementer": {"harness": "claude", "model": "opus"}, "version": current}).to_string();

    assert_eq!(send(&roots, &fake, "POST", AGENTS, None, good.as_bytes()).0, 401);
    for method in ["PUT", "PATCH", "DELETE"] {
        assert_eq!(send(&roots, &fake, method, AGENTS, Some("tok"), good.as_bytes()).0, 405, "{method}");
    }

    let refused = json!({"error": "the request body is not one this action takes"});
    let bad = [
        json!({"implementer": {"harness": "claude", "model": "opus"}}).to_string(),
        json!({"version": current}).to_string(),
        json!({"implementer": null, "reviewer": null, "version": current}).to_string(),
        json!({"implementer": "claude", "version": current}).to_string(),
        json!({"implementer": {"harness": "claude"}, "version": current}).to_string(),
        json!({"implementer": {"harness": "claude", "model": 1}, "version": current}).to_string(),
        json!({"implementer": {"harness": "claude", "model": "opus", "effort": "high"}, "version": current}).to_string(),
        json!({"implementer": {"harness": "claude", "model": "opus"}, "version": current, "remote_actions": false}).to_string(),
        "not json".to_string(),
        String::new(),
    ];
    for body in &bad {
        assert_eq!(send(&roots, &fake, "POST", AGENTS, Some("tok"), body.as_bytes()), (400, refused.clone()), "{body}");
    }
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), ON, "nothing refused was written");
    let lines = agent_lines(&roots);
    assert_eq!(lines.len(), bad.len(), "every refusal after the token check is audited: {lines:?}");
}

#[test]
fn a_role_that_is_not_a_table_in_the_settings_is_refused_and_nothing_is_written() {
    let config = "remote_actions = true\nreviewer = \"pi\"\n";
    let (roots, fake) = setup("not-a-table", config);
    let current = orq_core::remote_settings::version_of(config);

    let body = json!({"implementer": {"harness": "claude", "model": "opus"}, "reviewer": {"harness": "claude", "model": "sonnet"}, "version": current});
    let (status, reply) = save(&roots, &fake, body);
    assert_eq!((status, reply), (500, json!({"error": "the settings could not be written"})));
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), config, "neither role is written, so the save is never partial");
    let lines = agent_lines(&roots);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((&lines[0]["outcome"], &lines[0]["reason"]), (&json!("refused"), &json!("a role in the settings is not a table")));
}

#[test]
fn options_that_cannot_be_read_refuse_the_save_with_500_and_nothing_is_written() {
    let (roots, _) = setup("options-error", ON);
    let failing = Fake { config: roots.config_file(), options: Err("pi did not answer".into()) };
    let current = version(&roots, &failing);

    let (status, reply) = save(&roots, &failing, json!({"implementer": {"harness": "claude", "model": "opus"}, "version": current}));
    assert_eq!((status, &reply), (500, &json!({"error": "the options could not be read"})));
    assert!(!reply.to_string().contains("pi did not answer"), "the provider's reason stays on the machine: {reply}");
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), ON);
    let lines = agent_lines(&roots);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((&lines[0]["outcome"], &lines[0]["reason"]), (&json!("refused"), &json!("the options could not be read")));
}

#[test]
fn a_settings_file_that_cannot_be_read_counts_as_actions_off_and_is_left_alone() {
    let broken = "remote_actions = true\nthis is [not toml";
    let (roots, fake) = setup("broken", broken);
    let current = orq_core::remote_settings::version_of(broken);

    let (status, reply) = save(&roots, &fake, json!({"implementer": {"harness": "claude", "model": "opus"}, "version": current}));
    assert_eq!((status, reply), (404, json!({"error": "not found"})));
    assert_eq!(std::fs::read_to_string(roots.config_file()).unwrap(), broken);
    let lines = agent_lines(&roots);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((&lines[0]["outcome"], &lines[0]["reason"]), (&json!("refused"), &json!("remote actions are off")));
}
