//! `/api/projects/{id}/settings`: a project's own settings in this machine's orq.db, for the web's
//! project Settings page. Each role is what the project chose, or unset with what a run would use
//! instead and where that comes from; then the delivery mode and the revision a save must name.
//! Only names leave the server, redacted like every other answer.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::project_settings::{Delivery, ProjectAgent, ProjectSettings, SaveOutcome};
use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

const MACHINE: &str = "[models.sonnet]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[implementer]\nharness = \"claude\"\nmodel = \"opus\"\neffort = \"high\"\n\n[reviewer]\nharness = \"pi\"\nmodel = \"openai/gpt-6.1-sol\"\n";

fn setup(label: &str, config: Option<&str>) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    if let Some(text) = config {
        std::fs::write(roots.config_file(), text).unwrap();
    }
    Setup { scan: base.join("code"), roots }
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "start"]] {
        let out = Command::new("git").args(args).current_dir(&path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    std::fs::canonicalize(path).unwrap()
}

fn save(s: &Setup, repo: &Path, settings: &ProjectSettings) -> u64 {
    let mut store = Store::open(&s.roots.database()).unwrap();
    let revision = store.project_settings(repo).unwrap().revision;
    match store.save_project_settings(repo, revision, settings).unwrap() {
        SaveOutcome::Saved { revision } => revision,
        SaveOutcome::Conflict => panic!("no one else saves here"),
    }
}

fn agent(harness: &str, model: &str, effort: Option<&str>) -> ProjectAgent {
    ProjectAgent { harness: harness.to_string(), model: model.to_string(), effort: effort.map(str::to_string) }
}

fn send(s: &Setup, path: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

#[test]
fn a_saved_role_and_delivery_are_given_with_the_revision_and_an_unset_role_says_what_runs_use_instead() {
    let s = setup("project-settings-saved", Some(MACHINE));
    let alpha = git_repo(&s.scan, "alpha");
    std::fs::write(alpha.join(".tdd-config.json"), r#"{"reviewers":{"default":{"reviewer":"pi","model":"kimi"}}}"#).unwrap();
    let revision = save(&s, &alpha, &ProjectSettings { implementer: Some(agent("claude", "sonnet", Some("medium"))), reviewer: None, delivery: Delivery::MergeLocal });

    let (status, body) = send(&s, "/api/projects/alpha/settings");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"settings": {
        "implementer": {
            "project": {"harness": "claude", "model": "sonnet", "effort": "medium"},
            "fallback": {"harness": "claude", "model": "opus", "effort": "high", "source": "machine_default"}
        },
        "reviewer": {
            "project": null,
            "fallback": {"harness": "pi", "model": "kimi", "effort": null, "source": "tdd_config"}
        },
        "delivery": "merge_local",
        "revision": revision
    }}));
}

#[test]
fn a_project_with_no_record_has_the_defaults_at_revision_0_and_each_fallback_names_its_source() {
    // No orq.db at all, a [roles] implementer, no machine reviewer.
    let s = setup("project-settings-none", Some("[models.sonnet]\nharness = \"claude\"\nmodel = \"sonnet\"\n\n[roles]\nimplementer = \"sonnet\"\n"));
    git_repo(&s.scan, "alpha");
    assert!(!s.roots.database().exists());

    let (status, body) = send(&s, "/api/projects/alpha/settings");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["implementer"], json!({"project": null, "fallback": {"harness": "claude", "model": "sonnet", "effort": null, "source": "roles"}}));
    assert_eq!(body["settings"]["reviewer"]["fallback"]["source"], json!("built_in"), "{body}");
    assert_eq!((&body["settings"]["delivery"], &body["settings"]["revision"]), (&json!("review"), &json!(0)));
    assert!(!s.roots.database().exists(), "reading never creates orq.db");
}

#[test]
fn an_unknown_project_is_not_found_and_a_secret_looking_value_is_redacted() {
    let s = setup("project-settings-redact", None);
    let alpha = git_repo(&s.scan, "alpha");
    save(&s, &alpha, &ProjectSettings { implementer: Some(agent("claude", "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789", None)), reviewer: None, delivery: Delivery::Review });

    assert_eq!(send(&s, "/api/projects/nope/settings").0, 404);
    let (status, body) = send(&s, "/api/projects/alpha/settings");
    assert_eq!(status, 200, "{body}");
    assert!(!body.to_string().contains("abcdefghijklmnopqrstuvwxyz0123456789"), "{body}");
}

#[test]
fn a_fallback_that_cannot_be_worked_out_is_null_and_the_saved_settings_are_still_given() {
    // A half-filled machine [implementer] that a run with the project's own implementer never reads.
    let s = setup("project-settings-broken-fallback", Some("[implementer]\nharness = \"claude\"\n"));
    let alpha = git_repo(&s.scan, "alpha");
    let revision = save(&s, &alpha, &ProjectSettings { implementer: Some(agent("claude", "sonnet", None)), reviewer: None, delivery: Delivery::Push });

    let (status, body) = send(&s, "/api/projects/alpha/settings");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["implementer"], json!({"project": {"harness": "claude", "model": "sonnet", "effort": null}, "fallback": null}));
    assert_eq!(body["settings"]["reviewer"]["fallback"]["source"], json!("built_in"), "{body}");
    assert_eq!((&body["settings"]["delivery"], &body["settings"]["revision"]), (&json!("push"), &json!(revision)));

    // Profiles that cannot be read leave both fallbacks unknown, not the page.
    std::fs::write(alpha.join(".orq.toml"), "[models.broken]\nharness = 1\n").unwrap();
    let (status, body) = send(&s, "/api/projects/alpha/settings");
    assert_eq!(status, 200, "{body}");
    assert_eq!((&body["settings"]["implementer"]["fallback"], &body["settings"]["reviewer"]["fallback"]), (&Value::Null, &Value::Null));
    assert_eq!(body["settings"]["revision"], json!(revision));
}

#[test]
fn an_unreadable_orq_db_is_a_failure_that_names_no_path() {
    let s = setup("project-settings-broken-db", Some(MACHINE));
    git_repo(&s.scan, "alpha");
    std::fs::write(s.roots.database(), "not a database").unwrap();

    assert_eq!(send(&s, "/api/projects/alpha/settings"), (500, json!({"error": "the project's settings could not be read"})));
}

#[test]
fn a_linked_worktree_reads_the_record_of_its_main_checkout() {
    let s = setup("project-settings-worktree", Some(MACHINE));
    let alpha = git_repo(&s.scan, "alpha");
    let linked = s.scan.join("alpha-wt");
    let out = Command::new("git").args(["worktree", "add", "-q", "-b", "side"]).arg(&linked).current_dir(&alpha).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let revision = save(&s, &alpha, &ProjectSettings { implementer: Some(agent("claude", "sonnet", None)), reviewer: None, delivery: Delivery::MergeLocal });

    let (status, body) = send(&s, "/api/projects/alpha-wt/settings");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["implementer"]["project"], json!({"harness": "claude", "model": "sonnet", "effort": null}), "{body}");
    assert_eq!((&body["settings"]["delivery"], &body["settings"]["revision"]), (&json!("merge_local"), &json!(revision)));
    let worktree = std::fs::canonicalize(&linked).unwrap();
    assert_eq!(Store::open(&s.roots.database()).unwrap().project_settings(&worktree).unwrap().revision, 0, "no record of its own was made");
}
