//! `POST /api/projects/{id}/settings` saves a project's own agents and delivery in this machine's
//! orq.db, keyed by its main checkout, for the web's project Settings page. The body names the whole
//! record (`implementer`, `reviewer`, `delivery`) and the `revision` it was read at; a null role is
//! reset, so a run falls back again. The answer is the settings as read back.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use orq_core::remote_audit;
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
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

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let options: Arc<orq_remote::projects::AgentOptionsSource> = Arc::new(|| vec![("claude".to_string(), vec!["opus".to_string(), "sonnet".to_string()]), ("pi".to_string(), vec!["kimi".to_string()])]);
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone()).with_agent_options(options);
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &provider);
    (r.status, r.body)
}

fn save(s: &Setup, id: &str, body: Value) -> (u16, Value) {
    send(s, "POST", &format!("/api/projects/{id}/settings"), body.to_string().as_bytes())
}

#[test]
fn a_save_sets_the_agents_and_delivery_answers_the_settings_read_back_and_is_audited() {
    let s = setup("project-settings-save");
    let alpha = git_repo(&s.scan, "alpha");

    let (status, body) = save(&s, "alpha", json!({
        "implementer": {"harness": "claude", "model": "sonnet", "effort": "high"},
        "reviewer": null,
        "delivery": "merge_local",
        "revision": 0
    }));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["implementer"]["project"], json!({"harness": "claude", "model": "sonnet", "effort": "high"}));
    assert_eq!(body["settings"]["reviewer"]["project"], Value::Null);
    assert_eq!((&body["settings"]["delivery"], &body["settings"]["revision"]), (&json!("merge_local"), &json!(1)));
    assert_eq!(send(&s, "GET", "/api/projects/alpha/settings", b"").1, body, "the answer is what a read gives");

    let stored = Store::open(&s.roots.database()).unwrap().project_settings(&alpha).unwrap();
    assert_eq!(stored.revision, 1);
    let lines = remote_audit::read(&s.roots, 20).unwrap();
    assert_eq!(lines.iter().map(|line| (line["action"].clone(), line["project"].clone(), line["outcome"].clone())).collect::<Vec<_>>(), vec![(json!("project.settings"), json!("alpha"), json!("allowed"))]);
}

#[test]
fn a_reset_role_falls_back_again_a_stale_save_changes_nothing_and_a_linked_worktree_saves_its_main_checkout_s_record() {
    let s = setup("project-settings-save-reset");
    let alpha = git_repo(&s.scan, "alpha");
    let linked = s.scan.join("alpha-wt");
    let out = Command::new("git").args(["worktree", "add", "-q", "-b", "side"]).arg(&linked).current_dir(&alpha).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let chosen = json!({"harness": "pi", "model": "kimi", "effort": "low"});

    let (status, body) = save(&s, "alpha-wt", json!({"implementer": null, "reviewer": chosen, "delivery": "push", "revision": 0}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(send(&s, "GET", "/api/projects/alpha/settings", b"").1, body, "the worktree saved its main checkout's record");

    let (status, body) = save(&s, "alpha", json!({"implementer": null, "reviewer": null, "delivery": "push", "revision": 1}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["settings"]["reviewer"]["project"], Value::Null);
    assert_eq!(body["settings"]["reviewer"]["fallback"]["source"], json!("built_in"), "{body}");

    let stale = json!({"implementer": null, "reviewer": chosen, "delivery": "review", "revision": 1});
    assert_eq!(save(&s, "alpha", stale), (409, json!({"error": "The project's settings changed since they were read"})));
    assert_eq!(send(&s, "GET", "/api/projects/alpha/settings", b"").1, body, "nothing was written");
    assert_eq!(save(&s, "nope", json!({"implementer": null, "reviewer": null, "delivery": "push", "revision": 0})).0, 404);
}
