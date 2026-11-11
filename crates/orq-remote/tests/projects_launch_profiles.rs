//! The web's pickers offer the model profiles the popup's launch picker offers: those of this machine's
//! config and the project's `.orq.toml`, with the implementer and reviewer the popup preselects.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

const PROFILES: &str = "remote_actions = true\n\n[models.sonnet]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[models.opus]\nharness = \"claude\"\nmodel = \"opus\"\n\n[models.kimi]\nharness = \"pi\"\nmodel = \"kimi\"\n\n[implementer]\nharness = \"claude\"\nmodel = \"opus\"\n";

fn setup(label: &str, config: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), config).unwrap();
    Setup { scan: base.join("code"), roots }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    path
}

fn send(s: &Setup, path: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

#[test]
fn the_profiles_of_this_machine_and_the_project_are_listed_with_the_popups_defaults() {
    let s = setup("launch-profiles", PROFILES);
    let alpha = git_repo(&s.scan, "alpha");
    std::fs::write(alpha.join(".orq.toml"), "[models.astra]\nharness = \"pi\"\nmodel = \"openai/astra\"\n").unwrap();
    std::fs::write(alpha.join(".tdd-config.json"), r#"{"reviewers":{"default":{"reviewer":"pi","model":"kimi"}}}"#).unwrap();

    let (status, body) = send(&s, "/api/projects/alpha/launch-profiles");
    assert_eq!(status, 200, "{body}");
    let mut names: Vec<String> = body["profiles"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
    names.sort();
    assert_eq!(names, ["astra", "kimi", "opus", "sonnet"]);
    assert_eq!((&body["implementer"], &body["reviewer"]), (&json!("opus"), &json!("kimi")), "{body}");
    assert_eq!(send(&s, "/api/projects/nope/launch-profiles").0, 404);
}

#[test]
fn a_project_with_no_defaults_has_none_and_an_unreadable_profile_file_is_a_failure() {
    let s = setup("launch-profiles-none", "remote_actions = true\n\n[models.sonnet]\nharness = \"claude\"\nmodel = \"sonnet\"\n");
    let alpha = git_repo(&s.scan, "alpha");
    assert_eq!(send(&s, "/api/projects/alpha/launch-profiles"), (200, json!({"profiles": ["sonnet"], "implementer": null, "reviewer": null})));

    std::fs::write(alpha.join(".orq.toml"), "[models.broken]\nharness = 1\n").unwrap();
    assert_eq!(send(&s, "/api/projects/alpha/launch-profiles"), (500, json!({"error": "the launch profiles could not be read"})));
}
