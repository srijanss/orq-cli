//! The web's Push of a project's target branch: `GET /api/projects/:id/push-target` says whether the
//! push is offered, why not, and the commits it would send; `POST` with the confirmed `{sha}` pushes,
//! only with remote actions on and from a passkey session, and writes one `project.push_target` audit
//! line. Driven against a checkout whose github.com origin git rewrites to a bare repository, with a
//! fake `gh` answering the branch lookup.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with_body, handle_with_confirm, RemoteConfig, Request};
use orq_core::project_settings::Delivery;
use orq_core::storage::StorageRoots;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use orq_test_support::FakeProvider;
use serde_json::{json, Value};

const ORIGIN: &str = "https://github.com/acme/alpha.git";
const LOOKUP: [&str; 2] = ["api", "repos/acme/alpha/branches/main"];
const RUN: &str = "run-1-PUSH-01";

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    repo: PathBuf,
    origin: PathBuf,
    /// Origin's `main` before the push.
    base: String,
    /// The commit from run `RUN`, and the one made by hand after it.
    from_run: String,
    by_hand: String,
    gh: FakeProvider,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, name: &str, subject: &str) -> String {
    std::fs::write(dir.join(name), format!("{subject}\n")).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", subject]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Project `alpha` on `main`, level with origin, then one commit merged locally from run `RUN` and one
/// made by hand; `origin_url` is what origin is set to, `gh` answers the branch lookup.
fn setup(label: &str, origin_url: &str, gh: FakeProvider) -> Setup {
    setup_with(label, origin_url, gh, Delivery::MergeLocal)
}

/// As [`setup`], with run `RUN` delivered by `delivery`.
fn setup_with(label: &str, origin_url: &str, gh: FakeProvider, delivery: Delivery) -> Setup {
    let base_dir = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base_dir.join("data"),
        config: base_dir.join("config"),
        state: base_dir.join("state"),
        cache: base_dir.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let origin = base_dir.join("origin.git");
    git(&base_dir, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    let scan = base_dir.join("code");
    let repo = scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", origin_url]);
    git(&repo, &["config", &format!("url.file://{}.insteadOf", origin.display()), origin_url]);
    std::fs::write(repo.join(".gitignore"), ".tdd-*.json\n.claude/\n").unwrap();
    std::fs::write(repo.join(".tdd-features.json"), json!([{"featureName": "PUSH-01", "dependsOn": [], "status": "in_progress"}]).to_string()).unwrap();
    let base = commit(&repo, "README", "start");
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    let from_run = commit(&repo, "feature.txt", "PUSH-01 work");
    let by_hand = commit(&repo, "notes.txt", "a note by hand");

    let store = RunStore::new(&roots.state);
    store
        .create(&RunRecord {
            delivery,
            run_id: RUN.into(),
            project_id: orq_git::project_id(&orq_git::resolve_project(&repo).unwrap()),
            feature_id: "PUSH-01".into(),
            remote: "origin".into(),
            provider: "github".into(),
            target_branch: "main".into(),
            base_sha: base.clone(),
            branch: format!("orq/{RUN}"),
            worktree: repo.join(".claude/worktrees").join(RUN),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{RUN}-0"),
            run_id: RUN.into(),
            project_id: store.load(RUN).unwrap().project_id,
            feature_id: "PUSH-01".into(),
            kind: "merge.completed".into(),
            stage: Some("merge_local".into()),
            attempt: 1,
            occurred_at: "2026-11-19T10:00:00Z".into(),
            payload: json!({"head_sha": from_run}),
        })
        .unwrap();
    Setup { scan, roots, repo, origin, base, from_run, by_hand, gh }
}

fn github(label: &str) -> Setup {
    setup(label, ORIGIN, FakeProvider::builder().reply(&LOOKUP, r#"{"name":"main","protected":false}"#).build())
}

fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone()).with_gh(s.gh.gh())
}

fn request<'a>(method: &'a str, path: &'a str) -> Request<'a> {
    Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") }
}

fn send(s: &Setup, method: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request(method, "/api/projects/alpha/push-target"), body, &provider(s));
    (r.status, r.body)
}

fn send_with_passkey(s: &Setup, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let confirm = |_: &Value| true;
    let r = handle_with_confirm(&s.roots, &config, "server-project", "server", &request("POST", "/api/projects/alpha/push-target"), body, &provider(s), Some(&confirm));
    (r.status, r.body)
}

fn audit_lines(s: &Setup) -> Vec<Value> {
    orq_core::remote_audit::read(&s.roots, 100).unwrap().into_iter().filter(|line| line["action"] == "project.push_target").collect()
}

fn origin_main(s: &Setup) -> String {
    git(&s.origin, &["rev-parse", "refs/heads/main"])
}

#[test]
fn get_lists_the_commits_a_push_would_send_with_their_runs_and_offers_the_push() {
    let s = github("push-target-get");
    let (status, body) = send(&s, "GET", b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["provider"], "github", "{body}");
    assert_eq!(body["reason"], Value::Null, "{body}");
    assert_eq!((body["remote"].as_str(), body["target"].as_str(), body["count"].as_u64()), (Some("origin"), Some("main"), Some(2)), "{body}");
    assert_eq!(body["sha"], s.by_hand, "{body}");
    assert_eq!(body["origin_sha"], s.base, "{body}");
    assert_eq!(body["remote_actions"], true, "{body}");
    let commits = body["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2, "{body}");
    assert_eq!((commits[0]["sha"].as_str(), commits[0]["subject"].as_str(), &commits[0]["run"]), (Some(s.by_hand.as_str()), Some("a note by hand"), &Value::Null), "{body}");
    assert_eq!((commits[1]["sha"].as_str(), commits[1]["run"].as_str()), (Some(s.from_run.as_str()), Some(RUN)), "{body}");
    assert_eq!(commits[1]["short"], s.from_run[..7], "{body}");
    assert_eq!(body["more"], 0, "{body}");
    assert!(audit_lines(&s).is_empty(), "a read is not audited");
}

#[test]
fn get_for_a_gitlab_project_says_deliver_by_mr_review_and_offers_no_push() {
    let s = setup("push-target-gitlab", "https://gitlab.com/acme/alpha.git", FakeProvider::builder().build());
    let (status, body) = send(&s, "GET", b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!((body["provider"].as_str(), body["reason"].as_str()), (Some("gitlab"), Some("GitLab: deliver by MR review")), "{body}");
    assert!(s.gh.calls().is_empty(), "GitHub is not asked about a GitLab project");
}

#[test]
fn get_for_a_protected_branch_gives_the_reason_the_push_is_off() {
    let s = setup("push-target-protected", ORIGIN, FakeProvider::builder().reply(&LOOKUP, r#"{"name":"main","protected":true}"#).build());
    let (status, body) = send(&s, "GET", b"");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["reason"], "main is protected on GitHub: deliver by review instead", "{body}");
}

/// The "not pushed" line of project `alpha` as the project card and the project's queue page get it.
fn unpushed_lines(s: &Setup) -> (Value, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let get = |path: &str| handle_with_body(&s.roots, &config, "server-project", "server", &request("GET", path), b"", &provider(s)).body;
    let list = get("/api/projects");
    let card = list["projects"].as_array().unwrap().iter().find(|p| p["id"] == "alpha").cloned().unwrap();
    let queue = get("/api/projects/alpha/queue");
    (card["unpushed"].clone(), queue["queue"]["unpushed"].clone())
}

#[test]
fn the_not_pushed_line_names_the_project_provider_so_only_github_offers_the_push() {
    let s = github("push-target-line-github");
    let (card, queue) = unpushed_lines(&s);
    assert_eq!((card["count"].as_u64(), card["provider"].as_str()), (Some(2), Some("github")), "{card}");
    assert_eq!(queue["provider"], "github", "{queue}");

    let s = setup("push-target-line-gitlab", "https://gitlab.com/acme/alpha.git", FakeProvider::builder().build());
    let (card, queue) = unpushed_lines(&s);
    assert_eq!(card["provider"], "gitlab", "{card}");
    assert_eq!(queue["provider"], "gitlab", "{queue}");
    assert!(s.gh.calls().is_empty(), "the line asks GitHub nothing");
}

fn run_page(s: &Setup) -> Value {
    let config = RemoteConfig { token: "tok".into() };
    let path = format!("/api/projects/alpha/runs/{RUN}");
    handle_with_body(&s.roots, &config, "server-project", "server", &request("GET", &path), b"", &provider(s)).body
}

#[test]
fn a_delivered_merge_local_run_page_shows_the_not_pushed_line_and_its_timeline_names_the_push() {
    let s = github("push-target-run-page");
    let page = run_page(&s);
    assert_eq!((page["run"]["unpushed"]["count"].as_u64(), page["run"]["unpushed"]["provider"].as_str()), (Some(2), Some("github")), "{page}");

    let body = json!({"sha": s.by_hand}).to_string();
    let (status, answer) = send_with_passkey(&s, body.as_bytes());
    assert_eq!(status, 200, "{answer}");
    let page = run_page(&s);
    assert_eq!(page["run"]["unpushed"], Value::Null, "nothing is left to push: {page}");
    let pushed = page["events"].as_array().unwrap().iter().find(|event| event["kind"] == "target.pushed").cloned().unwrap();
    assert_eq!(pushed["pushed"], json!({"remote": "origin", "target": "main"}), "{pushed}");
}

#[test]
fn a_run_page_of_a_run_not_merged_locally_has_no_not_pushed_line() {
    let s = setup_with("push-target-run-review", ORIGIN, FakeProvider::builder().build(), Delivery::Review);
    let page = run_page(&s);
    assert_eq!(page["run"]["unpushed"], Value::Null, "{page}");
}

#[test]
fn get_for_an_unknown_project_is_not_found() {
    let s = github("push-target-unknown");
    let config = RemoteConfig { token: "tok".into() };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request("GET", "/api/projects/nope/push-target"), b"", &provider(&s));
    assert_eq!(r.status, 404, "{}", r.body);
}

#[test]
fn post_from_a_passkey_session_pushes_the_confirmed_commit_records_the_runs_and_writes_one_audit_line() {
    let s = github("push-target-post");
    let body = json!({"sha": s.by_hand}).to_string();
    let (status, answer) = send_with_passkey(&s, body.as_bytes());
    assert_eq!(status, 200, "{answer}");
    assert_eq!((answer["from"].as_str(), answer["to"].as_str(), answer["count"].as_u64()), (Some(s.base.as_str()), Some(s.by_hand.as_str()), Some(2)), "{answer}");
    assert_eq!(answer["runs"], json!([RUN]), "{answer}");
    assert_eq!(origin_main(&s), s.by_hand, "origin has the local target");

    let lines = audit_lines(&s);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["outcome"], "allowed", "{lines:?}");
    assert_eq!(lines[0]["project"], "alpha", "{lines:?}");
    let reason = lines[0]["reason"].as_str().unwrap();
    for part in ["main", &s.base[..], &s.by_hand[..], "2 commits", "from web"] {
        assert!(reason.contains(part), "{part} in {reason}");
    }

    let events = RunStore::new(&s.roots.state).events(RUN).unwrap();
    let pushed: Vec<&Event> = events.iter().filter(|event| event.kind == "target.pushed").collect();
    assert_eq!(pushed.len(), 1, "{events:?}");
    assert_eq!((pushed[0].payload["target"].as_str(), pushed[0].payload["remote"].as_str()), (Some("main"), Some("origin")));

    let (_, after) = send(&s, "GET", b"");
    assert_eq!(after["count"], 0, "nothing is left to push: {after}");
}

#[test]
fn post_without_a_passkey_session_or_with_actions_off_pushes_nothing_and_is_audited() {
    let s = github("push-target-no-passkey");
    let body = json!({"sha": s.by_hand}).to_string();
    let (status, answer) = send(&s, "POST", body.as_bytes());
    assert_eq!(status, 403, "{answer}");
    assert_eq!(origin_main(&s), s.base, "nothing was pushed");
    let lines = audit_lines(&s);
    assert_eq!((lines.len(), lines[0]["outcome"].as_str()), (1, Some("refused")), "{lines:?}");

    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    let (status, answer) = send_with_passkey(&s, body.as_bytes());
    assert_eq!(status, 404, "{answer}");
    assert_eq!(origin_main(&s), s.base, "nothing was pushed");
}

#[test]
fn post_of_a_sha_that_is_no_longer_the_target_is_refused_as_the_branch_changed() {
    let s = github("push-target-stale");
    let stale = json!({"sha": s.from_run}).to_string();
    let (status, answer) = send_with_passkey(&s, stale.as_bytes());
    assert_eq!(status, 409, "{answer}");
    assert_eq!(answer["error"], "the branch changed: review again", "{answer}");
    assert_eq!(origin_main(&s), s.base, "nothing was pushed");

    let (status, answer) = send_with_passkey(&s, br#"{"sha":"main"}"#);
    assert_eq!(status, 400, "a sha that is not a commit id is refused: {answer}");
    let (status, answer) = send_with_passkey(&s, br#"{"sha":"x","force":true}"#);
    assert_eq!(status, 400, "an unknown field is refused: {answer}");
    assert_eq!(origin_main(&s), s.base, "nothing was pushed");
    let _ = &s.repo;
}
