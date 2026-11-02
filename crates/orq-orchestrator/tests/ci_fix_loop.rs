use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{
    run_ci_fix_loop, CiFixConfig, CiFixOutcome, CiFixSteps, CiOutcome, FixSessionSpec, Gate, PushOutcome, RunRecord,
    RunStore, StoreError,
};

const SHA1: &str = "1111111111111111111111111111111111111111";
const SHA2: &str = "2222222222222222222222222222222222222222";
const SHA3: &str = "3333333333333333333333333333333333333333";

struct World {
    store: RunStore,
    root: PathBuf,
    worktree: PathBuf,
}

fn world(tag: &str) -> World {
    let root = std::env::temp_dir().join(format!("orq-ci-fix-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store, root, worktree }
}

/// The scripted world the loop talks to: what CI says each time it is asked, what a push
/// answers, and what the fix session does.
struct Script {
    worktree: PathBuf,
    root: PathBuf,
    waits: VecDeque<CiOutcome>,
    pushes: VecDeque<PushOutcome>,
    /// Shell run by every fix session; `$LOG` is the failure log it was given.
    fix: String,
    waited: Vec<String>,
    sessions: Vec<u32>,
    pushed: u32,
}

impl Script {
    fn new(w: &World, waits: Vec<CiOutcome>, pushes: Vec<PushOutcome>, fix: &str) -> Script {
        Script {
            worktree: w.worktree.clone(),
            root: w.root.clone(),
            waits: waits.into(),
            pushes: pushes.into(),
            fix: fix.to_string(),
            waited: Vec::new(),
            sessions: Vec::new(),
            pushed: 0,
        }
    }
}

impl CiFixSteps for Script {
    fn wait(&mut self, head_sha: &str) -> Result<CiOutcome, StoreError> {
        self.waited.push(head_sha.to_string());
        Ok(self.waits.pop_front().expect("CI was asked more often than scripted"))
    }
    fn push(&mut self) -> Result<PushOutcome, StoreError> {
        self.pushed += 1;
        Ok(self.pushes.pop_front().expect("a push was not scripted"))
    }
    fn failure_log(&mut self, failing: &[String]) -> String {
        format!("boom: {}\n", failing.join(", "))
    }
    fn fix_session(&mut self, attempt: u32, failure_log: &Path) -> FixSessionSpec {
        self.sessions.push(attempt);
        FixSessionSpec {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), self.fix.clone()],
            env: vec![("LOG".to_string(), failure_log.to_string_lossy().into_owned())],
            cwd: self.worktree.clone(),
            session_id: Some("session-1".to_string()),
            log: self.root.join("fix.log"),
            terminal: false,
        }
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

/// A gate that passes once `fixed` exists in the worktree.
fn gate_needing_fixed() -> Gate {
    Gate::Steps { steps: vec![sh("test -f fixed")], notes: Vec::new() }
}

fn config<'a>(w: &'a World, gate: &'a Gate, max_attempts: u32) -> CiFixConfig<'a> {
    CiFixConfig {
        gate,
        gate_enabled: true,
        gate_log: Box::leak(w.root.join("gate.log").into_boxed_path()),
        log_dir: Box::leak(w.worktree.join(".orq").into_boxed_path()),
        max_attempts,
    }
}

fn red(names: &[&str]) -> CiOutcome {
    CiOutcome::Failed { failing: names.iter().map(|n| n.to_string()).collect() }
}

fn pushed(sha: &str) -> PushOutcome {
    PushOutcome::Pushed { head_sha: sha.to_string() }
}

fn kinds(w: &World) -> Vec<(String, String)> {
    w.store.events("run-1").unwrap().into_iter().map(|e| (e.stage.unwrap_or_default(), e.kind)).collect()
}

fn pair(stage: &str, kind: &str) -> (String, String) {
    (stage.to_string(), kind.to_string())
}

#[test]
fn a_ci_that_is_green_the_first_time_starts_no_fix_session() {
    let w = world("green");
    let gate = gate_needing_fixed();
    let mut script = Script::new(&w, vec![CiOutcome::Passed], vec![], "exit 0");

    let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 2), &mut script).unwrap();

    assert_eq!(outcome, CiFixOutcome::Passed { fix_attempts: 0 });
    assert_eq!(script.waited, [SHA1]);
    assert!(script.sessions.is_empty());
    assert_eq!(script.pushed, 0);
    assert!(!w.worktree.join(".orq").exists(), "no failure log without a failure");
}

#[test]
fn failing_checks_are_written_into_the_worktree_fixed_gated_pushed_and_waited_on_again() {
    let w = world("repair");
    let gate = gate_needing_fixed();
    // The session only repairs the worktree if it was given the failure log.
    let mut script =
        Script::new(&w, vec![red(&["build", "lint"]), CiOutcome::Passed], vec![pushed(SHA2)], "grep -q 'boom: build, lint' \"$LOG\" && touch fixed");

    let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 2), &mut script).unwrap();

    assert_eq!(outcome, CiFixOutcome::Passed { fix_attempts: 1 });
    assert_eq!(script.waited, [SHA1, SHA2], "the second wait is for the commit that was pushed");
    assert_eq!(script.sessions, [1]);
    assert_eq!(fs::read_to_string(w.worktree.join(".orq/ci-failure-1.log")).unwrap(), "boom: build, lint\n");
    assert_eq!(
        kinds(&w),
        [
            pair("ci_fix", "stage.started"),
            pair("ci_fix", "stage.completed"),
            pair("verify", "stage.started"),
            pair("verify", "stage.completed"),
        ]
    );
}

#[test]
fn each_fix_attempt_gets_its_own_failure_log() {
    let w = world("two-logs");
    let gate = Gate::Skipped { reason: "none".to_string() };
    let mut script = Script::new(
        &w,
        vec![red(&["first"]), red(&["second"]), CiOutcome::Passed],
        vec![pushed(SHA2), pushed(SHA3)],
        "exit 0",
    );

    let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 2), &mut script).unwrap();

    assert_eq!(outcome, CiFixOutcome::Passed { fix_attempts: 2 });
    assert_eq!(script.waited, [SHA1, SHA2, SHA3]);
    assert_eq!(fs::read_to_string(w.worktree.join(".orq/ci-failure-1.log")).unwrap(), "boom: first\n");
    assert_eq!(fs::read_to_string(w.worktree.join(".orq/ci-failure-2.log")).unwrap(), "boom: second\n");
}

#[test]
fn a_ci_still_red_after_the_allowed_attempts_fails_the_run_and_keeps_the_worktree() {
    let w = world("still-red");
    let gate = Gate::Skipped { reason: "none".to_string() };
    let mut script = Script::new(
        &w,
        vec![red(&["a"]), red(&["b"]), red(&["c"])],
        vec![pushed(SHA2), pushed(SHA3)],
        "exit 0",
    );

    let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 2), &mut script).unwrap();

    assert_eq!(outcome, CiFixOutcome::Failed { fix_attempts: 2, failing: vec!["c".to_string()] });
    assert_eq!(script.sessions, [1, 2]);
    assert!(w.worktree.exists(), "the worktree is kept for inspection");
    let last = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(last.kind, "run.failed");
    assert!(last.payload["reason"].as_str().unwrap().contains("2 fix attempt"), "{}", last.payload);
}

#[test]
fn zero_allowed_attempts_fails_straight_away_without_a_session() {
    let w = world("zero");
    let gate = Gate::Skipped { reason: "none".to_string() };
    let mut script = Script::new(&w, vec![red(&["a"])], vec![], "exit 0");

    let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 0), &mut script).unwrap();

    assert_eq!(outcome, CiFixOutcome::Failed { fix_attempts: 0, failing: vec!["a".to_string()] });
    assert!(script.sessions.is_empty());
    assert_eq!(w.store.events("run-1").unwrap().pop().unwrap().kind, "run.failed");
}

#[test]
fn a_ci_that_timed_out_errored_or_was_skipped_is_never_fixed() {
    for (tag, ci, expected) in [
        ("timeout", CiOutcome::TimedOut, CiFixOutcome::TimedOut { fix_attempts: 0 }),
        (
            "error",
            CiOutcome::Errored { reason: "provider down".to_string() },
            CiFixOutcome::Errored { fix_attempts: 0, reason: "provider down".to_string() },
        ),
        ("skipped", CiOutcome::Skipped, CiFixOutcome::Skipped),
    ] {
        let w = world(tag);
        let gate = gate_needing_fixed();
        let mut script = Script::new(&w, vec![ci], vec![], "touch fixed");

        let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 2), &mut script).unwrap();

        assert_eq!(outcome, expected, "{tag}");
        assert!(script.sessions.is_empty(), "{tag}");
        assert_eq!(script.pushed, 0, "{tag}");
    }
}

#[test]
fn a_fix_that_leaves_the_gate_red_is_not_pushed_and_fails_the_run() {
    let w = world("gate-red");
    let gate = gate_needing_fixed();
    let mut script = Script::new(&w, vec![red(&["build"])], vec![], "exit 0");

    let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 2), &mut script).unwrap();

    assert_eq!(outcome, CiFixOutcome::GateFailed { fix_attempts: 1, step: 0, exit_code: Some(1) });
    assert_eq!(script.pushed, 0, "nothing red is published");
    assert_eq!(script.waited, [SHA1]);
    let last = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(last.kind, "run.failed");
    assert!(last.payload["reason"].as_str().unwrap().contains("gate"), "{}", last.payload);
}

#[test]
fn a_push_that_fails_after_a_fix_fails_the_run_without_waiting_again() {
    let w = world("push-fails");
    let gate = Gate::Skipped { reason: "none".to_string() };
    let mut script =
        Script::new(&w, vec![red(&["build"])], vec![PushOutcome::Failed { reason: "rejected".to_string() }], "exit 0");

    let outcome = run_ci_fix_loop(&w.store, "run-1", SHA1, &config(&w, &gate, 2), &mut script).unwrap();

    assert_eq!(outcome, CiFixOutcome::PushFailed { fix_attempts: 1, reason: "rejected".to_string() });
    assert_eq!(script.waited, [SHA1]);
    let last = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(last.kind, "run.failed");
    assert!(last.payload["reason"].as_str().unwrap().contains("rejected"), "{}", last.payload);
}

#[test]
fn an_unknown_run_is_an_error_and_nothing_is_asked() {
    let w = world("unknown");
    let gate = Gate::Skipped { reason: "none".to_string() };
    let mut script = Script::new(&w, vec![CiOutcome::Passed], vec![], "exit 0");

    let result = run_ci_fix_loop(&w.store, "ghost", SHA1, &config(&w, &gate, 2), &mut script);

    assert!(result.is_err());
    assert!(script.waited.is_empty());
}
