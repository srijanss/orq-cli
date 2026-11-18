//! The web's Discard and Clear ask the orchestrator what the popup asks: what would go with the branch,
//! then discard the run, putting its features back to pending unless its work is on the target branch.

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
fn add_run(s: &Setup, repo: &Path, run: &str, kinds: &[&str]) {
    let project = project_id(repo);
    let base = git(repo, &["rev-parse", "HEAD"]);
    let worktree = repo.join(".claude").join("worktrees").join(run);
    let branch = format!("orq/{run}");
    git(repo, &["worktree", "add", "-q", "-b", &branch, worktree.to_str().unwrap()]);
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", &format!("work of {run}")]);
    let store = RunStore::new(&s.roots.state);
    store
        .create(&RunRecord { run_id: run.into(), project_id: project.clone(), feature_id: format!("f-{run}"), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: base, branch, worktree, repository: String::new(), profiles: Default::default(), delivery: Default::default() })
        .unwrap();
    for (n, kind) in kinds.iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.clone(), feature_id: format!("f-{run}"), kind: (*kind).into(), stage: None, attempt: 1, occurred_at: format!("2026-11-01T10:00:0{n}Z"), payload: json!({}) };
        store.append_event(&event).unwrap();
    }
}

fn send(s: &Setup, method: &str, path: &str) -> (u16, Value) {
    let provider = StoreProjects::new(s.roots.clone(), s.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&s.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

fn gone(s: &Setup, run: &str) -> bool {
    RunStore::new(&s.roots.state).load(run).is_err()
}

#[test]
fn a_failed_run_is_warned_about_then_discarded_and_a_merged_one_is_cleared() {
    let s = setup("run-discard");
    let alpha = git_repo(&s.scan, "alpha");
    let beta = git_repo(&s.scan, "beta");
    add_run(&s, &alpha, "r-fail", &["run.started", "run.failed"]);
    add_run(&s, &alpha, "r-merged", &["run.started", "run.failed"]);
    git(&alpha, &["merge", "-q", "--ff-only", "orq/r-merged"]);
    add_run(&s, &alpha, "r-run", &["run.started"]);
    add_run(&s, &beta, "r-beta", &["run.started", "run.failed"]);

    assert_eq!(
        send(&s, "GET", "/api/projects/alpha/runs/r-fail/discard-warning"),
        (200, json!({"run": "r-fail", "warning": "its branch orq/r-fail goes with 1 commit not on the target branch"}))
    );
    assert_eq!(send(&s, "GET", "/api/projects/alpha/runs/r-merged/discard-warning"), (200, json!({"run": "r-merged", "warning": ""})));
    assert_eq!(send(&s, "GET", "/api/projects/alpha/runs/r-beta/discard-warning").0, 404, "a run of another project is not this project's");

    let (status, body) = send(&s, "POST", "/api/projects/alpha/runs/r-fail/discard");
    assert_eq!((status, &body["run"], &body["cleared"]), (202, &json!("r-fail"), &json!(false)), "{body}");
    assert!(body["note"].as_str().unwrap().contains("Deleted the branch orq/r-fail with 1 commit not on the target branch"), "{body}");
    assert!(gone(&s, "r-fail"));

    let (status, body) = send(&s, "POST", "/api/projects/alpha/runs/r-merged/discard");
    assert_eq!((status, &body["cleared"]), (202, &json!(true)), "{body}");
    assert!(body["note"].as_str().unwrap().contains("Its work is on the target branch"), "{body}");
    assert!(gone(&s, "r-merged"));

    let (status, body) = send(&s, "POST", "/api/projects/alpha/runs/r-run/discard");
    assert_eq!(status, 409, "{body}");
    assert!(body["error"].as_str().unwrap().starts_with("r-run is running"), "{body}");
    assert!(!gone(&s, "r-run"));
    assert_eq!(send(&s, "POST", "/api/projects/alpha/runs/r-beta/discard").0, 404);
    assert!(!gone(&s, "r-beta"));
}

fn state_in_list(s: &Setup, run: &str) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/runs");
    assert_eq!(status, 200, "{body}");
    body["runs"].as_array().unwrap().iter().find(|row| row["run_id"] == run).map(|row| row["state"].clone()).unwrap_or(Value::Null)
}

#[test]
fn a_failed_or_review_waiting_run_whose_work_is_on_its_target_shows_as_merged() {
    let s = setup("runs-merged");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-fail", &["run.started", "run.failed"]);
    add_run(&s, &alpha, "r-merged", &["run.started", "run.failed"]);
    git(&alpha, &["merge", "-q", "--ff-only", "orq/r-merged"]);

    assert_eq!(state_in_list(&s, "r-fail"), json!("failed"));
    assert_eq!(state_in_list(&s, "r-merged"), json!("merged"), "its work is on main, as the popup shows it");
    let (status, body) = send(&s, "GET", "/api/projects/alpha/runs/r-merged");
    assert_eq!((status, &body["run"]["state"]), (200, &json!("merged")), "{body}");
    let (_, body) = send(&s, "GET", "/api/projects/alpha/runs/r-fail");
    assert_eq!(body["run"]["state"], json!("failed"), "{body}");
}

#[test]
fn a_worktree_kept_for_its_uncommitted_work_is_named_within_the_project_not_by_its_path_on_this_machine() {
    let s = setup("run-discard-kept");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-dirty", &["run.started", "run.failed"]);
    std::fs::write(alpha.join(".claude/worktrees/r-dirty/left.txt"), "x").unwrap();
    let (status, body) = send(&s, "POST", "/api/projects/alpha/runs/r-dirty/discard");
    assert_eq!(status, 202, "{body}");
    let note = body["note"].as_str().unwrap();
    assert!(note.contains("Uncommitted work: kept .claude/worktrees/r-dirty and its branch."), "{body}");
    assert!(!note.contains(alpha.to_str().unwrap()), "no path on this machine: {body}");
}

#[test]
fn the_reply_for_a_kept_worktree_says_which_file_kept_it() {
    let s = setup("run-discard-kept-reason");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-env", &["run.started", "run.failed"]);
    std::fs::write(alpha.join(".git/info/exclude"), ".env\n").unwrap();
    std::fs::write(alpha.join(".claude/worktrees/r-env/.env"), "SECRET=1\n").unwrap();

    let (status, body) = send(&s, "POST", "/api/projects/alpha/runs/r-env/discard");

    assert_eq!(status, 202, "{body}");
    let note = body["note"].as_str().unwrap();
    assert!(note.contains("Uncommitted work: kept .claude/worktrees/r-env and its branch. Reason: the worktree has uncommitted changes: .env."), "{body}");
    assert!(alpha.join(".claude/worktrees/r-env/.env").exists());
}

/// `later` continues the run `from` on its branch and worktree, as a resume does, a second after it started.
fn continue_run(s: &Setup, from: &str, later: &str) {
    let store = RunStore::new(&s.roots.state);
    let mut record = store.load(from).unwrap();
    let feature = record.feature_id.clone();
    record.run_id = later.into();
    store.create(&record).unwrap();
    let event = Event { schema_version: 1, event_id: format!("{later}-0"), run_id: later.into(), project_id: record.project_id.clone(), feature_id: feature, kind: "run.started".into(), stage: None, attempt: 1, occurred_at: "2026-11-01T11:00:00Z".into(), payload: json!({}) };
    store.append_event(&event).unwrap();
}

#[test]
fn a_run_a_later_run_continued_is_refused_with_the_reason_and_the_refusal_is_audited() {
    let s = setup("run-discard-taken-over");
    let alpha = git_repo(&s.scan, "alpha");
    add_run(&s, &alpha, "r-old", &["run.started", "run.failed"]);
    continue_run(&s, "r-old", "r-new");
    let reason = json!({"error": "r-old was continued as r-new; discard or accept that run instead"});

    assert_eq!(send(&s, "GET", "/api/projects/alpha/runs/r-old/discard-warning"), (409, reason.clone()));
    assert_eq!(send(&s, "POST", "/api/projects/alpha/runs/r-old/discard"), (409, reason.clone()));
    assert_eq!(send(&s, "POST", "/api/projects/alpha/runs/r-old/accept"), (409, reason));
    assert!(!gone(&s, "r-old") && !gone(&s, "r-new"));
    assert!(alpha.join(".claude/worktrees/r-old").exists());

    let audit = std::fs::read_to_string(s.roots.state.join("remote-actions.jsonl")).unwrap();
    let refusals: Vec<Value> = audit.lines().map(|line| serde_json::from_str(line).unwrap()).filter(|line: &Value| line["reason"] == json!("r-old was continued as r-new; discard or accept that run instead")).collect();
    assert_eq!(refusals.len(), 2, "{audit}");
}

fn ledger_status(repo: &Path, feature: &str) -> Value {
    let ledger: Value = serde_json::from_slice(&std::fs::read(repo.join(".tdd-features.json")).unwrap()).unwrap();
    ledger.as_array().unwrap().iter().find(|entry| entry["featureName"] == feature).unwrap()["status"].clone()
}

#[test]
fn a_discard_leaves_completed_a_feature_finished_after_the_run_stopped_or_whose_work_is_on_main_and_says_why() {
    let s = setup("run-discard-finished");
    // The runs' events are at 2026-11-01T10:00:00Z and 10:00:01Z.
    let cases: [(&str, &str, &str); 4] = [
        ("late", "completed", "2026-11-05T00:00:00Z"),
        ("onmain", "completed", "2026-11-01T09:00:00Z"),
        ("own", "completed", "2026-11-01T10:00:00Z"),
        ("wip", "in_progress", ""),
    ];
    let mut repos = Vec::new();
    for (name, status, at) in cases {
        let repo = git_repo(&s.scan, name);
        let run = format!("r-{name}");
        add_run(&s, &repo, &run, &["run.started", "run.blocked"]);
        let feature = format!("f-{run}");
        let recorded = if at.is_empty() { String::new() } else { format!(r#","recordedAt":"{at}""#) };
        std::fs::write(repo.join(".tdd-features.json"), format!(r#"[{{"featureName":"{feature}","status":"{status}"{recorded}}}]"#)).unwrap();
        repos.push(repo);
    }
    // The person finished `late` after the run blocked, and threw its checkout away.
    let late = &repos[0];
    git(late, &["worktree", "remove", "--force", late.join(".claude/worktrees/r-late").to_str().unwrap()]);
    git(late, &["branch", "-D", "orq/r-late"]);
    // The work of `onmain` reached main by a commit of its own.
    git(&repos[1], &["commit", "-q", "--allow-empty", "-m", "fix(web): the settings cog (f-r-onmain)"]);
    let sha = git(&repos[1], &["rev-parse", "--short=8", "HEAD"]);

    let (status, body) = send(&s, "POST", "/api/projects/late/runs/r-late/discard");
    assert_eq!(status, 202, "{body}");
    assert!(body["note"].as_str().unwrap().contains("f-r-late stays completed: finished after this run stopped."), "{body}");
    assert_eq!(ledger_status(late, "f-r-late"), "completed");

    let (status, body) = send(&s, "POST", "/api/projects/onmain/runs/r-onmain/discard");
    assert_eq!(status, 202, "{body}");
    assert!(body["note"].as_str().unwrap().contains(&format!("f-r-onmain stays completed: its work is on main ({sha}).")), "{body}");
    assert_eq!(ledger_status(&repos[1], "f-r-onmain"), "completed");

    let (status, body) = send(&s, "POST", "/api/projects/own/runs/r-own/discard");
    assert_eq!(status, 202, "{body}");
    assert!(body["note"].as_str().unwrap().contains("f-r-own is pending again."), "{body}");
    assert_eq!(ledger_status(&repos[2], "f-r-own"), "pending");

    let (status, body) = send(&s, "POST", "/api/projects/wip/runs/r-wip/discard");
    assert_eq!(status, 202, "{body}");
    assert!(body["note"].as_str().unwrap().contains("f-r-wip is pending again."), "{body}");
    assert_eq!(ledger_status(&repos[3], "f-r-wip"), "pending");
}

#[test]
fn a_discard_leaves_completed_a_feature_it_cannot_tell_was_finished_outside_the_run_when_the_target_history_cannot_be_read() {
    let s = setup("run-discard-unreadable-history");
    let repo = git_repo(&s.scan, "broken");
    add_run(&s, &repo, "r-broken", &["run.started", "run.blocked"]);
    std::fs::write(repo.join(".tdd-features.json"), r#"[{"featureName":"f-r-broken","status":"completed","recordedAt":"2026-11-01T09:00:00Z"}]"#).unwrap();
    // The person threw the run's checkout away, and main moved on past a commit whose object is lost.
    git(&repo, &["worktree", "remove", "--force", repo.join(".claude/worktrees/r-broken").to_str().unwrap()]);
    git(&repo, &["branch", "-D", "orq/r-broken"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "lost"]);
    let lost = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "after the lost one"]);
    std::fs::remove_file(repo.join(".git/objects").join(&lost[..2]).join(&lost[2..])).unwrap();

    let (status, body) = send(&s, "POST", "/api/projects/broken/runs/r-broken/discard");
    assert_eq!(status, 202, "{body}");
    assert!(body["note"].as_str().unwrap().contains("f-r-broken stays completed: cannot tell whether it was finished outside this run."), "{body}");
    assert_eq!(ledger_status(&repo, "f-r-broken"), "completed");
}
