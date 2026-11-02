use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orq_orchestrator::{run_gate_with_fixes, FixSessionSpec, Gate, GateFixOutcome, RunRecord, RunStore};

struct World {
    store: RunStore,
    root: PathBuf,
    worktree: PathBuf,
    gate_log: PathBuf,
}

fn world(tag: &str) -> World {
    let root = std::env::temp_dir().join(format!("orq-gate-fix-{}-{tag}", std::process::id()));
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
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let gate_log = root.join("gate.log");
    World { store, root, worktree, gate_log }
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

/// A gate that passes once `fixed` exists in the worktree.
fn gate_needing_fixed() -> Gate {
    Gate::Steps { steps: vec![sh("test -f fixed")], notes: Vec::new() }
}

fn spec(w: &World, script: &str) -> FixSessionSpec {
    FixSessionSpec {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        env: Vec::new(),
        cwd: w.worktree.clone(),
        session_id: Some("session-1".to_string()),
        log: w.root.join("fix.log"),
        terminal: false,
    }
}

fn kinds(w: &World) -> Vec<(String, String)> {
    w.store
        .events("run-1")
        .unwrap()
        .into_iter()
        .map(|e| (e.stage.unwrap_or_default(), e.kind))
        .collect()
}

fn pair(stage: &str, kind: &str) -> (String, String) {
    (stage.to_string(), kind.to_string())
}

#[test]
fn a_passing_gate_starts_no_fix_session() {
    let w = world("pass");
    fs::write(w.worktree.join("fixed"), "").unwrap();
    let calls = RefCell::new(0);

    let outcome =
        run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 2, |_, _| {
            *calls.borrow_mut() += 1;
            spec(&w, "exit 0")
        })
        .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 0 });
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(kinds(&w), [pair("verify", "stage.started"), pair("verify", "stage.completed")]);
}

#[test]
fn a_fix_session_that_repairs_the_failure_lets_the_gate_pass_on_the_rerun() {
    let w = world("repair");

    let outcome =
        run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 2, |_, _| {
            spec(&w, "touch fixed")
        })
        .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
    assert_eq!(
        kinds(&w),
        [
            pair("verify", "stage.started"),
            pair("verify", "stage.failed"),
            pair("gate_fix", "stage.started"),
            pair("gate_fix", "stage.completed"),
            pair("verify", "stage.started"),
            pair("verify", "stage.completed"),
        ]
    );
}

#[test]
fn reruns_and_fix_sessions_carry_their_attempt_number() {
    let w = world("numbered");

    run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 2, |attempt, _| {
        spec(&w, if attempt == 2 { "touch fixed" } else { "exit 0" })
    })
    .unwrap();

    let seen: Vec<(String, String, u32)> =
        w.store.events("run-1").unwrap().into_iter().map(|e| (e.stage.unwrap(), e.kind, e.attempt)).collect();
    let started: Vec<_> = seen.iter().filter(|(_, kind, _)| kind == "stage.started").collect();
    assert_eq!(
        started,
        [
            &("verify".to_string(), "stage.started".to_string(), 1),
            &("gate_fix".to_string(), "stage.started".to_string(), 1),
            &("verify".to_string(), "stage.started".to_string(), 2),
            &("gate_fix".to_string(), "stage.started".to_string(), 2),
            &("verify".to_string(), "stage.started".to_string(), 3),
        ]
    );
    assert!(seen.iter().all(|(_, _, attempt)| *attempt >= 1));
}

#[test]
fn a_fixer_that_repairs_the_worktree_but_exits_nonzero_still_passes_on_the_rerun() {
    let w = world("nonzero");

    let outcome =
        run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 2, |_, _| {
            spec(&w, "touch fixed; exit 1")
        })
        .unwrap();

    assert_eq!(outcome, GateFixOutcome::Passed { fix_attempts: 1 });
    let fix_failed = w.store.events("run-1").unwrap().into_iter().find(|e| e.stage.as_deref() == Some("gate_fix") && e.kind == "stage.failed").unwrap();
    assert_eq!(fix_failed.payload["exit_code"], 1, "the harness status is recorded but does not decide");
    assert_eq!(
        kinds(&w),
        [
            pair("verify", "stage.started"),
            pair("verify", "stage.failed"),
            pair("gate_fix", "stage.started"),
            pair("gate_fix", "stage.failed"),
            pair("verify", "stage.started"),
            pair("verify", "stage.completed"),
        ]
    );
}

#[test]
fn a_gate_still_red_after_the_allowed_attempts_fails_the_run_and_keeps_the_worktree() {
    let w = world("red");
    let attempts = RefCell::new(Vec::new());

    let outcome =
        run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 2, |attempt, _| {
            attempts.borrow_mut().push(attempt);
            spec(&w, "exit 0")
        })
        .unwrap();

    assert_eq!(outcome, GateFixOutcome::Failed { fix_attempts: 2, step: 0, exit_code: Some(1) });
    assert_eq!(*attempts.borrow(), [1, 2]);
    assert!(w.worktree.exists(), "the worktree is kept for inspection");
    let last = w.store.events("run-1").unwrap().pop().unwrap();
    assert_eq!(last.kind, "run.failed");
    assert!(last.payload["reason"].as_str().unwrap().contains("2 fix attempt"), "{}", last.payload);
}

#[test]
fn zero_allowed_attempts_fails_straight_away_without_a_session() {
    let w = world("zero");
    let calls = RefCell::new(0);

    let outcome =
        run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 0, |_, _| {
            *calls.borrow_mut() += 1;
            spec(&w, "touch fixed")
        })
        .unwrap();

    assert_eq!(outcome, GateFixOutcome::Failed { fix_attempts: 0, step: 0, exit_code: Some(1) });
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(w.store.events("run-1").unwrap().pop().unwrap().kind, "run.failed");
}

#[test]
fn a_skipped_or_disabled_gate_is_never_fixed() {
    let w = world("skipped");
    let calls = RefCell::new(0);
    let mut fixer = |_: u32, _: &Path| {
        *calls.borrow_mut() += 1;
        spec(&w, "exit 0")
    };

    let skipped = Gate::Skipped { reason: "no local gate detected".to_string() };
    let outcome = run_gate_with_fixes(&w.store, "run-1", &skipped, true, &w.gate_log, 2, &mut fixer).unwrap();
    assert_eq!(outcome, GateFixOutcome::Skipped { reason: "no local gate detected".to_string() });

    let outcome = run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), false, &w.gate_log, 2, &mut fixer).unwrap();
    assert!(matches!(outcome, GateFixOutcome::Skipped { .. }));
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn a_fix_session_that_cannot_start_still_uses_up_an_attempt() {
    let w = world("nostart");
    let mut broken = spec(&w, "");
    broken.program = "/nonexistent/orq-fixer".to_string();

    let outcome =
        run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 1, |_, _| broken.clone()).unwrap();

    assert_eq!(outcome, GateFixOutcome::Failed { fix_attempts: 1, step: 0, exit_code: Some(1) });
    assert!(kinds(&w).contains(&pair("gate_fix", "stage.failed")));
}

#[test]
fn the_fixer_is_told_where_the_gate_output_is() {
    let w = world("log");
    let seen = RefCell::new(String::new());

    run_gate_with_fixes(&w.store, "run-1", &gate_needing_fixed(), true, &w.gate_log, 1, |_, log| {
        *seen.borrow_mut() = fs::read_to_string(log).unwrap();
        spec(&w, "touch fixed")
    })
    .unwrap();

    assert!(seen.borrow().contains("test -f fixed"), "the gate log is already written: {}", seen.borrow());
}

#[test]
fn an_unknown_run_is_an_error_and_no_fix_session_is_asked_for() {
    let w = world("unknown");
    let calls = RefCell::new(0);

    let result = run_gate_with_fixes(&w.store, "ghost", &gate_needing_fixed(), true, &w.gate_log, 2, |_, _| {
        *calls.borrow_mut() += 1;
        spec(&w, "exit 0")
    });

    assert!(result.is_err());
    assert_eq!(*calls.borrow(), 0);
}
