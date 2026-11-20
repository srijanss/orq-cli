//! Once a run's MR is merged, its run page and its project's card offer Clean up: mark the feature
//! completed (the ledger snapshotted first), delete the kept worktree, delete the local branch, and
//! fast-forward the main checkout's target, each only when it is safe. The project's `after_merge`
//! setting can have the server do it on its own when it sees the merge. Driven here against a
//! temporary project cloned from a bare origin, with a second clone standing for the forge.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::project_settings::{AfterMerge, ProjectSettings};
use orq_core::remote_inbox::{handle_with_body, handle_with_confirm, RemoteConfig, Request};
use orq_core::review_lifecycle::{ProviderState, ReviewLifecycle};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

const RUN: &str = "run-1-COM-01";

struct Setup {
    base: PathBuf,
    scan: PathBuf,
    roots: StorageRoots,
    /// The project's main checkout, a clone of `origin`.
    repo: PathBuf,
    /// Another clone of `origin`, standing for the forge merging MRs.
    forge: PathBuf,
    /// The run's kept worktree.
    worktree: PathBuf,
}

/// A project `alpha` cloned from a bare origin, whose feature COM-01 was built by run `RUN` in a kept
/// worktree on branch `orq/RUN`, pushed and opened as !12; `change` sets the project's settings.
fn setup(label: &str, change: impl FnOnce(&mut ProjectSettings)) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let origin = base.join("origin.git");
    git(
        &base,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    let scan = base.join("code");
    std::fs::create_dir_all(&scan).unwrap();
    git(&scan, &["clone", "-q", origin.to_str().unwrap(), "alpha"]);
    let repo = std::fs::canonicalize(scan.join("alpha")).unwrap();
    git(&repo, &["checkout", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "start\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".tdd-*.json\n.claude/\n").unwrap();
    git(&repo, &["add", "README", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let forge = base.join("forge");
    git(
        &base,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            forge.to_str().unwrap(),
        ],
    );
    let ledger = json!([{"featureName": "COM-01", "dependsOn": [], "status": "in_progress"}, {"featureName": "COM-02", "dependsOn": [], "status": "pending"}]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    let mut settings = ProjectSettings::default();
    change(&mut settings);
    Store::open(&roots.database())
        .unwrap()
        .save_project_settings(&repo, 0, &settings)
        .unwrap();

    let branch = format!("orq/{RUN}");
    let worktree = repo.join(".claude/worktrees").join(RUN);
    let base_sha = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::write(worktree.join("feature.txt"), "COM-01\n").unwrap();
    git(&worktree, &["add", "feature.txt"]);
    git(&worktree, &["commit", "-q", "-m", "COM-01 work"]);
    git(&worktree, &["push", "-q", "origin", &branch]);
    let store = RunStore::new(&roots.state);
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: RUN.into(),
            project_id: orq_git::project_id(&orq_git::resolve_project(&repo).unwrap()),
            feature_id: "COM-01".into(),
            remote: "origin".into(),
            provider: "gitlab".into(),
            target_branch: "main".into(),
            base_sha,
            branch,
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: Default::default(),
        })
        .unwrap();
    event(&store, "stage.started", json!({}));
    event(&store, "run.completed", json!({"ending": "review_ready"}));
    Setup {
        base,
        scan,
        roots,
        repo,
        forge,
        worktree,
    }
}

fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone())
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method,
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let r = handle_with_body(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        body,
        &provider(s),
    );
    (r.status, r.body)
}

/// A request from a passkey session, whose step-up checks always pass.
fn send_with_passkey(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig {
        token: "tok".into(),
    };
    let request = Request {
        method,
        path,
        peer: "127.0.0.1".parse().unwrap(),
        authorization: Some("Bearer tok"),
    };
    let confirm = |_: &Value| true;
    let r = handle_with_confirm(
        &s.roots,
        &config,
        "server-project",
        "server",
        &request,
        body,
        &provider(s),
        Some(&confirm),
    );
    (r.status, r.body)
}

fn event(store: &RunStore, kind: &str, payload: Value) {
    let record = store.load(RUN).unwrap();
    let n = store.events(RUN).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{RUN}-{n}"),
            run_id: RUN.into(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: kind.into(),
            stage: Some("implement".into()),
            attempt: 1,
            occurred_at: "2026-11-19T10:00:00Z".into(),
            payload,
        })
        .unwrap();
}

/// Runs git in `dir` and gives back what it printed, trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The forge merges the run's MR: origin's `main` fast-forwards to the run's branch. Its sha.
fn merge_on_forge(s: &Setup) -> String {
    git(&s.forge, &["fetch", "-q", "origin"]);
    git(
        &s.forge,
        &["merge", "-q", "--ff-only", &format!("origin/orq/{RUN}")],
    );
    git(&s.forge, &["push", "-q", "origin", "main"]);
    git(&s.forge, &["rev-parse", "HEAD"])
}

/// What the review refresh saved about the run's MR, !12: `state` at the provider.
fn mr(s: &Setup, state: ProviderState) {
    let mut lifecycle = ReviewLifecycle::associate(
        "gitlab",
        "12",
        "https://gitlab.example/alpha/-/merge_requests/12",
        &"b".repeat(40),
    );
    lifecycle.state = state;
    lifecycle.observed = true;
    Store::open(&s.roots.database())
        .unwrap()
        .save_lifecycle(RUN, &lifecycle)
        .unwrap();
}

/// The run as its page reads it.
fn run_page(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", &format!("/api/projects/alpha/runs/{RUN}"), b"");
    assert_eq!(status, 200, "{body}");
    body
}

/// Each step of the clean-up card as (step, state).
fn steps(card: &Value) -> Vec<(String, String)> {
    card["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("no steps: {card}"))
        .iter()
        .map(|step| {
            (
                step["step"].as_str().unwrap().to_string(),
                step["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn a_merged_run_shows_the_clean_up_card_with_its_four_steps() {
    let s = setup("cleanup-card", |_| {});
    assert!(
        run_page(&s)["run"].get("cleanup").is_none(),
        "no card before the merge"
    );
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    let card = run_page(&s)["run"]["cleanup"].clone();
    assert_eq!(
        steps(&card),
        pairs(&[
            ("complete_feature", "pending"),
            ("delete_worktree", "pending"),
            ("delete_branch", "pending"),
            ("pull_target", "pending")
        ]),
        "{card}"
    );
    let _ = (&s.base, &s.repo, &s.worktree);
}

fn cleanup_path() -> String {
    format!("/api/projects/alpha/runs/{RUN}/cleanup")
}

/// Presses Clean up from a passkey session, confirmed.
fn clean_up(s: &Setup) -> Value {
    let (status, body) = send_with_passkey(
        s,
        "POST",
        &cleanup_path(),
        json!({"confirm": true}).to_string().as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    body
}

/// The kinds of the run's `cleanup.*` events, in the order recorded.
fn cleanup_events(s: &Setup) -> Vec<(String, Value)> {
    RunStore::new(&s.roots.state)
        .events(RUN)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind.starts_with("cleanup."))
        .map(|event| (event.kind, event.payload))
        .collect()
}

fn ledger_status(s: &Setup, feature: &str) -> String {
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(s.repo.join(".tdd-features.json")).unwrap())
            .unwrap();
    let entries = ledger
        .as_array()
        .cloned()
        .unwrap_or_else(|| ledger["features"].as_array().cloned().unwrap_or_default());
    entries
        .iter()
        .find(|entry| entry["featureName"] == feature)
        .unwrap()["status"]
        .as_str()
        .unwrap()
        .to_string()
}

fn branch_exists(s: &Setup) -> bool {
    Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/orq/{RUN}"),
        ])
        .current_dir(&s.repo)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn clean_up_runs_the_steps_in_order_and_records_each() {
    let s = setup("cleanup-runs", |_| {});
    let merged = merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    let answer = clean_up(&s);
    let done = pairs(&[
        ("complete_feature", "done"),
        ("delete_worktree", "done"),
        ("delete_branch", "done"),
        ("pull_target", "done"),
    ]);
    assert_eq!(steps(&answer), done, "{answer}");
    assert_eq!(ledger_status(&s, "COM-01"), "completed");
    assert_eq!(
        ledger_status(&s, "COM-02"),
        "pending",
        "only the run's feature"
    );
    assert!(!s.worktree.exists(), "the worktree is deleted");
    assert!(!branch_exists(&s), "the branch is deleted");
    assert_eq!(
        git(&s.repo, &["rev-parse", "refs/heads/main"]),
        merged,
        "main fast-forwarded"
    );
    assert_eq!(
        git(&s.repo, &["rev-parse", "HEAD"]),
        merged,
        "the checkout moved with it"
    );
    let kinds: Vec<String> = cleanup_events(&s)
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(
        kinds,
        [
            "cleanup.complete_feature",
            "cleanup.delete_worktree",
            "cleanup.delete_branch",
            "cleanup.pull_target"
        ]
    );
    assert_eq!(
        steps(&run_page(&s)["run"]["cleanup"]),
        done,
        "the card shows what was done"
    );
}

/// The step `step` of `card`.
fn step<'a>(card: &'a Value, step: &str) -> &'a Value {
    card["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|shown| shown["step"] == step)
        .unwrap()
}

#[test]
fn a_dirty_worktree_is_kept_and_the_steps_after_it_are_skipped_with_the_reason() {
    let s = setup("cleanup-dirty", |_| {});
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    std::fs::write(s.worktree.join("feature.txt"), "edited after the merge\n").unwrap();
    let answer = clean_up(&s);
    assert_eq!(
        steps(&answer),
        pairs(&[
            ("complete_feature", "done"),
            ("delete_worktree", "skipped"),
            ("delete_branch", "skipped"),
            ("pull_target", "skipped")
        ]),
        "{answer}"
    );
    let reason = step(&answer, "delete_worktree")["reason"].as_str().unwrap();
    assert!(
        reason.contains("uncommitted changes") && reason.contains("feature.txt"),
        "{reason}"
    );
    assert!(
        step(&answer, "delete_branch")["reason"]
            .as_str()
            .unwrap()
            .contains("deleting the worktree did not finish"),
        "{answer}"
    );
    assert!(s.worktree.exists(), "the worktree is kept");
    assert!(branch_exists(&s), "the branch is kept");
}

#[test]
fn a_branch_with_commits_not_on_the_target_is_not_deleted() {
    let s = setup("cleanup-unmerged", |_| {});
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    // The worktree is gone already, and the branch has a commit the merge never took.
    git(
        &s.repo,
        &["worktree", "remove", s.worktree.to_str().unwrap()],
    );
    git(&s.repo, &["checkout", "-q", &format!("orq/{RUN}")]);
    std::fs::write(s.repo.join("late.txt"), "late\n").unwrap();
    git(&s.repo, &["add", "late.txt"]);
    git(&s.repo, &["commit", "-q", "-m", "late"]);
    git(&s.repo, &["checkout", "-q", "main"]);
    let answer = clean_up(&s);
    assert_eq!(
        steps(&answer),
        pairs(&[
            ("complete_feature", "done"),
            ("delete_worktree", "done"),
            ("delete_branch", "skipped"),
            ("pull_target", "skipped")
        ]),
        "{answer}"
    );
    assert!(
        step(&answer, "delete_branch")["reason"]
            .as_str()
            .unwrap()
            .contains("commits not on origin/main"),
        "{answer}"
    );
    assert!(branch_exists(&s));
}

#[test]
fn a_branch_checked_out_in_another_checkout_is_not_deleted() {
    let s = setup("cleanup-checked-out", |_| {});
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    // The run's worktree is gone, and someone has the branch checked out in a worktree of their own.
    git(&s.repo, &["worktree", "remove", s.worktree.to_str().unwrap()]);
    git(&s.repo, &["fetch", "-q", "origin"]);
    let elsewhere = s.base.join("elsewhere");
    git(&s.repo, &["worktree", "add", "-q", elsewhere.to_str().unwrap(), &format!("orq/{RUN}")]);
    let answer = clean_up(&s);
    assert_eq!(step(&answer, "delete_branch")["state"], "skipped", "{answer}");
    let reason = step(&answer, "delete_branch")["reason"].as_str().unwrap();
    assert!(reason.contains("checked out"), "{reason}");
    assert!(branch_exists(&s), "the branch is kept");
    assert_eq!(git(&elsewhere, &["symbolic-ref", "--short", "HEAD"]), format!("orq/{RUN}"), "that checkout keeps its branch");
}

#[test]
fn a_retried_pull_fast_forwards_the_runs_own_target() {
    let s = setup("cleanup-retry-target", |_| {});
    let merged = merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    // The merge's own pull failed, and the project's warning is about another target.
    event(&RunStore::new(&s.roots.state), "target.pulled", json!({"outcome": "fetch_failed", "message": "could not fetch origin"}));
    let project = orq_git::project_id(&orq_git::resolve_project(&s.repo).unwrap());
    let warning = s.roots.state.join("target-pull").join(project);
    std::fs::create_dir_all(warning.parent().unwrap()).unwrap();
    let release = json!({"remote": "origin", "target": "release", "message": "release could not be pulled"}).to_string();
    std::fs::write(&warning, &release).unwrap();
    let answer = clean_up(&s);
    assert_eq!(step(&answer, "pull_target")["state"], "done", "{answer}");
    assert_eq!(git(&s.repo, &["rev-parse", "refs/heads/main"]), merged, "the run's target, main, is pulled");
    assert_eq!(std::fs::read_to_string(&warning).unwrap(), release, "the warning about release is kept");
}

#[test]
fn a_main_checkout_on_another_branch_is_not_fast_forwarded() {
    let s = setup("cleanup-other-branch", |_| {});
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    let before = git(&s.repo, &["rev-parse", "refs/heads/main"]);
    git(&s.repo, &["checkout", "-q", "-b", "side"]);
    let answer = clean_up(&s);
    assert_eq!(step(&answer, "pull_target")["state"], "skipped", "{answer}");
    assert_eq!(
        step(&answer, "pull_target")["reason"],
        "the main checkout is on side, not main"
    );
    assert_eq!(
        git(&s.repo, &["rev-parse", "refs/heads/main"]),
        before,
        "main was not moved"
    );
    assert!(
        !s.worktree.exists() && !branch_exists(&s),
        "the steps before it still ran"
    );
}

#[test]
fn the_ledger_is_snapshotted_before_the_feature_is_marked_completed() {
    let s = setup("cleanup-snapshot", |_| {});
    let before = std::fs::read_to_string(s.repo.join(".tdd-features.json")).unwrap();
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    clean_up(&s);
    let (_, completed) = cleanup_events(&s)
        .into_iter()
        .find(|(kind, _)| kind == "cleanup.complete_feature")
        .unwrap();
    let name = completed["snapshot"]
        .as_str()
        .unwrap_or_else(|| panic!("{completed}"));
    let project = RunStore::new(&s.roots.state).load(RUN).unwrap().project_id;
    let snapshot = std::fs::read_to_string(
        s.roots
            .state
            .join("ledger-snapshots")
            .join(project)
            .join(name),
    )
    .unwrap();
    assert_eq!(snapshot, before, "the snapshot is the ledger as it was");
    assert_eq!(ledger_status(&s, "COM-01"), "completed");
}

#[test]
fn a_second_clean_up_keeps_the_first_snapshot_and_does_not_redo_a_done_step() {
    let s = setup("cleanup-again", |_| {});
    let before = std::fs::read_to_string(s.repo.join(".tdd-features.json")).unwrap();
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    std::fs::write(s.worktree.join("feature.txt"), "edited after the merge\n").unwrap();
    clean_up(&s);
    let answer = clean_up(&s);
    assert_eq!(step(&answer, "complete_feature")["state"], "done", "{answer}");
    let project = RunStore::new(&s.roots.state).load(RUN).unwrap().project_id;
    let dir = s.roots.state.join("ledger-snapshots").join(project);
    let snapshots: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
    assert_eq!(std::fs::read_to_string(&snapshots[0]).unwrap(), before, "the snapshot is still the ledger before the clean-up");
    let completions = cleanup_events(&s).into_iter().filter(|(kind, _)| kind == "cleanup.complete_feature").count();
    assert_eq!(completions, 1, "marking the feature completed is not redone");
}

#[test]
fn clean_up_needs_remote_actions_a_passkey_a_confirmation_and_a_merged_mr() {
    let s = setup("cleanup-gated", |_| {});
    let confirmed = json!({"confirm": true}).to_string();
    assert_eq!(
        send_with_passkey(&s, "POST", &cleanup_path(), confirmed.as_bytes()).0,
        409,
        "not merged yet"
    );
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    assert_eq!(
        send(&s, "POST", &cleanup_path(), confirmed.as_bytes()).0,
        403,
        "the token alone may not"
    );
    assert_eq!(
        send_with_passkey(&s, "POST", &cleanup_path(), b"").0,
        400,
        "unconfirmed"
    );
    assert_eq!(
        send_with_passkey(
            &s,
            "POST",
            "/api/projects/alpha/runs/run-9-nobody/cleanup",
            confirmed.as_bytes()
        )
        .0,
        404
    );
    std::fs::write(s.roots.config_file(), "remote_actions = false\n").unwrap();
    assert_eq!(
        send_with_passkey(&s, "POST", &cleanup_path(), confirmed.as_bytes()).0,
        404
    );
    assert!(cleanup_events(&s).is_empty(), "nothing was done");
    assert!(s.worktree.exists());
    let audit = orq_core::remote_audit::read(&s.roots, 10).unwrap();
    assert!(
        audit
            .iter()
            .all(|entry| entry["action"] == "run.cleanup" && entry["outcome"] == "refused"),
        "{audit:?}"
    );
    assert_eq!(audit.len(), 5);
}

/// Has the project's notifier append each message to a file; the file.
fn notify_to_file(s: &Setup) -> PathBuf {
    let notes = s.base.join("notes.txt");
    let dir = s.repo.join(".git/orq");
    std::fs::create_dir_all(&dir).unwrap();
    let command = json!([
        "/bin/sh",
        "-c",
        format!(
            "printf '%s\\n' \"$NOTIFY_MESSAGE\" >> '{}'",
            notes.display()
        )
    ]);
    std::fs::write(dir.join("run.toml"), format!("notify_cmd = {command}\n")).unwrap();
    notes
}

#[test]
fn with_after_merge_auto_the_server_cleans_up_on_its_own_and_tells() {
    let s = setup("cleanup-auto", |settings| {
        settings.after_merge = AfterMerge::Auto
    });
    let notes = notify_to_file(&s);
    let merged = merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    provider(&s).review_looked_at(RUN);
    let card = run_page(&s)["run"]["cleanup"].clone();
    assert_eq!(
        steps(&card),
        pairs(&[
            ("complete_feature", "done"),
            ("delete_worktree", "done"),
            ("delete_branch", "done"),
            ("pull_target", "done")
        ]),
        "{card}"
    );
    assert_eq!(git(&s.repo, &["rev-parse", "refs/heads/main"]), merged);
    assert!(!s.worktree.exists());
    let told = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(
        told.contains("COM-01") && told.contains("cleaned up"),
        "{told:?}"
    );

    provider(&s).review_looked_at(RUN);
    assert_eq!(cleanup_events(&s).len(), 4, "one merge is cleaned up once");
}

#[test]
fn a_clean_up_that_cannot_record_its_steps_stops_and_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup("cleanup-unrecorded", |settings| settings.after_merge = AfterMerge::Auto);
    let notes = notify_to_file(&s);
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    let events = s.roots.state.join("runs").join(RUN).join("events.jsonl");
    assert!(events.is_file(), "{events:?}");
    std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o444)).unwrap();
    provider(&s).review_looked_at(RUN);
    assert!(s.worktree.exists() && branch_exists(&s), "nothing past the first unrecorded step is done");
    let told = std::fs::read_to_string(&notes).unwrap_or_default();
    assert!(told.contains("clean-up stopped") && told.contains("could not be recorded"), "{told:?}");

    let (status, body) = send_with_passkey(&s, "POST", &cleanup_path(), json!({"confirm": true}).to_string().as_bytes());
    assert_eq!(status, 500, "a pressed Clean up says it failed: {body}");
    assert!(s.worktree.exists(), "and goes no further");
    std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_clean_up_that_cannot_read_the_runs_history_changes_nothing() {
    let s = setup("cleanup-unreadable", |_| {});
    let before = std::fs::read_to_string(s.repo.join(".tdd-features.json")).unwrap();
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    let events = s.roots.state.join("runs").join(RUN).join("events.jsonl");
    let mut text = std::fs::read_to_string(&events).unwrap();
    text.push_str("not an event\n");
    std::fs::write(&events, text).unwrap();
    let (status, body) = send_with_passkey(&s, "POST", &cleanup_path(), json!({"confirm": true}).to_string().as_bytes());
    assert_eq!(status, 500, "{body}");
    assert_eq!(std::fs::read_to_string(s.repo.join(".tdd-features.json")).unwrap(), before, "the ledger is not touched");
    assert!(!s.roots.state.join("ledger-snapshots").exists(), "nor snapshotted");
    assert!(s.worktree.exists() && branch_exists(&s), "nor the worktree or branch deleted");
}

#[test]
fn with_after_merge_ask_nothing_is_cleaned_up_until_pressed() {
    let s = setup("cleanup-ask", |_| {});
    let notes = notify_to_file(&s);
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    provider(&s).review_looked_at(RUN);
    assert!(cleanup_events(&s).is_empty());
    assert!(s.worktree.exists() && branch_exists(&s));
    assert_eq!(ledger_status(&s, "COM-01"), "in_progress");
    assert!(!notes.exists(), "nothing to tell");
}

fn project_settings(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/settings", b"");
    assert_eq!(status, 200, "{body}");
    body["settings"].clone()
}

#[test]
fn after_merge_is_ask_by_default_and_saved_from_the_settings() {
    let s = setup("cleanup-setting", |_| {});
    let shown = project_settings(&s);
    assert_eq!(shown["after_merge"], "ask", "{shown}");
    let mut body = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": null, "revision": shown["revision"]});
    body["after_merge"] = json!("auto");
    let (status, reply) = send_with_passkey(
        &s,
        "POST",
        "/api/projects/alpha/settings",
        body.to_string().as_bytes(),
    );
    assert_eq!(status, 200, "{reply}");
    assert_eq!(project_settings(&s)["after_merge"], "auto");

    let mut bad = json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": null, "revision": project_settings(&s)["revision"]});
    bad["after_merge"] = json!("sometimes");
    assert_eq!(
        send(
            &s,
            "POST",
            "/api/projects/alpha/settings",
            bad.to_string().as_bytes()
        )
        .0,
        400
    );
    assert_eq!(
        project_settings(&s)["after_merge"],
        "auto",
        "a refused save writes nothing"
    );
}

#[test]
fn turning_after_merge_to_auto_needs_a_passkey_session() {
    let s = setup("cleanup-setting-passkey", |_| {});
    let body = |revision: &Value| json!({"implementer": null, "reviewer": null, "delivery": "review", "max_active_runs": null, "min_free_disk_gb": null, "after_merge": "auto", "revision": revision}).to_string();
    let path = "/api/projects/alpha/settings";
    let (status, reply) = send(&s, "POST", path, body(&project_settings(&s)["revision"]).as_bytes());
    assert_eq!(status, 403, "the token alone cannot turn it on: {reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("passkey"), "{reply}");
    assert_eq!(project_settings(&s)["after_merge"], "ask", "a refused save writes nothing");

    let (status, reply) = send_with_passkey(&s, "POST", path, body(&project_settings(&s)["revision"]).as_bytes());
    assert_eq!(status, 200, "{reply}");
    assert_eq!(project_settings(&s)["after_merge"], "auto");

    let (status, reply) = send(&s, "POST", path, body(&project_settings(&s)["revision"]).as_bytes());
    assert_eq!(status, 200, "the token may save other settings while it stays on: {reply}");
}

/// Project alpha's dashboard card.
fn project_card(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects", b"");
    assert_eq!(status, 200, "{body}");
    let list = body
        .as_array()
        .cloned()
        .unwrap_or_else(|| body["projects"].as_array().cloned().unwrap_or_default());
    list.into_iter()
        .find(|project| project["id"] == "alpha")
        .unwrap_or_else(|| panic!("no alpha: {body}"))
}

#[test]
fn the_project_card_lists_merged_runs_waiting_to_be_cleaned_up() {
    let s = setup("cleanup-project-card", |_| {});
    assert_eq!(project_card(&s)["cleanup"], json!([]), "nothing merged yet");
    merge_on_forge(&s);
    mr(&s, ProviderState::Merged);
    let card = project_card(&s);
    assert_eq!(
        card["cleanup"],
        json!([{"run": RUN, "feature": "COM-01", "review": "!12"}]),
        "{card}"
    );
    clean_up(&s);
    assert_eq!(
        project_card(&s)["cleanup"],
        json!([]),
        "a cleaned-up run is no longer listed"
    );
}

/// Project alpha's page, as its queue answer carries it.
fn project_page(s: &Setup) -> Value {
    let (status, body) = send(s, "GET", "/api/projects/alpha/queue", b"");
    assert_eq!(status, 200, "{body}");
    body["queue"].clone()
}

#[test]
fn commits_on_the_target_not_pushed_are_shown_on_the_project_card_and_page() {
    let s = setup("cleanup-unpushed", |_| {});
    assert!(
        project_card(&s)["unpushed"].is_null(),
        "nothing unpushed yet: {}",
        project_card(&s)
    );
    for subject in ["tidy readme", "rotate glpat-abcdefghijklmnopqrst"] {
        std::fs::write(s.repo.join("README"), subject).unwrap();
        git(&s.repo, &["commit", "-q", "-am", subject]);
    }
    let head = std::process::Command::new("git")
        .args(["rev-parse", "main"])
        .current_dir(&s.repo)
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_string();
    // The line also names the commit a push would send and the origin's provider, which a
    // local origin does not have.
    let shown = json!({"target": "main", "sha": head, "count": 2, "message": "2 commits on main not pushed", "subjects": ["rotate [REDACTED]", "tidy readme"], "provider": null});
    assert_eq!(project_card(&s)["unpushed"], shown);
    assert_eq!(project_page(&s)["unpushed"], shown);
    git(&s.repo, &["push", "-q", "origin", "main"]);
    assert!(
        project_card(&s)["unpushed"].is_null(),
        "pushed: {}",
        project_card(&s)
    );
    assert!(project_page(&s).get("unpushed").is_none());
}
