use orq_orchestrator::run_worktree_tests;

fn dir(tag: &str) -> std::path::PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-accept-tests-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_configured_command_that_succeeds_passes_and_one_that_fails_says_what_it_printed() {
    let worktree = dir("configured");
    let pass = vec!["sh".to_string(), "-c".to_string(), "exit 0".to_string()];
    assert_eq!(run_worktree_tests(&worktree, Some(pass.as_slice())), Ok(()));

    let fail = vec!["sh".to_string(), "-c".to_string(), "echo 2 tests failed; exit 1".to_string()];
    let err = run_worktree_tests(&worktree, Some(fail.as_slice())).unwrap_err();
    assert!(err.contains("2 tests failed"), "{err}");
}

#[test]
fn the_command_runs_in_the_worktree() {
    let worktree = dir("cwd");
    std::fs::write(worktree.join("marker.txt"), "x").unwrap();
    let check = vec!["sh".to_string(), "-c".to_string(), "test -f marker.txt".to_string()];
    assert_eq!(run_worktree_tests(&worktree, Some(check.as_slice())), Ok(()));
}

#[test]
fn a_worktree_with_nothing_to_test_is_not_a_pass() {
    let err = run_worktree_tests(&dir("empty"), None).unwrap_err();
    assert!(err.contains("no local gate detected"), "{err}");
}

#[test]
fn a_failing_command_that_prints_a_lot_says_only_the_end_of_it_in_the_order_it_was_printed() {
    let worktree = dir("noisy");
    let script = "i=0; while [ $i -lt 3000 ]; do echo \"out line $i padding padding padding\"; echo \"err line $i\" >&2; i=$((i+1)); done; echo final-\"\"out; echo final-\"\"err >&2; exit 1";
    let noisy = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
    let err = run_worktree_tests(&worktree, Some(noisy.as_slice())).unwrap_err();
    assert!(err.len() < 4000, "{} bytes", err.len());
    assert!(err.contains("final-out") && err.contains("final-err"), "{err}");
    assert!(err.find("final-out") < err.find("final-err"), "the end of the output is in the order it was printed: {err}");
    assert!(!err.contains("out line 0 "), "{err}");
}

#[test]
fn a_command_that_cannot_start_is_a_failure() {
    let missing = vec!["definitely-not-a-program-orq".to_string()];
    assert!(run_worktree_tests(&dir("missing"), Some(missing.as_slice())).is_err());
}
