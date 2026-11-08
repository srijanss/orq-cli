//! `/api/settings`: the run settings (which agent implements, which reviews, the named models),
//! read-only, behind the same checks. Only fields known to be harmless leave the server; anything
//! else in the config file, secrets and credential paths included, never does.

mod web_deps;

use std::path::PathBuf;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

fn setup(label: &str, config: Option<&str>) -> (StoreProjects, StorageRoots) {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    if let Some(text) = config {
        std::fs::write(roots.config_file(), text).unwrap();
    }
    let scan: PathBuf = base.join("code");
    (StoreProjects::new(roots.clone(), scan), roots)
}

fn ask(s: &(StoreProjects, StorageRoots), method: &str, authorization: Option<&str>) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path: "/api/settings", peer: "127.0.0.1".parse().unwrap(), authorization };
    let r = handle_with(&s.1, &config, "p", "p", &request, &s.0);
    (r.status, r.body)
}

const CONFIG: &str = r#"
api_key = "sk-TOP-SECRET-1"
[implementer]
harness = "claude"
model = "opus"
effort = "high"
env = { ANTHROPIC_API_KEY = "sk-SECRET-2" }
credentials = "/home/dev/.ssh/id_ed25519"
[reviewer]
harness = "pi"
model = "openai/gpt-6.1-sol"
token = "tok-SECRET-3"
[models.opus]
harness = "claude"
model = "opus"
auth_file = "/home/dev/.config/secret-path"
[remote]
token = "remote-SECRET-4"
"#;

#[test]
fn the_settings_list_the_agents_and_models_and_nothing_else_from_the_config() {
    let s = setup("settings-list", Some(CONFIG));
    let (status, body) = ask(&s, "GET", Some("Bearer tok"));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"], json!({
        "implementer": {"harness": "claude", "model": "opus", "effort": "high"},
        "reviewer": {"harness": "pi", "model": "openai/gpt-6.1-sol", "effort": null},
        "models": [{"name": "opus", "harness": "claude", "model": "opus", "effort": null}]
    }), "{body}");
    let text = body.to_string();
    for leak in ["SECRET", ".ssh", "secret-path", "api_key", "token", "/home/"] {
        assert!(!text.contains(leak), "{leak} leaked: {text}");
    }
}

#[test]
fn a_value_that_looks_like_a_secret_is_redacted_even_in_an_allowed_field() {
    let s = setup("settings-redact", Some("[implementer]\nharness = \"claude\"\nmodel = \"sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789\"\n"));
    let (status, body) = ask(&s, "GET", Some("Bearer tok"));
    assert_eq!(status, 200, "{body}");
    assert!(!body.to_string().contains("abcdefghijklmnopqrstuvwxyz0123456789"), "{body}");
}

#[test]
fn no_config_file_means_empty_settings_and_a_broken_one_is_an_error_naming_no_path() {
    let s = setup("settings-none", None);
    let (status, body) = ask(&s, "GET", Some("Bearer tok"));
    assert_eq!((status, &body["settings"]), (200, &json!({"implementer": null, "reviewer": null, "models": []})), "{body}");

    let s = setup("settings-bad", Some("this is [not toml"));
    let (status, body) = ask(&s, "GET", Some("Bearer tok"));
    assert_eq!(status, 500, "{body}");
    assert!(!body.to_string().contains(s.1.config.to_str().unwrap()), "{body}");
}

#[test]
fn settings_need_the_token_and_only_read() {
    let s = setup("settings-auth", Some(CONFIG));
    assert_eq!(ask(&s, "GET", None).0, 401);
    assert_eq!(ask(&s, "GET", Some("Bearer wrong")).0, 401);
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        assert_eq!(ask(&s, method, Some("Bearer tok")).0, 405, "{method}");
    }
    let (status, body) = ask(&s, "HEAD", Some("Bearer tok"));
    assert_eq!((status, body), (200, Value::Null));
}

#[test]
fn inline_tables_give_the_same_settings_as_standard_tables() {
    let standard = setup("settings-std", Some("[implementer]\nharness = \"claude\"\nmodel = \"opus\"\n[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n"));
    let inline = setup("settings-inline", Some("implementer = { harness = \"claude\", model = \"opus\", token = \"SECRET\" }\n[models]\nopus = { harness = \"claude\", model = \"opus\" }\n"));
    let (a, b) = (ask(&standard, "GET", Some("Bearer tok")), ask(&inline, "GET", Some("Bearer tok")));
    assert_eq!(a, b);
    assert_eq!(b.1["settings"]["implementer"]["model"], "opus", "{b:?}");
    assert_eq!(b.1["settings"]["models"].as_array().unwrap().len(), 1, "{b:?}");
}

#[test]
fn a_config_that_cannot_be_read_is_an_error_not_empty_settings() {
    let dir = setup("settings-dir", None);
    std::fs::create_dir(dir.1.config_file()).unwrap();
    let (status, body) = ask(&dir, "GET", Some("Bearer tok"));
    assert_eq!(status, 500, "{body}");
    assert!(!body.to_string().contains(dir.1.config.to_str().unwrap()), "{body}");
    let bytes = setup("settings-bytes", None);
    std::fs::write(bytes.1.config_file(), [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(ask(&bytes, "GET", Some("Bearer tok")).0, 500);
}
