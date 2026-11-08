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

#[test]
#[cfg(unix)]
fn a_logs_directory_that_cannot_be_read_is_an_error_not_an_empty_list() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("logs-unreadable");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha));
    log(&s, "r1", "gate.log", "hello\n");
    let dir = s.roots.state.join("logs/r1");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (status, body) = get(&s, "/api/projects/alpha/runs/r1/logs");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    if std::fs::read_dir(&dir).is_ok() && status == 200 && body == json!({"files": [{"name": "gate.log", "bytes": 6}]}) {
        // Running with rights that ignore permissions (root): nothing to check.
        return;
    }
    assert_eq!(status, 500, "{body}");
    assert!(!body.to_string().contains(dir.to_str().unwrap()), "{body}");
}

#[test]
fn a_log_over_the_read_cap_shows_its_readable_prefix_marked_truncated_with_secrets_and_odd_bytes_handled() {
    const CAP: usize = 8 * 1024 * 1024;
    let s = setup("logs-cap");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha));
    // Exactly at the cap: not truncated.
    log(&s, "r1", "exact.log", &format!("{}\n", "a".repeat(999)).repeat(CAP / 1000 + 1)[..CAP]);
    let (status, exact) = get(&s, "/api/projects/alpha/runs/r1/logs/exact.log");
    assert_eq!((status, &exact["truncated"]), (200, &json!(false)), "{}", &exact.to_string()[..200]);

    // A key block begins before the cap and ends after it; a two-byte character is cut by the cap.
    let mut bytes = format!("{}\n", "l".repeat(999)).repeat((CAP - 4000) / 1000).into_bytes();
    bytes.extend_from_slice(b"-----BEGIN PRIVATE KEY-----\nSECRETKEYBODYSECRETKEYBODY\n");
    bytes.resize(CAP - 1, b'S');
    bytes.extend_from_slice("é".as_bytes());
    bytes.extend_from_slice(b"\n-----END PRIVATE KEY-----\nafter\n");
    let dir = s.roots.state.join("logs/r1");
    std::fs::write(dir.join("big.log"), &bytes).unwrap();
    let mut cursor = String::new();
    let mut seen = String::new();
    let mut pages = 0;
    loop {
        let path = if cursor.is_empty() { "/api/projects/alpha/runs/r1/logs/big.log".to_string() } else { format!("/api/projects/alpha/runs/r1/logs/big.log/after/{cursor}") };
        let (status, page) = get(&s, &path);
        assert_eq!(status, 200);
        assert_eq!(page["truncated"], true);
        seen.push_str(&page["lines"].to_string());
        pages += 1;
        match page["next_cursor"].as_str() {
            Some(next) => cursor = next.to_string(),
            None => break,
        }
        assert!(pages < 5000, "paging must end");
    }
    assert!(!seen.contains("SECRETKEYBODY") && !seen.contains("after"), "the cut key block and everything past the cap is gone");
    assert!(seen.contains("[REDACTED]"));
}

#[test]
fn a_log_that_has_not_changed_is_not_read_and_redacted_again_for_the_next_page() {
    let s = setup("logs-snapshot");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha));
    let text: String = (0..600).map(|n| format!("line {n:04}\n")).collect();
    log(&s, "r1", "gate.log", &text);
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let page = |p: &StoreProjects, path: &str| ask(p, &s, "GET", path, Some("Bearer tok"));
    let (_, first) = page(&provider, "/api/projects/alpha/runs/r1/logs/gate.log");
    assert_eq!(first["next_cursor"], "500");

    // Same size and time, different bytes: only a fresh read would show it.
    let file = s.roots.state.join("logs/r1/gate.log");
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, text.replace("line", "LINE")).unwrap();
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(modified).unwrap();
    let (_, second) = page(&provider, "/api/projects/alpha/runs/r1/logs/gate.log/after/500");
    assert_eq!(second["lines"][0], "line 0500", "{second}");
    // A longer file is read afresh, and so is a new provider.
    std::fs::write(&file, format!("{}more\n", text.replace("line", "LINE"))).unwrap();
    assert_eq!(page(&provider, "/api/projects/alpha/runs/r1/logs/gate.log/after/500").1["lines"][0], "LINE 0500");
    std::fs::write(&file, text.replace("line", "LINE")).unwrap();
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(modified).unwrap();
    let fresh = StoreProjects::new(s.roots.clone(), s.scan.clone());
    assert_eq!(page(&fresh, "/api/projects/alpha/runs/r1/logs/gate.log/after/500").1["lines"][0], "LINE 0500");
}

#[test]
fn starting_a_log_again_reads_it_afresh_even_when_its_size_and_time_are_unchanged() {
    let s = setup("logs-restart");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, "r1", &project_id(&alpha));
    let text: String = (0..600).map(|n| format!("line {n:04}\n")).collect();
    log(&s, "r1", "gate.log", &text);
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let first = ask(&provider, &s, "GET", "/api/projects/alpha/runs/r1/logs/gate.log", Some("Bearer tok")).1;
    assert_eq!(first["lines"][0], "line 0000");
    let file = s.roots.state.join("logs/r1/gate.log");
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, text.replace("line", "LINE")).unwrap();
    std::fs::File::options().write(true).open(&file).unwrap().set_modified(modified).unwrap();
    let again = ask(&provider, &s, "GET", "/api/projects/alpha/runs/r1/logs/gate.log", Some("Bearer tok")).1;
    assert_eq!(again["lines"][0], "LINE 0000", "{again}");
}
