//! An Accept runs the project's tests in the environment the verify gate gives its steps, not in the
//! whole environment of the server that started it: a server started in a Herdr pane must not let the
//! tests find that Herdr, nor any other variable of its own.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-accept-env-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("worktree")).unwrap();
    root
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_string(), "-c".to_string(), script.to_string()]
}

/// Runs the test `name` again in its own process carrying what a server started in a Herdr pane
/// carries, and a variable of its own; `true` in the caller, once that passed.
fn in_the_servers_environment(name: &str) -> bool {
    let env = [
        ("HERDR_SOCKET_PATH", "/run/user/1000/herdr.sock"),
        ("HERDR_BIN_PATH", "/usr/local/bin/herdr"),
        ("HERDR_ENV", "1"),
        ("ORQ_SECRET_TEST", "must-not-leak"),
    ];
    let env: Vec<(&str, &std::ffi::OsStr)> = env
        .iter()
        .map(|(name, value)| (*name, std::ffi::OsStr::new(*value)))
        .collect();
    orq_test_support::in_own_process_with_env(name, &env)
}

/// The variables a step saw, from the `env` it wrote to `seen.txt` in `worktree`.
fn seen(worktree: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(worktree.join("seen.txt"))
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[test]
fn an_accepts_test_step_sees_none_of_the_servers_herdr_or_own_variables_but_still_path_and_home() {
    if in_the_servers_environment("an_accepts_test_step_sees_none_of_the_servers_herdr_or_own_variables_but_still_path_and_home") {
        return;
    }
    let root = dir("clean");
    let worktree = root.join("worktree");

    orq_orchestrator::run_steps(&worktree, &[sh("env > seen.txt")]).unwrap();

    let seen = seen(&worktree);
    for name in [
        "HERDR_SOCKET_PATH",
        "HERDR_BIN_PATH",
        "HERDR_ENV",
        "ORQ_SECRET_TEST",
    ] {
        assert!(
            !seen.contains_key(name),
            "{name} reached the accept's tests: {seen:?}"
        );
    }
    assert_eq!(
        seen.get("PATH").map(String::as_str),
        std::env::var("PATH").ok().as_deref()
    );
    assert_eq!(
        seen.get("HOME").map(String::as_str),
        std::env::var("HOME").ok().as_deref()
    );
}

#[test]
fn an_accepts_test_step_sees_exactly_what_the_verify_gates_step_sees_for_the_same_gate_env() {
    if in_the_servers_environment(
        "an_accepts_test_step_sees_exactly_what_the_verify_gates_step_sees_for_the_same_gate_env",
    ) {
        return;
    }
    let root = dir("same-as-gate");
    let worktree = root.join("worktree");
    let env = vec![
        ("DB_NAME".to_string(), "shop_fix_com_01".to_string()),
        ("GATE_BASE".to_string(), "0".repeat(40)),
    ];
    let store = orq_orchestrator::RunStore::new(&root.join("state"));
    store
        .create(&orq_orchestrator::RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-01".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--COM-01".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let gate = orq_orchestrator::Gate::Steps {
        steps: vec![sh("env > seen.txt")],
        notes: Vec::new(),
    };
    let outcome = orq_orchestrator::run_gate_stage_with_env(
        &store,
        "run-1",
        &gate,
        true,
        &root.join("gate.log"),
        &env,
    )
    .unwrap();
    assert_eq!(outcome, orq_orchestrator::GateOutcome::Passed);
    let gate_saw = seen(&worktree);
    fs::remove_file(worktree.join("seen.txt")).unwrap();

    orq_orchestrator::run_steps_with_env(&worktree, &[sh("env > seen.txt")], &env).unwrap();

    let accept_saw = seen(&worktree);
    assert_eq!(
        accept_saw.get("DB_NAME").map(String::as_str),
        Some("shop_fix_com_01"),
        "{accept_saw:?}"
    );
    assert_eq!(accept_saw, gate_saw);
}
