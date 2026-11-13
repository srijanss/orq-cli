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
    assert_eq!(send(&s, "/api/projects/alpha/launch-profiles"), (200, json!({"profiles": ["sonnet"], "implementer": null, "reviewer": null, "project_default": false})));

    std::fs::write(alpha.join(".orq.toml"), "[models.broken]\nharness = 1\n").unwrap();
    assert_eq!(send(&s, "/api/projects/alpha/launch-profiles"), (500, json!({"error": "the launch profiles could not be read"})));
}

type Agent<'a> = Option<(&'a str, &'a str, Option<&'a str>)>;

fn save_agents(s: &Setup, repo: &Path, implementer: Agent, reviewer: Agent) {
    use orq_core::project_settings::{ProjectAgent, ProjectSettings};
    let agent = |chosen: Agent| chosen.map(|(harness, model, effort)| ProjectAgent { harness: harness.into(), model: model.into(), effort: effort.map(Into::into) });
    let settings = ProjectSettings { implementer: agent(implementer), reviewer: agent(reviewer), ..Default::default() };
    let mut store = orq_core::store::Store::open(&s.roots.database()).unwrap();
    let root = std::fs::canonicalize(repo).unwrap();
    let revision = store.project_settings(&root).unwrap().revision;
    store.save_project_settings(&root, revision, &settings).unwrap();
}

/// The project's own agents come before the machine's: each role preselects the profile with the
/// project's harness and model, and a role whose agent no profile has preselects none and offers
/// Project default, a start with no profiles named.
#[test]
fn the_projects_agents_are_preselected_and_one_no_profile_has_offers_project_default() {
    let s = setup("launch-profiles-project", PROFILES);
    let alpha = git_repo(&s.scan, "alpha");
    assert_eq!(send(&s, "/api/projects/alpha/launch-profiles").1["project_default"], json!(false), "no project settings: nothing to offer");

    save_agents(&s, &alpha, Some(("claude", "sonnet", None)), Some(("pi", "kimi", None)));
    let (status, body) = send(&s, "/api/projects/alpha/launch-profiles");
    assert_eq!(status, 200, "{body}");
    assert_eq!((&body["implementer"], &body["reviewer"], &body["project_default"]), (&json!("sonnet"), &json!("kimi"), &json!(false)), "{body}");

    save_agents(&s, &alpha, Some(("pi", "glm", None)), None);
    let (_, body) = send(&s, "/api/projects/alpha/launch-profiles");
    assert_eq!((&body["implementer"], &body["reviewer"], &body["project_default"]), (&json!(null), &json!(null), &json!(true)), "{body}");

    save_agents(&s, &alpha, None, Some(("pi", "glm", None)));
    let (_, body) = send(&s, "/api/projects/alpha/launch-profiles");
    assert_eq!((&body["implementer"], &body["reviewer"], &body["project_default"]), (&json!("opus"), &json!(null), &json!(true)), "{body}");
}

/// A profile stands for the project's agent only with the agent's effort as its thinking level too, since
/// starting with the profile runs at the profile's level: otherwise the role preselects none and Project
/// default is offered, so the project's effort is kept.
#[test]
fn a_profile_with_another_thinking_level_is_not_the_projects_agent() {
    let config = format!("{PROFILES}\n[models.deep]\nharness = \"pi\"\nmodel = \"kimi\"\nthinking = \"high\"\n");
    let s = setup("launch-profiles-effort", &config);
    let alpha = git_repo(&s.scan, "alpha");

    save_agents(&s, &alpha, Some(("claude", "sonnet", Some("high"))), Some(("pi", "kimi", Some("high"))));
    let (status, body) = send(&s, "/api/projects/alpha/launch-profiles");
    assert_eq!(status, 200, "{body}");
    assert_eq!((&body["implementer"], &body["reviewer"], &body["project_default"]), (&json!(null), &json!("deep"), &json!(true)), "{body}");

    save_agents(&s, &alpha, None, Some(("pi", "kimi", None)));
    let (_, body) = send(&s, "/api/projects/alpha/launch-profiles");
    assert_eq!((&body["reviewer"], &body["project_default"]), (&json!("kimi"), &json!(false)), "no effort is the profile without a thinking level: {body}");
}

/// The project's own agents come from orq.db: one that cannot be read fails the read, saying only that it
/// failed, since the reason can name a path on this machine.
#[test]
fn an_unreadable_database_fails_the_read_without_its_reason() {
    let s = setup("launch-profiles-bad-db", PROFILES);
    git_repo(&s.scan, "alpha");
    std::fs::write(s.roots.database(), "not a database, just some text that is long enough to be a header").unwrap();
    assert_eq!(send(&s, "/api/projects/alpha/launch-profiles"), (500, json!({"error": "the launch profiles could not be read"})));
}
