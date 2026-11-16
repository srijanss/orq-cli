//! The kept worktrees of every project the server scans, for the web's Storage card: listed grouped by
//! project, deleted one at a time or all the merged ones at once, behind the same gates as a storage prune.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use orq_core::passkey_register::PasskeyHost;
use orq_core::passkey_routes::PasskeyAuth;
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use orq_remote::{Server, ServerState};
use serde_json::{json, Value};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct World {
    roots: StorageRoots,
    code: PathBuf,
}

fn world(tag: &str) -> World {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-kept-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let code = base.join("code");
    std::fs::create_dir_all(&code).unwrap();
    World { roots, code: code.canonicalize().unwrap() }
}

impl World {
    fn project(&self, name: &str) -> PathBuf {
        let repo = self.code.join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), ".claude/settings.local.json\n.claude/worktrees/\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        repo
    }

    /// A worktree on `branch`, recorded as `run`, that cleanup kept; `commit` puts a commit on it.
    fn kept(&self, repo: &Path, run: &str, branch: &str, commit: bool) -> PathBuf {
        let path = repo.join(".claude/worktrees").join(branch);
        git(repo, &["worktree", "add", "-q", "-b", branch, path.to_str().unwrap()]);
        if commit {
            std::fs::write(path.join("b.txt"), "b\n").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "-q", "-m", "b"]);
        }
        let store = RunStore::new(&self.roots.state);
        let record = RunRecord {
            delivery: Default::default(),
            run_id: run.into(),
            project_id: format!("key-{run}"),
            feature_id: branch.trim_start_matches("fix--").replace('/', "-"),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: git(repo, &["rev-parse", "main"]),
            branch: branch.into(),
            worktree: path.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        };
        store.create(&record).unwrap();
        store
            .append_event(&Event {
                schema_version: 1,
                event_id: format!("{run}-1"),
                run_id: run.into(),
                project_id: record.project_id.clone(),
                feature_id: record.feature_id.clone(),
                kind: "worktree.kept".into(),
                stage: Some("cleanup".into()),
                attempt: 1,
                occurred_at: "2026-11-16T00:00:00Z".into(),
                payload: json!({ "worktree": path, "reason": "kept", "run_succeeded": true }),
            })
            .unwrap();
        path
    }

    fn provider(&self) -> StoreProjects {
        StoreProjects::new(self.roots.clone(), self.code.clone())
    }

    fn send(&self, provider: &StoreProjects, method: &str, path: &str, body: &str) -> (u16, Value) {
        let config = RemoteConfig { token: "tok".into() };
        let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
        let r = handle_with_body(&self.roots, &config, "p", "p", &request, body.as_bytes(), provider);
        (r.status, r.body)
    }

    fn listed(&self, provider: &StoreProjects) -> Value {
        let (status, body) = self.send(provider, "GET", "/api/storage/worktrees", "");
        assert_eq!(status, 200, "{body}");
        body
    }
}

fn version_of(listed: &Value, project: &str, branch: &str) -> String {
    let group = listed["projects"].as_array().unwrap().iter().find(|g| g["id"] == project).unwrap();
    let entry = group["worktrees"].as_array().unwrap().iter().find(|w| w["branch"] == branch).unwrap();
    entry["version"].as_str().unwrap().to_string()
}

#[test]
fn the_kept_worktrees_of_two_projects_are_listed_grouped_with_their_totals() {
    let w = world("list");
    let alpha = w.project("alpha");
    let beta = w.project("beta");
    w.kept(&alpha, "run-1", "fix--A", false);
    w.kept(&beta, "run-2", "fix--B", true);
    let provider = w.provider();

    let listed = w.listed(&provider);

    assert_eq!(listed["count"], 2, "{listed}");
    assert!(listed["bytes"].as_u64().unwrap() > 0);
    assert!(listed["size"].as_str().is_some());
    let groups = listed["projects"].as_array().unwrap();
    let names: Vec<&str> = groups.iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    let a = &groups[0]["worktrees"][0];
    assert_eq!(a["branch"], "fix--A");
    assert_eq!(a["run"], "run-1");
    assert_eq!(a["ending"], "completed");
    assert_eq!(a["reason"], "kept");
    assert_eq!(a["commits"], 0);
    assert_eq!(a["merged"], true);
    assert_eq!(a["safe"], true);
    assert!(a["size"].as_str().is_some());
    let b = &groups[1]["worktrees"][0];
    assert_eq!(b["commits"], 1);
    assert_eq!(b["safe"], false);
    assert!(!listed.to_string().contains(w.code.to_str().unwrap()), "no path on this machine is shown: {listed}");
}

#[test]
fn a_delete_removes_one_worktree_and_a_stale_or_unsafe_one_is_refused() {
    let w = world("delete");
    let alpha = w.project("alpha");
    let merged = w.kept(&alpha, "run-1", "fix--A", false);
    let ahead = w.kept(&alpha, "run-2", "fix--B", true);
    let provider = w.provider();
    let listed = w.listed(&provider);

    let (status, body) = w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--B/delete", &json!({ "version": version_of(&listed, "alpha", "fix--B") }).to_string());
    assert_eq!(status, 409, "{body}");
    assert!(ahead.exists());
    let (status, body) = w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--A/delete", &json!({ "version": "stale" }).to_string());
    assert_eq!((status, body["error"].as_str()), (409, Some("the worktree changed since you opened it; nothing was deleted")));
    assert!(merged.exists());

    let (status, body) = w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--A/delete", &json!({ "version": version_of(&listed, "alpha", "fix--A") }).to_string());
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["branch"], "fix--A");
    assert!(!merged.exists());
    let (status, _) = w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--B/delete", &json!({ "version": version_of(&listed, "alpha", "fix--B"), "force": true }).to_string());
    assert_eq!(status, 200);
    assert!(!ahead.exists());
    assert_eq!(w.listed(&provider)["count"], 0);
}

#[test]
fn delete_merged_removes_only_the_safe_ones_and_reports_each() {
    let w = world("merged");
    let alpha = w.project("alpha");
    let beta = w.project("beta");
    let one = w.kept(&alpha, "run-1", "fix--A", false);
    let two = w.kept(&beta, "run-2", "fix--C", false);
    let ahead = w.kept(&beta, "run-3", "fix--B", true);
    let provider = w.provider();
    let listed = w.listed(&provider);
    let item = |project: &str, branch: &str| json!({ "project": project, "branch": branch, "version": version_of(&listed, project, branch) });

    let body = json!({ "worktrees": [item("alpha", "fix--A"), item("beta", "fix--C"), item("beta", "fix--B")] }).to_string();
    let (status, answer) = w.send(&provider, "POST", "/api/storage/worktrees/delete-merged", &body);

    assert_eq!(status, 200, "{answer}");
    let results = answer["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["deleted"], true);
    assert_eq!(results[1]["deleted"], true);
    assert_eq!(results[2]["deleted"], false);
    assert!(results[2]["error"].as_str().is_some());
    assert!(!one.exists() && !two.exists() && ahead.exists());
}

#[test]
fn an_unknown_project_or_branch_is_not_found_and_actions_off_hide_it() {
    let w = world("unknown");
    let alpha = w.project("alpha");
    w.kept(&alpha, "run-1", "fix--A", false);
    let provider = w.provider();
    let body = json!({ "version": "v" }).to_string();
    assert_eq!(w.send(&provider, "POST", "/api/storage/worktrees/nope/fix--A/delete", &body).0, 404);
    assert_eq!(w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--Nope/delete", &body).0, 404);
    assert_eq!(w.send(&provider, "POST", "/api/storage/worktrees/alpha/delete", &body).0, 404);
    assert_eq!(w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--A/delete", r#"{"other":1}"#).0, 400);

    std::fs::write(w.roots.config_file(), "remote_actions = false\n").unwrap();
    assert_eq!(w.send(&provider, "GET", "/api/storage/worktrees", "").0, 404);
    assert_eq!(w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--A/delete", &body).0, 404);
}

#[test]
fn a_branch_with_a_slash_is_the_path_after_the_project() {
    let w = world("slash");
    let alpha = w.project("alpha");
    let path = w.kept(&alpha, "run-1", "fix--team/A", false);
    let provider = w.provider();
    let version = version_of(&w.listed(&provider), "alpha", "fix--team/A");
    let (status, body) = w.send(&provider, "POST", "/api/storage/worktrees/alpha/fix--team/A/delete", &json!({ "version": version }).to_string());
    assert_eq!(status, 200, "{body}");
    assert!(!path.exists());
}

#[test]
fn a_delete_without_the_session_csrf_token_is_refused_before_anything_is_deleted() {
    let w = world("csrf");
    let alpha = w.project("alpha");
    let path = w.kept(&alpha, "run-1", "fix--A", false);
    let token = "kept-worktrees-test-token-0123456789";
    let state = Arc::new(ServerState { roots: w.roots.clone(), config: RemoteConfig { token: token.into() }, project_id: "p".into(), project_name: "p".into() });
    let auth = Arc::new(PasskeyAuth::new(PasskeyHost::from_setting("box.tail1234.ts.net").unwrap()));
    let server = Server::start_with_auth(TcpListener::bind("127.0.0.1:0").unwrap(), state, Arc::new(w.provider()), auth).unwrap();

    let body = br#"{"version":"v","force":true}"#;
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    let head = format!(
        "POST /api/storage/worktrees/alpha/fix--A/delete HTTP/1.1\r\nHost: x\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nOrigin: https://box.tail1234.ts.net\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);

    assert!(answer.lines().next().unwrap_or("").contains("403"), "{answer}");
    assert!(path.exists());
}
