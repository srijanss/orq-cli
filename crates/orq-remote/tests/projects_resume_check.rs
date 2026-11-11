//! The web's resume check asks the orchestrator the popup's question: where would this run continue?

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    git(&path, &["commit", "-q", "--allow-empty", "-m", "start"]);
    path
}

fn project_id(repo: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(repo).unwrap())
}

/// A run of `repo` whose worktree is a real checkout of its own branch, one commit ahead of main.
fn add_run(s: &Setup, repo: &Path, run: &str, project: &str, kinds: &[&str]) {
    let base = git(repo, &["rev-parse", "HEAD"]);
    let worktree = repo.join(".claude").join("worktrees").join(run);
    let branch = format!("orq/{run}");
    git(repo, &["worktree", "add", "-q", "-b", &branch, worktree.to_str().unwrap()]);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "work"]);
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: base, branch, worktree, repository: String::new(), profiles: Default::default() })
        .unwrap();
    for (n, kind) in kinds.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: None, attempt: 1, occurred_at: format!("2026-11-01T10:00:0{n}Z"), payload: json!({}) };
        store.append_event(&event).unwrap();
    }
}

fn check(s: &Setup, path: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

#[test]
fn a_stopped_run_continues_where_it_stopped_and_a_finished_one_is_refused_in_the_orchestrators_words() {
    let s = setup("resume-check");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, &alpha, "r-fail", &project_id(&alpha), &["run.started", "run.failed"]);
    add_run(&s, &alpha, "r-done", &project_id(&alpha), &["run.started", "run.completed"]);
    add_run(&s, &beta, "r-beta", &project_id(&beta), &["run.started", "run.failed"]);

    assert_eq!(check(&s, "/api/projects/alpha/runs/r-fail/resume-check"), (200, json!({"run": "r-fail", "next": "where it stopped"})));
    let (status, body) = check(&s, "/api/projects/alpha/runs/r-done/resume-check");
    assert_eq!(status, 409, "{body}");
    assert!(body["error"].as_str().unwrap().starts_with("r-done "), "{body}");
    assert_eq!(check(&s, "/api/projects/alpha/runs/r-beta/resume-check").0, 404, "a run of another project is not this project's");
    assert_eq!(check(&s, "/api/projects/alpha/runs/r-none/resume-check").0, 404);
}
