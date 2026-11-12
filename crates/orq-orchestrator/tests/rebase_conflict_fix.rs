use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{
    fix_rebase_conflicts, rebase_onto_target, ConflictFixOutcome, ImplementSession, RebaseOutcome,
    RunRecord, RunStore,
};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct World {
    store: RunStore,
    root: PathBuf,
    worktree: PathBuf,
}

/// A run whose branch conflicts with the target on `a.txt`; the rebase has
/// already stopped, so the worktree is mid-rebase when `conflicted` is true.
fn world(tag: &str, conflicted: bool) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-conflict-fix-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let remote = root.join("remote.git");
    git(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    let worktree = root.join("worktree");
    git(
        &root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
    );
    for (key, value) in [
        ("user.name", "t"),
        ("user.email", "t@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        git(&worktree, &["config", key, value]);
    }
    fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "base"]);
    git(&worktree, &["push", "-q", "origin", "HEAD:main"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    git(&worktree, &["checkout", "-q", "-b", "fix--COM-02"]);
    fs::write(worktree.join("a.txt"), "mine\n").unwrap();
    git(&worktree, &["commit", "-q", "-am", "edit a"]);
    let other = root.join("other");
    git(
        &root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join("a.txt"), "theirs\n").unwrap();
    git(&other, &["commit", "-q", "-am", "target a"]);
    git(&other, &["push", "-q", "origin", "HEAD:main"]);

    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: remote.to_str().unwrap().to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: base,
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    if conflicted {
        let outcome = rebase_onto_target(&store, "run-1", true).unwrap();
        assert!(matches!(outcome, RebaseOutcome::Conflict { .. }));
    }
    World {
        store,
        root,
        worktree,
    }
}

fn run_agent(w: &World, script: &str) -> Result<ConflictFixOutcome, orq_orchestrator::StoreError> {
    let args = vec!["-c".to_string(), script.to_string()];
    let log = w.root.join("fix.log");
    let session = ImplementSession {
        program: "sh",
        args: &args,
        env: &[],
        cwd: &w.worktree,
        session_id: None,
        log: &log,
        terminal: false,
        exit_marker: None,
    };
    fix_rebase_conflicts(&w.store, "run-1", &session)
}

fn kinds(w: &World) -> Vec<(String, Option<String>)> {
    w.store
        .events("run-1")
        .unwrap()
        .into_iter()
        .map(|e| (e.kind, e.stage))
        .collect()
}

#[test]
fn a_session_that_finishes_the_rebase_unblocks_the_run() {
    let w = world("resolved", true);

    let outcome = run_agent(
        &w,
        "printf 'merged\\n' > a.txt && git add a.txt && GIT_EDITOR=true git rebase --continue",
    )
    .unwrap();

    assert_eq!(outcome, ConflictFixOutcome::Resolved);
    assert_eq!(
        git(&w.worktree, &["symbolic-ref", "HEAD"]),
        "refs/heads/fix--COM-02"
    );
    assert_eq!(
        fs::read_to_string(w.worktree.join("a.txt")).unwrap(),
        "merged\n"
    );
    let events = kinds(&w);
    assert_eq!(
        events.last().unwrap(),
        &("rebase.completed".to_string(), Some("rebase".to_string()))
    );
    assert!(events.contains(&("stage.started".to_string(), Some("rebase_fix".to_string()))));
    let target = git(&w.root.join("remote.git"), &["rev-parse", "main"]);
    let completed = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(
        completed.payload["target_sha"],
        target.as_str(),
        "the captured target is recorded"
    );
}

#[test]
fn a_session_that_leaves_the_conflict_keeps_the_worktree_mid_rebase_and_the_run_blocked_with_a_hint(
) {
    let w = world("unresolved", true);

    let outcome = run_agent(&w, "true").unwrap();

    let ConflictFixOutcome::StillBlocked { reason } = outcome else {
        panic!("expected blocked, got {outcome:?}")
    };
    assert!(reason.contains("git rebase --continue"), "{reason}");
    assert!(reason.contains(w.worktree.to_str().unwrap()), "{reason}");
    assert!(git(&w.worktree, &["status", "--porcelain=v1"]).contains("UU a.txt"));
    let last = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(last.kind, "run.blocked");
    assert_eq!(last.stage.as_deref(), Some("rebase"));
    assert_eq!(last.payload["reason"], reason.as_str());
}

#[test]
fn a_session_that_aborts_or_quits_the_rebase_does_not_count_as_resolved() {
    for (tag, script) in [
        ("abort", "git rebase --abort"),
        ("quit", "git rebase --quit"),
    ] {
        let w = world(tag, true);

        let outcome = run_agent(&w, script).unwrap();

        let ConflictFixOutcome::StillBlocked { reason } = outcome else {
            panic!("{tag}: expected blocked, got {outcome:?}")
        };
        assert!(
            reason.contains(w.worktree.to_str().unwrap()),
            "{tag}: {reason}"
        );
        let events = kinds(&w);
        assert!(
            !events.iter().any(|(kind, _)| kind == "rebase.completed"),
            "{tag}: {events:?}"
        );
        assert_eq!(events.last().unwrap().0, "run.blocked", "{tag}");
    }
}

#[test]
fn a_session_that_resets_the_run_branch_to_the_target_has_lost_the_work_and_is_not_resolved() {
    let w = world("reset-to-target", true);

    let outcome = run_agent(
        &w,
        "onto=$(cat \"$(git rev-parse --git-path rebase-merge/onto)\") && git rebase --abort && git reset -q --hard $onto",
    )
    .unwrap();

    let ConflictFixOutcome::StillBlocked { reason } = outcome else {
        panic!("expected blocked, got {outcome:?}")
    };
    assert!(reason.contains(w.worktree.to_str().unwrap()), "{reason}");
    assert!(!event_kinds(&w).contains(&"rebase.completed".to_string()));
}

fn event_kinds(w: &World) -> Vec<String> {
    kinds(w).into_iter().map(|(kind, _)| kind).collect()
}

#[test]
fn a_failing_session_with_the_conflict_left_keeps_the_run_blocked_and_records_the_failure() {
    let w = world("exit-1", true);

    let outcome = run_agent(&w, "exit 3").unwrap();

    let ConflictFixOutcome::StillBlocked { reason } = outcome else {
        panic!("expected blocked, got {outcome:?}")
    };
    assert!(reason.contains("git rebase --continue"), "{reason}");
    assert_eq!(
        event_kinds(&w),
        [
            "run.blocked",
            "stage.started",
            "stage.failed",
            "run.blocked"
        ]
    );
    assert!(git(&w.worktree, &["status", "--porcelain=v1"]).contains("UU a.txt"));
}

#[test]
fn a_session_that_cannot_start_keeps_the_run_blocked_and_records_the_failure() {
    let w = world("no-program", true);
    let log = w.root.join("fix.log");
    let session = ImplementSession {
        program: "/nonexistent/orq-agent",
        args: &[],
        env: &[],
        cwd: &w.worktree,
        session_id: None,
        log: &log,
        terminal: false,
        exit_marker: None,
    };

    let outcome = fix_rebase_conflicts(&w.store, "run-1", &session).unwrap();

    let ConflictFixOutcome::StillBlocked { reason } = outcome else {
        panic!("expected blocked, got {outcome:?}")
    };
    assert!(reason.contains("git rebase --continue"), "{reason}");
    assert_eq!(
        event_kinds(&w),
        [
            "run.blocked",
            "stage.started",
            "stage.failed",
            "run.blocked"
        ]
    );
    assert!(git(&w.worktree, &["status", "--porcelain=v1"]).contains("UU a.txt"));
}

#[test]
fn a_session_that_finishes_the_rebase_and_then_exits_nonzero_is_still_resolved() {
    let w = world("done-then-fail", true);

    let outcome = run_agent(
        &w,
        "printf 'merged\\n' > a.txt && git add a.txt && GIT_EDITOR=true git rebase --continue; exit 1",
    )
    .unwrap();

    assert_eq!(outcome, ConflictFixOutcome::Resolved);
    assert_eq!(
        event_kinds(&w),
        [
            "run.blocked",
            "stage.started",
            "stage.failed",
            "rebase.completed"
        ]
    );
}

/// Like `run_agent`, for another run and worktree.
fn run_agent_in(
    w: &World,
    run_id: &str,
    cwd: &Path,
    script: &str,
) -> Result<ConflictFixOutcome, orq_orchestrator::StoreError> {
    let args = vec!["-c".to_string(), script.to_string()];
    let log = w.root.join(format!("{run_id}.log"));
    let session = ImplementSession {
        program: "sh",
        args: &args,
        env: &[],
        cwd,
        session_id: None,
        log: &log,
        terminal: false,
        exit_marker: None,
    };
    fix_rebase_conflicts(&w.store, run_id, &session)
}

/// Start a rebase by hand (the world's rebase already stopped, so reset first).
fn restart_rebase_with(w: &World, flag: &str) {
    git(&w.worktree, &["rebase", "--abort"]);
    git(&w.worktree, &["fetch", "-q", "origin"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(&w.worktree)
        .args(["rebase", flag, "origin/main"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the rebase must stop on the conflict"
    );
}

#[test]
fn the_apply_backend_is_resolved_the_same_way() {
    let w = world("apply", true);
    restart_rebase_with(&w, "--apply");
    assert!(
        w.worktree.join(".git/rebase-apply").is_dir()
            && !w.worktree.join(".git/rebase-merge").exists()
    );

    let outcome = run_agent(
        &w,
        "printf 'merged\\n' > a.txt && git add a.txt && GIT_EDITOR=true git rebase --continue",
    )
    .unwrap();

    assert_eq!(outcome, ConflictFixOutcome::Resolved);
}

#[test]
fn missing_or_empty_onto_metadata_is_an_error_and_no_session_starts() {
    for (tag, content) in [("missing", None), ("empty", Some(""))] {
        let w = world(&format!("onto-{tag}"), true);
        let onto = w.worktree.join(".git/rebase-merge/onto");
        match content {
            None => fs::remove_file(&onto).unwrap(),
            Some(text) => fs::write(&onto, text).unwrap(),
        }
        let marker = w.root.join("ran");
        let events_before = w.store.events("run-1").unwrap().len();

        let result = run_agent(&w, &format!("touch {}", marker.display()));

        assert!(result.is_err(), "{tag}");
        assert!(!marker.exists(), "{tag}: the session must not start");
        assert_eq!(
            w.store.events("run-1").unwrap().len(),
            events_before,
            "{tag}: nothing recorded"
        );
    }
}

#[test]
fn a_linked_worktree_is_resolved_through_its_own_git_dir() {
    let w = world("linked", false);
    let second = w.root.join("second");
    git(
        &w.worktree,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fix--COM-03",
            second.to_str().unwrap(),
            "fix--COM-02",
        ],
    );
    git(&second, &["fetch", "-q", "origin"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(&second)
        .args(["rebase", "origin/main"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the rebase must stop on the conflict"
    );
    let mut record = w.store.load("run-1").unwrap();
    record.run_id = "run-2".to_string();
    record.branch = "fix--COM-03".to_string();
    record.worktree = second.clone();
    w.store.create(&record).unwrap();

    let outcome = run_agent_in(
        &w,
        "run-2",
        &second,
        "printf 'merged\\n' > a.txt && git add a.txt && GIT_EDITOR=true git rebase --continue",
    )
    .unwrap();

    assert_eq!(outcome, ConflictFixOutcome::Resolved);
    assert_eq!(
        git(&second, &["symbolic-ref", "HEAD"]),
        "refs/heads/fix--COM-03"
    );
}

#[test]
fn a_worktree_that_is_not_mid_rebase_is_an_error_and_no_session_runs() {
    let w = world("not-rebasing", false);
    let marker = w.root.join("ran");

    let result = run_agent(&w, &format!("touch {}", marker.display()));

    assert!(result.is_err());
    assert!(!marker.exists(), "the session must not start");
    assert!(w.store.events("run-1").unwrap().is_empty());
}
