//! A merge-local run blocked because the checkout has uncommitted changes to files it changes can be merged from
//! the web with those changes set aside: only such a run reaches the launcher, and every attempt is audited.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, RunLauncher, StoreProjects};
use serde_json::{json, Value};

#[derive(Default)]
struct Launcher {
    merged: Mutex<Vec<(PathBuf, String)>>,
}

impl RunLauncher for Launcher {
    fn start(&self, _repo: &Path, _feature: &str) -> Result<String, LaunchError> {
        Err(LaunchError::Failed("not used".into()))
    }

    fn merge_set_aside(&self, repo: &Path, run: &str) -> Result<String, LaunchError> {
        self.merged.lock().unwrap().push((repo.to_path_buf(), run.to_string()));
        match run {
            "r-stale" => Err(LaunchError::Refused("cannot merge r-stale: main moved".into())),
            "r-crash" => Err(LaunchError::Failed("cannot find /usr/local/bin/orq".into())),
            _ => Ok("run-2".into()),
        }
    }
}

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
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

fn project_id(repo: &Path) -> String {
    orq_git::project_id(&orq_git::resolve_project(repo).unwrap())
}

fn add_run(s: &Setup, run: &str, project: &str, events: &[(&str, Value)]) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, (kind, payload)) in events.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: None, attempt: 1, occurred_at: "2026-11-01T10:00:00Z".into(), payload: payload.clone() };
        store.append_event(&event).unwrap();
    }
}

fn dirty_block(files: &[&str]) -> Vec<(&'static str, Value)> {
    vec![
        ("run.started", json!({})),
        ("merge.refused", json!({"reason": "dirty_checkout", "files": files})),
        ("run.completed", json!({"ending": "blocked", "reason": "main is checked out with uncommitted changes"})),
    ]
}

fn ask(s: &Setup, launcher: Option<&Arc<Launcher>>, method: &str, path: &str) -> (u16, Value) {
    let mut provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    if let Some(launcher) = launcher {
        provider = provider.with_launcher(launcher.clone());
    }
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

fn audited(s: &Setup) -> Vec<(String, Option<String>, String, Option<String>)> {
    let mut lines = orq_core::remote_audit::read(&s.roots, 100).unwrap();
    lines.reverse();
    lines
        .iter()
        .map(|line| (line["action"].as_str().unwrap().to_string(), line["run"].as_str().map(String::from), line["outcome"].as_str().unwrap().to_string(), line["reason"].as_str().map(String::from)))
        .collect()
}

#[test]
fn only_a_run_blocked_on_a_dirty_checkout_is_merged_setting_aside_and_every_attempt_is_audited() {
    let s = setup("merge-set-aside");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, "r-dirty", &project_id(&alpha), &dirty_block(&["src/lib.rs"]));
    add_run(&s, "r-stale", &project_id(&alpha), &dirty_block(&["src/lib.rs"]));
    add_run(&s, "r-crash", &project_id(&alpha), &dirty_block(&["src/lib.rs"]));
    add_run(&s, "r-moved", &project_id(&alpha), &[("run.started", json!({})), ("run.completed", json!({"ending": "blocked", "reason": "main moved"}))]);
    // A later merge attempt that was refused for another reason: the run no longer waits on the checkout.
    let mut since = dirty_block(&["src/lib.rs"]);
    since.insert(2, ("merge.refused", json!({"reason": "base_moved"})));
    add_run(&s, "r-since", &project_id(&alpha), &since);
    add_run(&s, "r-other", &project_id(&beta), &dirty_block(&["src/lib.rs"]));
    let launcher = Arc::new(Launcher::default());
    let merge = |run: &str| ask(&s, Some(&launcher), "POST", &format!("/api/projects/alpha/runs/{run}/merge-set-aside"));

    assert_eq!(merge("r-dirty"), (202, json!({"run": "run-2"})));
    assert_eq!(*launcher.merged.lock().unwrap(), [(alpha.clone(), "r-dirty".to_string())]);
    let not_waiting = json!({"error": "this run is not waiting on uncommitted changes in the checkout"});
    assert_eq!(merge("r-moved"), (409, not_waiting.clone()));
    assert_eq!(merge("r-since"), (409, not_waiting));
    assert_eq!(merge("r-stale"), (409, json!({"error": "cannot merge r-stale: main moved"})));
    assert_eq!(merge("r-crash"), (500, json!({"error": "the run could not be merged"})));
    assert_eq!(merge("r-other").0, 404);
    assert_eq!(merge("r-nothing").0, 404);
    assert_eq!(launcher.merged.lock().unwrap().len(), 3, "only runs waiting on the checkout reach the launcher");
    // A server with no launcher says so instead of pretending.
    assert_eq!(ask(&s, None, "POST", "/api/projects/alpha/runs/r-dirty/merge-set-aside"), (409, json!({"error": "this server cannot merge runs"})));

    let lines = audited(&s);
    let action = |run: &str, outcome: &str, reason: Option<&str>| ("run.merge_set_aside".to_string(), Some(run.to_string()), outcome.to_string(), reason.map(String::from));
    assert_eq!(lines[0], action("r-dirty", "allowed", None));
    assert_eq!(lines[1], action("r-moved", "refused", Some("this run is not waiting on uncommitted changes in the checkout")));
    assert_eq!(lines[3], action("r-stale", "refused", Some("cannot merge r-stale: main moved")));
    assert_eq!(lines[4], action("r-crash", "refused", Some("the run could not be merged")));
    assert!(lines.iter().all(|line| line.0 == "run.merge_set_aside"), "{lines:?}");
}

#[test]
fn a_run_waiting_on_the_checkout_names_its_files_and_a_set_aside_merge_says_where_the_changes_are() {
    let s = setup("merge-set-aside-shown");
    let alpha = git_repo(&s.scan, "alpha");
    let hostile = "src/\u{1b}[31mevil<b>.rs";
    add_run(&s, "r-dirty", &project_id(&alpha), &dirty_block(&["src/lib.rs", hostile]));
    let note = "your changes to src/lib.rs conflicted with this run and are in stash 0123abcd (orq set-aside r-merged); the checkout has conflict markers in src/lib.rs";
    let mut merged = dirty_block(&["src/lib.rs"]);
    merged.push(("merge.set_aside", json!({"stash": "0123abcd", "restored": false, "files": ["src/lib.rs"], "conflicted": ["src/lib.rs"], "note": note})));
    merged.push(("run.completed", json!({"ending": "merged"})));
    add_run(&s, "r-merged", &project_id(&alpha), &merged);

    let (status, list) = ask(&s, None, "GET", "/api/projects/alpha/runs");
    assert_eq!(status, 200, "{list}");
    let row = |run: &str| list["runs"].as_array().unwrap().iter().find(|row| row["run_id"] == run).unwrap().clone();
    assert_eq!(row("r-dirty")["dirty_files"], json!(["src/lib.rs", hostile]), "{list}");
    assert!(row("r-merged").get("dirty_files").is_none(), "a merged run waits on nothing: {list}");

    let (_, page) = ask(&s, None, "GET", "/api/projects/alpha/runs/r-dirty");
    assert_eq!(page["run"]["dirty_files"], json!(["src/lib.rs", hostile]), "{page}");

    let (_, page) = ask(&s, None, "GET", "/api/projects/alpha/runs/r-merged");
    assert!(page["run"].get("dirty_files").is_none(), "{page}");
    let event = page["events"].as_array().unwrap().iter().find(|event| event["kind"] == "merge.set_aside").unwrap().clone();
    assert_eq!(event["set_aside"], json!({"stash": "0123abcd", "message": "orq set-aside r-merged", "restored": false, "files": ["src/lib.rs"], "conflicted": ["src/lib.rs"], "note": note}), "{page}");
    let refused = page["events"].as_array().unwrap().iter().find(|event| event["kind"] == "merge.refused").unwrap().clone();
    assert!(refused.get("set_aside").is_none(), "{page}");
}

/// The page in the browser harness, or `None` when the web dependencies are not installed.
fn page(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

#[test]
fn the_page_of_a_run_waiting_on_the_checkout_merges_it_setting_the_changes_aside_after_asking() {
    let token = "typed-token-0123456789abcdef0123456789";
    let run = json!({"run_id": "r-dirty", "feature": "f-r-dirty", "state": "blocked", "stage": null, "started_at": null, "updated_at": null, "target_branch": "main", "dirty_files": ["src/lib.rs"]});
    let projects = json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}});
    let responses = vec![
        json!({"status": 200, "json": {"run": run, "events": [], "next_cursor": null, "remote_actions": true}}),
        projects,
        json!({"status": 200, "stream": [], "end": "hold"}),
        json!({"status": 202, "json": {"run": "run-2"}}),
    ];
    let steps = vec![json!({"submit": token}), json!({"click": "[data-merge-set-aside]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = page(json!({"url": "/projects/alpha/orchestrations/r-dirty", "responses": responses, "steps": steps, "selectors": ["[data-merge-set-aside]"]})) else { return };
    let posts: Vec<&Value> = out["requests"].as_array().unwrap().iter().filter(|r| r["method"] == "POST").collect();
    assert_eq!(posts.len(), 1, "{out}");
    assert_eq!(posts[0]["url"], "/api/projects/alpha/runs/r-dirty/merge-set-aside");
}
