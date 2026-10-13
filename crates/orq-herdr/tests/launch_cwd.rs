use std::path::PathBuf;

use orq_herdr::{resolve_launch_cwd, CwdSource, LaunchCwdError};

fn dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("orq-launch-cwd-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn explicit_cwd_wins_over_pane_env_and_process_cwd() {
    let (explicit, pane, process) = (dir("explicit"), dir("pane"), dir("process"));

    let resolved = resolve_launch_cwd(Some(explicit.as_path()), pane.to_str(), &process).unwrap();

    assert_eq!(resolved, explicit);
}

#[test]
fn pane_env_is_used_when_there_is_no_explicit_cwd() {
    let (pane, process) = (dir("pane2"), dir("process2"));

    let resolved = resolve_launch_cwd(None, pane.to_str(), &process).unwrap();

    assert_eq!(resolved, pane);
}

#[test]
fn process_cwd_is_used_when_nothing_else_is_given() {
    let process = dir("process3");

    let resolved = resolve_launch_cwd(None, None, &process).unwrap();

    assert_eq!(resolved, process);
}


#[test]
fn an_empty_pane_env_is_an_error_and_does_not_fall_back_to_the_process_cwd() {
    let process = dir("process4");

    let err = resolve_launch_cwd(None, Some(""), &process).unwrap_err();

    assert_eq!(err, LaunchCwdError::Empty { source: CwdSource::HerdrActivePaneCwd });
}

#[test]
fn a_missing_explicit_cwd_is_an_error_and_does_not_fall_back_to_the_pane_env() {
    let (pane, process) = (dir("pane5"), dir("process5"));
    let missing = process.join("does-not-exist");

    let err = resolve_launch_cwd(Some(missing.as_path()), pane.to_str(), &process).unwrap_err();

    assert_eq!(
        err,
        LaunchCwdError::NotFound { source: CwdSource::Explicit, path: missing }
    );
}

#[test]
fn an_empty_explicit_cwd_is_an_error() {
    let process = dir("process6");

    let err = resolve_launch_cwd(Some(std::path::Path::new("")), None, &process).unwrap_err();

    assert_eq!(err, LaunchCwdError::Empty { source: CwdSource::Explicit });
}
