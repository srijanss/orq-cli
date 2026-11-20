//! A project whose `.orq/run.toml` has `[release] release_on_merge = "auto"` gets a release commit after
//! each merge-local merge the queue settles: the level comes from one non-interactive agent call (a
//! commit-subject fallback when it fails), the commit is made in a worktree and fast-forwarded into the
//! target, and the after-merge commands then run at it. Driven here with a fake launcher, a fake agent
//! and fake release commands against a temporary project.

mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orq_core::project_settings::{Delivery, ProjectSettings};
use orq_core::remote_inbox::{handle_with_body, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_core::store::Store;
use orq_orchestrator::{Event, RunRecord, RunStore};
use orq_remote::projects::{LaunchError, QueueRound, ReleaseAgent, ResumeRequest, RunLauncher, StoreProjects};
use serde_json::{json, Value};

struct Launcher {
    state: PathBuf,
    delivery: Delivery,
    started: Mutex<Vec<String>>,
}

impl RunLauncher for Launcher {
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError> {
        let mut started = self.started.lock().unwrap();
        started.push(feature.to_string());
        let run = format!("run-{}-{feature}", started.len());
        seed_run(&self.state, repo, &run, feature, self.delivery);
        Ok(run)
    }

    fn resume_with(&self, _repo: &Path, _run: &str, _request: &ResumeRequest) -> Result<String, LaunchError> {
        Err(LaunchError::Failed("not used".into()))
    }
}

/// An agent that answers every call with `reply`, remembering the argv it was given.
struct FakeAgent {
    reply: Result<String, String>,
    /// How long it takes to answer.
    delay: Duration,
    asked: Mutex<Vec<Vec<String>>>,
    /// Something that happens in the project while the agent is thinking.
    during: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ReleaseAgent for FakeAgent {
    fn ask(&self, _harness: &str, argv: &[String]) -> Result<String, String> {
        self.asked.lock().unwrap().push(argv.to_vec());
        if let Some(during) = self.during.lock().unwrap().take() {
            during();
        }
        std::thread::sleep(self.delay);
        self.reply.clone()
    }
}

fn replying(reply: Result<String, String>, delay: Duration) -> Arc<FakeAgent> {
    Arc::new(FakeAgent { reply, delay, asked: Mutex::default(), during: Mutex::default() })
}

fn answering(level: &str) -> Arc<FakeAgent> {
    replying(Ok(json!({"level": level, "reason": format!("because {level}")}).to_string()), Duration::ZERO)
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
    launcher: Arc<Launcher>,
    agent: Arc<FakeAgent>,
    repo: PathBuf,
    marks: PathBuf,
}

/// A project `alpha` on `main` at version 0.1.2, merge-local, with COM-01 and COM-02 queued, whose
/// `.orq/run.toml` releases automatically and whose release command writes a `Cargo.lock`. The after-merge
/// command records the version it sees.
fn setup(label: &str, agent: Arc<FakeAgent>, release: &str) -> Setup {
    setup_delivering(label, agent, release, Delivery::MergeLocal)
}

fn setup_delivering(label: &str, agent: Arc<FakeAgent>, release: &str, delivery: Delivery) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots {
        data: base.join("data"),
        config: base.join("config"),
        state: base.join("state"),
        cache: base.join("cache"),
    };
    roots.create_all().unwrap();
    std::fs::write(roots.config_file(), "remote_actions = true\n").unwrap();
    let launcher = Arc::new(Launcher { state: roots.state.clone(), delivery, started: Mutex::default() });
    let scan = base.join("code");
    let repo = scan.join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    let marks = base.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("Cargo.toml"), "[workspace.package]\nversion = \"0.1.2\"\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".tdd-*.json\n.claude/\n.orq/\n").unwrap();
    git(&repo, &["add", "Cargo.toml", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "start"]);
    let ledger = json!([{"featureName": "COM-01", "description": "add the thing", "dependsOn": [], "status": "pending"}, {"featureName": "COM-02", "dependsOn": [], "status": "pending"}]);
    std::fs::write(repo.join(".tdd-features.json"), ledger.to_string()).unwrap();
    std::fs::create_dir_all(repo.join(".orq")).unwrap();
    let release_command = format!(
        "git -C {repo} status --porcelain > {marks}/main-status; echo locked > Cargo.lock",
        repo = repo.display(),
        marks = marks.display()
    );
    let after = format!("cat Cargo.toml > {marks}/after-version; echo after >> {marks}/order", marks = marks.display());
    std::fs::write(
        repo.join(".orq/run.toml"),
        format!("[release]\nrelease_on_merge = \"{release}\"\ncommands = [[\"sh\", \"-c\", \"{release_command}\"]]\n\n[after_merge]\ncommands = [[\"sh\", \"-c\", \"{after}\"]]\n"),
    )
    .unwrap();
    let settings = ProjectSettings { delivery, max_active_runs: Some(1), ..ProjectSettings::default() };
    let mut store = Store::open(&roots.database()).unwrap();
    store.save_project_settings(&repo, 0, &settings).unwrap();
    let s = Setup { scan, roots, launcher, agent, repo, marks };
    let (status, body) = send(&s, "POST", "/api/projects/alpha/queue", json!({"features": ["COM-01", "COM-02"]}).to_string().as_bytes());
    assert_eq!(status, 200, "{body}");
    s
}

fn provider(s: &Setup) -> StoreProjects {
    StoreProjects::new(s.roots.clone(), s.scan.clone()).with_launcher(s.launcher.clone()).with_release_agent(s.agent.clone())
}

fn send(s: &Setup, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method, path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with_body(&s.roots, &config, "server-project", "server", &request, body, &provider(s));
    (r.status, r.body)
}

fn round(s: &Setup) {
    provider(s).queue_round(&QueueRound { minute_of_day: 12 * 60, restart_pending: false });
}

fn event(store: &RunStore, run: &str, kind: &str, payload: Value) {
    let record = store.load(run).unwrap();
    let n = store.events(run).unwrap().len();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("{run}-{n}"),
            run_id: run.into(),
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

fn seed_run(state: &Path, repo: &Path, run: &str, feature: &str, delivery: Delivery) {
    let store = RunStore::new(state);
    let record = RunRecord {
        delivery,
        run_id: run.into(),
        project_id: orq_git::project_id(&orq_git::resolve_project(repo).unwrap()),
        feature_id: feature.into(),
        remote: "origin".into(),
        provider: "gitlab".into(),
        target_branch: "main".into(),
        base_sha: git(repo, &["rev-parse", "refs/heads/main"]),
        branch: format!("orq/{run}"),
        worktree: repo.join(".claude/worktrees").join(run),
        repository: String::new(),
        profiles: Default::default(),
    };
    store.create(&record).unwrap();
    event(&store, run, "stage.started", json!({}));
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn wait_for(s: &Setup, run: &str, kind: &str) -> Option<Event> {
    let store = RunStore::new(&s.roots.state);
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(found) = store.events(run).unwrap().into_iter().rev().find(|e| e.kind == kind) {
            return Some(found);
        }
        if Instant::now() > until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Starts COM-01, lets a round see it going, and lands its merge as a commit of `subject` on main; the
/// round that settles it is left to the caller.
fn landed_with(s: &Setup, subject: &str) -> (String, String) {
    round(s);
    round(s);
    std::fs::write(s.repo.join("thing.txt"), subject).unwrap();
    git(&s.repo, &["add", "thing.txt"]);
    git(&s.repo, &["commit", "-q", "-m", subject]);
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    let run = "run-1-COM-01".to_string();
    let store = RunStore::new(&s.roots.state);
    event(&store, &run, "merge.completed", json!({"head_sha": head, "target_branch": "main"}));
    event(&store, &run, "run.completed", json!({"ending": "merged"}));
    (run, head)
}

/// [`landed_with`], then the round that settles it.
fn merged_with(s: &Setup, subject: &str) -> (String, String) {
    let landed = landed_with(s, subject);
    round(s);
    landed
}

fn events_of(s: &Setup, run: &str, kind: &str) -> Vec<Event> {
    RunStore::new(&s.roots.state).events(run).unwrap().into_iter().filter(|e| e.kind == kind).collect()
}

/// Waits long enough for something that would have happened to have done so.
fn settle_time() {
    std::thread::sleep(Duration::from_millis(1500));
}

fn version_of(s: &Setup) -> String {
    std::fs::read_to_string(s.repo.join("Cargo.toml")).unwrap()
}

#[test]
fn a_patch_release_bumps_the_version_in_a_commit_fast_forwarded_into_main() {
    let s = setup("patch", answering("patch"), "auto");
    let (run, merged) = merged_with(&s, "fix: the thing");
    let created = wait_for(&s, &run, "release.created").expect("the release was recorded");
    assert_eq!(created.payload["version"], "0.1.3");
    assert_eq!(created.payload["from"], "0.1.2");
    assert_eq!(created.payload["level"], "patch");
    assert_eq!(created.payload["decided_by"], "claude");
    assert!(version_of(&s).contains("version = \"0.1.3\""));
    assert_eq!(git(&s.repo, &["rev-parse", "HEAD~1"]), merged);
    assert_eq!(git(&s.repo, &["show", "-s", "--format=%s", "HEAD"]), "chore(release): COM-01 0.1.3");
    let body = git(&s.repo, &["show", "-s", "--format=%b", "HEAD"]);
    for line in ["level: patch", "reason: because patch", "feature: COM-01", &format!("run: {run}")] {
        assert!(body.lines().any(|l| l == line), "{line} in {body}");
    }
    let files = git(&s.repo, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.lines().collect::<Vec<_>>(), ["Cargo.lock", "Cargo.toml"]);
    assert_eq!(git(&s.repo, &["branch", "--list", "release/*"]), "");
    assert_eq!(git(&s.repo, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1);
    assert_eq!(std::fs::read_to_string(s.marks.join("main-status")).unwrap(), "");
}

#[test]
fn a_minor_or_major_level_gives_0_2_0_and_a_major_is_stored_as_proposed() {
    for level in ["minor", "major"] {
        let s = setup(&format!("level-{level}"), answering(level), "auto");
        let (run, _) = merged_with(&s, "feat: the thing");
        let created = wait_for(&s, &run, "release.created").expect("the release was recorded");
        assert_eq!(created.payload["version"], "0.2.0", "{level}");
        assert_eq!(created.payload["level"], "minor", "{level}");
        assert_eq!(created.payload["proposed"], level);
        assert!(version_of(&s).contains("version = \"0.2.0\""));
        let body = git(&s.repo, &["show", "-s", "--format=%b", "HEAD"]);
        assert!(body.lines().any(|l| l == "level: minor"), "{body}");
    }
}

#[test]
fn the_after_merge_commands_run_once_after_the_release_commit_and_see_the_new_version() {
    let s = setup("after-merge", answering("patch"), "auto");
    let (run, _) = merged_with(&s, "fix: the thing");
    let done = wait_for(&s, &run, "after_merge.commands").expect("the commands were recorded");
    assert_eq!(done.payload["ran"], true);
    assert_eq!(done.payload["merged"], git(&s.repo, &["rev-parse", "HEAD~1"]).as_str());
    assert!(std::fs::read_to_string(s.marks.join("after-version")).unwrap().contains("version = \"0.1.3\""));
    round(&s);
    settle_time();
    assert_eq!(std::fs::read_to_string(s.marks.join("order")).unwrap().lines().count(), 1);
    assert_eq!(events_of(&s, &run, "release.created").len(), 1);
    assert_eq!(s.agent.asked.lock().unwrap().len(), 1, "a second settle asked nothing");
}

#[test]
fn a_failing_slow_or_invalid_agent_falls_back_to_the_commit_subjects() {
    let cases = [
        (Err("timed out after 120 seconds".to_string()), "feat: add a thing", "minor", "0.2.0"),
        (Err("exited with 1".to_string()), "fix: mend a thing", "patch", "0.1.3"),
        (Ok("I think minor".to_string()), "fix: mend a thing", "patch", "0.1.3"),
        (Ok("{\"level\":\"huge\",\"reason\":\"x\"}".to_string()), "feat(web): add a thing", "minor", "0.2.0"),
    ];
    for (i, (reply, subject, level, version)) in cases.into_iter().enumerate() {
        let s = setup(&format!("fallback-{i}"), replying(reply, Duration::ZERO), "auto");
        let (run, _) = merged_with(&s, subject);
        let created = wait_for(&s, &run, "release.created").expect("the release was recorded");
        assert_eq!(created.payload["decided_by"], "fallback", "{i}");
        assert_eq!(created.payload["level"], level, "{i}");
        assert_eq!(created.payload["version"], version, "{i}");
        assert!(created.payload["reason"].as_str().unwrap().starts_with("chosen by commit subjects: "), "{i}");
    }
}

#[test]
fn a_dirty_checkout_or_a_moved_target_is_refused_and_leaves_the_merge_and_no_worktree() {
    for moved in [false, true] {
        let s = setup(&format!("refused-{moved}"), answering("patch"), "auto");
        let (run, merged) = landed_with(&s, "fix: the thing");
        if moved {
            std::fs::write(s.repo.join("other.txt"), "x").unwrap();
            git(&s.repo, &["add", "other.txt"]);
            git(&s.repo, &["commit", "-q", "-m", "chore: later"]);
        } else {
            std::fs::write(s.repo.join("thing.txt"), "edited").unwrap();
        }
        round(&s);
        let refused = wait_for(&s, &run, "release.refused").expect("the refusal was recorded");
        assert!(!refused.payload["why"].as_str().unwrap().is_empty());
        assert!(events_of(&s, &run, "release.created").is_empty());
        assert!(version_of(&s).contains("version = \"0.1.2\""));
        assert_eq!(git(&s.repo, &["rev-parse", if moved { "HEAD~1" } else { "HEAD" }]), merged);
        assert_eq!(git(&s.repo, &["branch", "--list", "release/*"]), "");
        assert_eq!(git(&s.repo, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1);
        if !moved {
            assert_eq!(std::fs::read_to_string(s.repo.join("thing.txt")).unwrap(), "edited");
        }
        let after = wait_for(&s, &run, "after_merge.commands").expect("the after-merge commands still ran or were skipped");
        assert_eq!(after.payload["merged"], merged.as_str());
    }
}

#[test]
fn a_version_file_that_is_a_symlink_is_refused_and_what_it_points_at_is_untouched() {
    let s = setup("symlink", answering("patch"), "auto");
    let outside = s.marks.join("external.toml");
    std::fs::write(&outside, "[package]\nversion = \"0.1.2\"\n").unwrap();
    std::os::unix::fs::symlink(&outside, s.repo.join("link.toml")).unwrap();
    git(&s.repo, &["add", "link.toml"]);
    git(&s.repo, &["commit", "-q", "-m", "chore: link"]);
    std::fs::write(s.repo.join(".orq/run.toml"), "[release]\nrelease_on_merge = \"auto\"\nversion_file = \"link.toml\"\nversion_key = \"package.version\"\n").unwrap();
    let (run, _) = merged_with(&s, "fix: the thing");
    let refused = wait_for(&s, &run, "release.refused").expect("the refusal was recorded");
    assert!(refused.payload["why"].as_str().unwrap().contains("symlink"), "{refused:?}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "[package]\nversion = \"0.1.2\"\n");
    assert_eq!(git(&s.repo, &["branch", "--list", "release/*"]), "");
}

#[test]
fn a_target_that_changes_while_the_agent_thinks_is_refused_and_no_branch_is_moved() {
    let agent = answering("patch");
    let s = setup("changes-meanwhile", agent.clone(), "auto");
    let repo = s.repo.clone();
    *agent.during.lock().unwrap() = Some(Box::new(move || {
        git(&repo, &["checkout", "-q", "-b", "other"]);
    }));
    let (run, merged) = merged_with(&s, "fix: the thing");
    let refused = wait_for(&s, &run, "release.refused").expect("the refusal was recorded");
    assert!(refused.payload["why"].as_str().unwrap().contains("other"), "{refused:?}");
    assert!(events_of(&s, &run, "release.created").is_empty());
    assert_eq!(git(&s.repo, &["rev-parse", "main"]), merged);
    assert_eq!(git(&s.repo, &["rev-parse", "HEAD"]), merged);
    assert_eq!(git(&s.repo, &["branch", "--list", "release/*"]), "");
}

#[test]
fn a_harness_that_exits_but_leaves_its_output_open_is_cut_off_by_the_deadline() {
    use std::os::unix::fs::PermissionsExt;
    let dir = web_deps::private_temp_dir("lingering-harness");
    let script = dir.join("harness.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 30 &\necho '{}'\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = Instant::now();
    let answer = orq_remote::projects::HarnessAgent::with_timeout(Duration::from_secs(1)).ask(script.to_str().unwrap(), &[]);
    assert!(started.elapsed() < Duration::from_secs(10), "it waited {:?}", started.elapsed());
    let why = answer.expect_err("an answer that never ends is not an answer");
    assert!(why.contains("timed out"), "{why}");
}

#[test]
fn a_merge_with_a_huge_stat_and_diff_gives_the_agent_a_bounded_prompt() {
    let s = setup("huge-evidence", answering("patch"), "auto");
    round(&s);
    round(&s);
    for i in 0..2500 {
        std::fs::write(s.repo.join(format!("a-file-with-a-rather-long-name-to-make-the-stat-big-{i:05}.txt")), format!("line {i}\n").repeat(40)).unwrap();
    }
    git(&s.repo, &["add", "."]);
    git(&s.repo, &["commit", "-q", "-m", "fix: a huge change"]);
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    let run = "run-1-COM-01";
    let store = RunStore::new(&s.roots.state);
    event(&store, run, "merge.completed", json!({"head_sha": head, "target_branch": "main"}));
    event(&store, run, "run.completed", json!({"ending": "merged"}));
    round(&s);
    wait_for(&s, run, "release.created").expect("the release was recorded");
    let asked = s.agent.asked.lock().unwrap();
    let prompt = asked[0].last().unwrap();
    assert!(prompt.chars().count() < 90_000, "the prompt has {} characters", prompt.chars().count());
    assert!(prompt.contains("fix: a huge change"), "the subjects are still in it");
}

#[test]
fn a_failing_release_command_refuses_the_release_and_leaves_no_worktree_branch_or_commit() {
    let s = setup("failing-command", answering("patch"), "auto");
    let after = format!("echo after >> {marks}/order", marks = s.marks.display());
    std::fs::write(
        s.repo.join(".orq/run.toml"),
        format!("[release]\nrelease_on_merge = \"auto\"\ncommands = [[\"sh\", \"-c\", \"echo half > Cargo.lock; exit 1\"]]\n\n[after_merge]\ncommands = [[\"sh\", \"-c\", \"{after}\"]]\n"),
    )
    .unwrap();
    let (run, merged) = merged_with(&s, "fix: the thing");
    let refused = wait_for(&s, &run, "release.refused").expect("the refusal was recorded");
    assert!(refused.payload["why"].as_str().unwrap().contains("failed"), "{}", refused.payload);
    assert!(events_of(&s, &run, "release.created").is_empty());
    assert_eq!(git(&s.repo, &["rev-parse", "HEAD"]), merged, "the merge is as it was");
    assert!(version_of(&s).contains("version = \"0.1.2\""));
    assert_eq!(git(&s.repo, &["status", "--porcelain", "--untracked-files=no"]), "");
    assert_eq!(git(&s.repo, &["branch", "--list", "release/*"]), "");
    assert_eq!(git(&s.repo, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1);
    assert!(!s.repo.join("Cargo.lock").exists(), "what the command wrote stayed in the release worktree");
    wait_for(&s, &run, "after_merge.commands").expect("the after-merge commands still ran");
    assert_eq!(std::fs::read_to_string(s.marks.join("order")).unwrap().lines().count(), 1);
}

#[test]
fn a_feature_commit_behind_many_long_subjects_still_makes_the_fallback_minor() {
    let s = setup("long-subjects", replying(Err("exited with 1".to_string()), Duration::ZERO), "auto");
    round(&s);
    round(&s);
    git(&s.repo, &["commit", "-q", "--allow-empty", "-m", "feat: the one that matters"]);
    for i in 0..30 {
        git(&s.repo, &["commit", "-q", "--allow-empty", "-m", &format!("fix: {i} {}", "x".repeat(500))]);
    }
    let head = git(&s.repo, &["rev-parse", "HEAD"]);
    let run = "run-1-COM-01";
    let store = RunStore::new(&s.roots.state);
    event(&store, run, "merge.completed", json!({"head_sha": head, "target_branch": "main"}));
    event(&store, run, "run.completed", json!({"ending": "merged"}));
    round(&s);
    let created = wait_for(&s, run, "release.created").expect("the release was recorded");
    assert_eq!(created.payload["decided_by"], "fallback");
    assert_eq!(created.payload["level"], "minor");
    assert_eq!(created.payload["version"], "0.2.0");
}

#[test]
fn a_release_that_is_off_or_a_review_delivery_does_nothing() {
    for (off, delivery) in [(true, Delivery::MergeLocal), (false, Delivery::Review)] {
        let agent = answering("patch");
        let s = setup_delivering("nothing", agent, if off { "off" } else { "auto" }, delivery);
        let (run, _) = merged_with(&s, "fix: the thing");
        settle_time();
        assert!(events_of(&s, &run, "release.created").is_empty());
        assert!(events_of(&s, &run, "release.refused").is_empty());
        assert!(s.agent.asked.lock().unwrap().is_empty());
        assert!(version_of(&s).contains("version = \"0.1.2\""));
        assert_eq!(git(&s.repo, &["log", "--format=%s", "-1"]), "fix: the thing");
    }
}

#[test]
fn the_queue_starts_nothing_until_the_release_landed() {
    let s = setup("queue-waits", replying(Ok("{\"level\":\"patch\",\"reason\":\"r\"}".to_string()), Duration::from_millis(1500)), "auto");
    let (run, _) = merged_with(&s, "fix: the thing");
    round(&s);
    assert_eq!(s.launcher.started.lock().unwrap().len(), 1, "COM-02 waited for the release");
    wait_for(&s, &run, "release.created").expect("the release landed");
    // The gate lifts a moment after the release is recorded, so the rounds that follow may still wait.
    let until = Instant::now() + Duration::from_secs(10);
    while s.launcher.started.lock().unwrap().len() < 2 && Instant::now() < until {
        round(&s);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(s.launcher.started.lock().unwrap().as_slice(), ["COM-01", "COM-02"]);
}
