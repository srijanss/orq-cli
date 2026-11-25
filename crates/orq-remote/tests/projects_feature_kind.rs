//! A feature is standard or frontend, kept in orq.db by project and feature name, never in the ledger. The web's
//! feature detail sets it through an authenticated action like the page's others, and the features and runs lists
//! say each one's kind.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
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
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    std::fs::write(path.join(".tdd-features.json"), r#"[{"featureName":"page","status":"pending"},{"featureName":"engine","status":"draft"}]"#).unwrap();
    path
}

fn call(s: &Setup, method: &str, path: &str, authorization: Option<&str>, body: &[u8]) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &provider);
    (r.status, r.body)
}

fn set_kind(s: &Setup, feature: &str, kind: &str) -> (u16, Value) {
    let body = json!({ "kind": kind }).to_string();
    call(s, "POST", &format!("/api/projects/alpha/features/{feature}/kind"), Some("Bearer tok"), body.as_bytes())
}

fn kinds(s: &Setup) -> Vec<(String, String)> {
    let (status, body) = call(s, "GET", "/api/projects/alpha/features", Some("Bearer tok"), b"");
    assert_eq!(status, 200, "{body}");
    body["features"].as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap().to_string(), f["kind"].as_str().unwrap().to_string())).collect()
}

fn pair(name: &str, kind: &str) -> (String, String) {
    (name.to_string(), kind.to_string())
}

#[test]
fn a_feature_set_to_frontend_reads_back_as_frontend_and_every_other_stays_standard() {
    let s = setup("kind-set");
    let alpha = git_repo(&s.scan, "alpha");
    let ledger = std::fs::read(alpha.join(".tdd-features.json")).unwrap();

    assert_eq!(kinds(&s), [pair("page", "standard"), pair("engine", "standard")], "standard by default");
    assert_eq!(set_kind(&s, "page", "frontend"), (200, json!({"feature": "page", "kind": "frontend"})));
    assert_eq!(kinds(&s), [pair("page", "frontend"), pair("engine", "standard")]);
    assert_eq!(set_kind(&s, "page", "standard"), (200, json!({"feature": "page", "kind": "standard"})));
    assert_eq!(kinds(&s), [pair("page", "standard"), pair("engine", "standard")]);
    assert_eq!(std::fs::read(alpha.join(".tdd-features.json")).unwrap(), ledger, "the ledger gets no new field");
}

#[test]
fn each_feature_says_the_design_package_folder_it_resolves_to_its_own_a_shared_one_or_none() {
    let s = setup("kind-design-package");
    let alpha = git_repo(&s.scan, "alpha");
    let ledger = json!([
        {"featureName": "page", "status": "pending"},
        {"featureName": "home", "status": "pending", "description": "The footer.\n\nDesign package: egiftit-homepage\nDesign components: F01\n"},
        {"featureName": "lost", "status": "pending", "description": "Design package: nowhere\n"},
        {"featureName": "engine", "status": "draft"},
    ]);
    std::fs::write(alpha.join(".tdd-features.json"), ledger.to_string()).unwrap();
    for package in ["page", "egiftit-homepage", "home"] {
        std::fs::create_dir_all(alpha.join("designs").join(package)).unwrap();
    }

    let (status, body) = call(&s, "GET", "/api/projects/alpha/features", Some("Bearer tok"), b"");
    assert_eq!(status, 200, "{body}");
    let packages: Vec<(String, Value)> = body["features"].as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap().to_string(), f["design_package"].clone())).collect();
    assert_eq!(
        packages,
        [
            ("page".to_string(), json!("page")),
            ("home".to_string(), json!("egiftit-homepage")),
            ("lost".to_string(), Value::Null),
            ("engine".to_string(), Value::Null),
        ],
        "a feature naming a shared package reads it, never its own folder; a package not on disk is none"
    );
}

fn create_run(s: &Setup, repo: &Path, run: &str, feature: &str) {
    use orq_orchestrator::{Event, RunRecord, RunStore};
    let project = orq_git::project_id(&orq_git::resolve_project(repo).unwrap());
    let runs = RunStore::new(&s.roots.state);
    runs.create(&RunRecord { run_id: run.into(), project_id: project.clone(), feature_id: feature.into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: repo.join(run), repository: String::new(), profiles: Default::default(), delivery: Default::default() }).unwrap();
    runs.append_event(&Event { schema_version: 1, event_id: format!("{run}-1"), run_id: run.into(), project_id: project, feature_id: feature.into(), kind: "run.started".into(), stage: None, attempt: 1, occurred_at: "2026-11-20T10:00:00Z".into(), payload: json!({}) }).unwrap();
}

#[test]
fn each_run_says_the_kind_of_its_feature() {
    let s = setup("kind-runs");
    let alpha = git_repo(&s.scan, "alpha");
    create_run(&s, &alpha, "r-page", "page");
    create_run(&s, &alpha, "r-engine", "engine");
    let run_kinds = |s: &Setup| {
        let (status, body) = call(s, "GET", "/api/projects/alpha/runs", Some("Bearer tok"), b"");
        assert_eq!(status, 200, "{body}");
        let mut listed: Vec<(String, String)> = body["runs"].as_array().unwrap().iter().map(|r| (r["feature"].as_str().unwrap().to_string(), r["feature_kind"].as_str().unwrap().to_string())).collect();
        listed.sort();
        listed
    };
    assert_eq!(run_kinds(&s), [pair("engine", "standard"), pair("page", "standard")], "standard by default");
    assert_eq!(set_kind(&s, "page", "frontend").0, 200);
    assert_eq!(run_kinds(&s), [pair("engine", "standard"), pair("page", "frontend")]);
}

#[test]
fn an_unknown_kind_a_feature_not_in_the_ledger_a_missing_token_and_actions_off_are_refused() {
    let s = setup("kind-refused");
    git_repo(&s.scan, "alpha");

    assert_eq!(set_kind(&s, "page", "backend").0, 400, "only standard and frontend are kinds");
    assert_eq!(set_kind(&s, "nowhere", "frontend"), (409, json!({"error": "nowhere is not in this project's ledger"})));
    assert_eq!(call(&s, "POST", "/api/projects/gamma/features/page/kind", Some("Bearer tok"), br#"{"kind":"frontend"}"#).0, 404, "no such project");
    assert_eq!(call(&s, "POST", "/api/projects/alpha/features/page/kind", Some("Bearer tok"), br#"{"kind":"frontend","extra":1}"#).0, 400);

    for authorization in [None, Some("Bearer wrong")] {
        assert_eq!(call(&s, "POST", "/api/projects/alpha/features/page/kind", authorization, br#"{"kind":"frontend"}"#).0, 401, "{authorization:?}");
    }
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    assert_eq!(set_kind(&s, "page", "frontend").0, 404, "the switch is an action like the page's others");
    assert_eq!(kinds(&s), [pair("page", "standard"), pair("engine", "standard")], "nothing refused was kept");
}

/// Listing reads orq.db without writing to it: a database from before feature kinds is neither migrated nor
/// upgraded by a list of features or runs, and its features read as standard.
#[test]
fn listing_reads_an_older_database_as_all_standard_without_upgrading_it() {
    let s = setup("kind-list-read-only");
    let alpha = git_repo(&s.scan, "alpha");
    create_run(&s, &alpha, "r-page", "page");
    assert_eq!(set_kind(&s, "page", "frontend").0, 200);
    let db = s.roots.database();
    let version = || -> u32 { rusqlite::Connection::open(&db).unwrap().pragma_query_value(None, "user_version", |row| row.get(0)).unwrap() };
    let tables = || -> i64 { rusqlite::Connection::open(&db).unwrap().query_row("SELECT count(*) FROM sqlite_master WHERE name = 'feature_kinds'", [], |row| row.get(0)).unwrap() };
    // Schema 16 added feature_kinds.
    let before_kinds = 15;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("ALTER TABLE feature_kinds RENAME TO feature_kinds_old;").unwrap();
    conn.pragma_update(None, "user_version", before_kinds).unwrap();
    drop(conn);

    assert_eq!(kinds(&s), [pair("page", "standard"), pair("engine", "standard")]);
    let (status, body) = call(&s, "GET", "/api/projects/alpha/runs", Some("Bearer tok"), b"");
    assert_eq!((status, &body["runs"][0]["feature_kind"]), (200, &json!("standard")), "{body}");
    assert_eq!(version(), before_kinds, "left as it was");
    assert_eq!(tables(), 0, "not migrated by a listing");
}
