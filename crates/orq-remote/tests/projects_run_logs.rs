//! `/api/projects/{id}/runs/{run}/logs`: a run's log files and their lines in pages, redacted,
//! read-only and scoped to the run's own project.

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

fn add_run(s: &Setup, run: &str, project: &str) {
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: s.scan.join(run), repository: String::new(), profiles: Default::default() })
        .unwrap();
    let event = Event { schema_version: 1, event_id: format!("{run}-0"), run_id: run.into(), project_id: project.into(), feature_id: format!("f-{run}"), kind: "run.started".into(), stage: None, attempt: 1, occurred_at: "2026-11-01T10:00:00Z".into(), payload: json!({}) };
    store.append_event(&event).unwrap();
}

fn log(s: &Setup, run: &str, name: &str, text: &str) {
    let dir = s.roots.state.join("logs").join(run);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

fn get(s: &Setup, path: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    ask(&provider, s, "GET", path, Some("Bearer tok"))
}

fn ask(provider: &StoreProjects, s: &Setup, method: &str, path: &str, authorization: Option<&str>) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, provider);
    (r.status, r.body)
}

#[test]
fn the_log_files_of_a_run_are_listed_and_a_file_gives_its_lines_with_secrets_removed() {
    let s = setup("logs-basic");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha));
    log(&s, "r1", "gate.log", "building\nAuthorization: Bearer abcdefghijklmnop12345\nGITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwx\ndone\n");
    log(&s, "r1", "push.log", "pushed\n");
    log(&s, "r1", "gate.done", "");
    log(&s, "r1", ".hidden.log", "x\n");

    let (status, files) = get(&s, "/api/projects/alpha/runs/r1/logs");
    assert_eq!(status, 200, "{files}");
    assert_eq!(files, json!({"files": [{"name": "gate.log", "bytes": 100}, {"name": "push.log", "bytes": 7}]}), "{files}");

    let (status, page) = get(&s, "/api/projects/alpha/runs/r1/logs/gate.log");
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["name"], "gate.log");
    assert_eq!(page["next_cursor"], Value::Null);
    let text = page["lines"].to_string();
    assert!(!text.contains("abcdefghijklmnop12345") && !text.contains("ghp_"), "{page}");
    assert_eq!(page["lines"][0], "building");
    assert_eq!(page["lines"][3], "done");
}


#[test]
fn a_long_log_is_paged_by_line_and_a_secret_spanning_lines_is_removed_wherever_the_page_breaks() {
    let s = setup("logs-paging");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha));
    let mut text = String::new();
    for n in 0..499 {
        text.push_str(&format!("line {n}\n"));
    }
    text.push_str("-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nZZZZsecretkeybodyZZZZ\n-----END PRIVATE KEY-----\nafter the key\nand more\n");
    log(&s, "r1", "gate.log", &text);

    let (_, first) = get(&s, "/api/projects/alpha/runs/r1/logs/gate.log");
    assert_eq!(first["lines"].as_array().unwrap().len(), 500, "{first}");
    assert_eq!(first["next_cursor"], "500");
    let (status, second) = get(&s, "/api/projects/alpha/runs/r1/logs/gate.log/after/500");
    assert_eq!(status, 200, "{second}");
    assert!(!first.to_string().contains("secretkeybody") && !second.to_string().contains("secretkeybody"), "{first}{second}");
    assert!(second["lines"].to_string().contains("after the key"), "{second}");
    assert_eq!(second["next_cursor"], Value::Null);
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/gate.log/after/9999").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/gate.log/after/x").0, 404);
}

#[test]
fn only_the_runs_own_plain_log_files_are_served_to_a_valid_token() {
    let s = setup("logs-scope");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, "r1", &project_id(&alpha));
    log(&s, "r1", "gate.log", "hello\n");
    log(&s, "r1", "gate.done", "x");
    std::fs::write(s.scan.join("outside.txt"), "outside secret\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(s.scan.join("outside.txt"), s.roots.state.join("logs/r1/link.log")).unwrap();

    assert_eq!(get(&s, "/api/projects/beta/runs/r1/logs").0, 404, "another project's run");
    assert_eq!(get(&s, "/api/projects/nope/runs/r1/logs").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/nope/logs").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/gate.done").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/missing.log").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/..%2Fx.log").0, 404);
    #[cfg(unix)]
    {
        assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/link.log").0, 404);
        let (_, files) = get(&s, "/api/projects/alpha/runs/r1/logs");
        assert_eq!(files, json!({"files": [{"name": "gate.log", "bytes": 6}]}), "{files}");
    }
    let _ = beta;
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    assert_eq!(ask(&provider, &s, "GET", "/api/projects/alpha/runs/r1/logs", None).0, 401);
    assert_eq!(ask(&provider, &s, "POST", "/api/projects/alpha/runs/r1/logs", Some("Bearer tok")).0, 405);
    // No logs yet is an empty list, not an error.
    add_run(&s, "r2", &project_id(&alpha));
    assert_eq!(get(&s, "/api/projects/alpha/runs/r2/logs").1, json!({"files": []}));
}

#[test]
#[cfg(unix)]
fn a_logs_directory_that_is_a_link_serves_nothing_from_where_it_points() {
    let s = setup("logs-dirlink");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha));
    let elsewhere = s.scan.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("stolen.log"), "not this run's\n").unwrap();
    std::fs::create_dir_all(s.roots.state.join("logs")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, s.roots.state.join("logs/r1")).unwrap();
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/stolen.log").0, 404);
    // The same for the logs directory itself.
    std::fs::remove_file(s.roots.state.join("logs/r1")).unwrap();
    std::fs::create_dir_all(elsewhere.join("r1")).unwrap();
    std::fs::write(elsewhere.join("r1/stolen.log"), "x\n").unwrap();
    std::fs::remove_dir(s.roots.state.join("logs")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, s.roots.state.join("logs")).unwrap();
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs").0, 404);
    assert_eq!(get(&s, "/api/projects/alpha/runs/r1/logs/stolen.log").0, 404);
}
