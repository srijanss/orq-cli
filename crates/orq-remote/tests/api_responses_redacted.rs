//! Every web response leaves out tokens, headers, session transcripts and local credential
//! paths: one fixture holds a secret of each kind and every GET endpoint is read.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const BEARER: &str = "Authorization: Bearer abcdefghijklmnop12345";
const GITHUB: &str = "ghp_abcdefghijklmnopqrstuvwx";

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

#[test]
fn no_endpoint_returns_a_token_a_header_a_transcript_or_a_credential_path() {
    let base = web_deps::private_temp_dir("redacted");
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let scan = base.join("code");
    let alpha = git_repo(&scan, "alpha");
    let id = orq_git::project_id(&orq_git::resolve_project(&alpha).unwrap());
    let store = RunStore::new(&roots.state);
    store
        .create(&RunRecord { run_id: "r1".into(), project_id: id, feature_id: "f1".into(), remote: format!("https://user:{GITHUB}@example.com/x.git"), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: "orq/r1".into(), worktree: roots.config.join("worktree"), repository: String::new(), profiles: Default::default() })
        .unwrap();
    let payload = json!({"header": BEARER, "note": format!("GITHUB_TOKEN={GITHUB}"), "token": "plainsecretvalue987", "path": roots.config.join("remote-token"), "transcript": "session transcript line"});
    let event = Event { schema_version: 1, event_id: "r1-0".into(), run_id: "r1".into(), project_id: String::new(), feature_id: "f1".into(), kind: "run.started".into(), stage: None, attempt: 1, occurred_at: "2026-11-01T10:00:00Z".into(), payload };
    store.append_event(&event).unwrap();
    let logs = roots.state.join("logs/r1");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("gate.log"), format!("{BEARER}\nGITHUB_TOKEN={GITHUB}\n")).unwrap();
    std::fs::write(alpha.join(".tdd-features.json"), json!([{"featureName": "f1", "description": format!("uses {GITHUB}"), "status": "draft"}, {"featureName": "f2", "dependsOn": [format!("after-{GITHUB}")], "status": format!("odd-{GITHUB}"), "recordedAt": format!("at-{GITHUB}")}]).to_string()).unwrap();

    let provider = StoreProjects::new(roots.clone(), scan);
    let config = RemoteConfig { token: "tok".into() };
    let paths = ["/api/projects", "/api/projects/alpha/features", "/api/projects/alpha/runs", "/api/projects/alpha/runs/r1", "/api/projects/alpha/runs/r1/after/0", "/api/projects/alpha/runs/r1/logs", "/api/projects/alpha/runs/r1/logs/gate.log", "/api/settings", "/api/reviews", "/api/projects/alpha/reviews"];
    let forbidden = ["abcdefghijklmnop12345", GITHUB, "plainsecretvalue987", "Bearer a", "remote-token", "passkeys", &roots.config.display().to_string()];
    for path in paths {
        let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let reply = handle_with(&roots, &config, "server-project", "server", &request, &provider);
        let text: String = serde_json::to_string::<Value>(&reply.body).unwrap();
        for secret in &forbidden {
            assert!(!text.contains(secret), "{path} leaks {secret}: {text}");
        }
    }
}
