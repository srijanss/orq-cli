use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{load_run_settings, SettingsError};

fn repo(tag: &str) -> PathBuf {
    let dir = orq_test_support::process_temp_dir().join(format!("orq-run-settings-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let out = std::process::Command::new("git").arg("-C").arg(&dir).args(["init", "-q", "-b", "main"]).output().unwrap();
    assert!(out.status.success());
    dir
}

fn shared(repo: &Path, text: &str) {
    fs::create_dir_all(repo.join(".orq")).unwrap();
    fs::write(repo.join(".orq/run.toml"), text).unwrap();
}

fn private(repo: &Path, text: &str) {
    fs::create_dir_all(repo.join(".git/orq")).unwrap();
    fs::write(repo.join(".git/orq/run.toml"), text).unwrap();
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn a_repo_without_settings_gets_none() {
    let r = repo("none");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!((s.forge, s.base_branch, s.gate, s.ci_timeout), (None, None, None, None));
    assert!(s.setup.is_empty());
}

#[test]
fn the_checked_in_file_is_read() {
    let r = repo("shared");
    shared(
        &r,
        "forge = \"glab\"\nbase_branch = \"develop\"\ngate = [\"make\", \"check\"]\nsetup = [[\"npm\", \"ci\"], [\"make\", \"deps\"]]\nci_timeout_secs = 600\n",
    );
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!(s.forge.as_deref(), Some("glab"));
    assert_eq!(s.base_branch.as_deref(), Some("develop"));
    assert_eq!(s.gate, Some(vec!["make".to_string(), "check".to_string()]));
    assert_eq!(s.setup, vec![vec!["npm".to_string(), "ci".to_string()], vec!["make".to_string(), "deps".to_string()]]);
    assert_eq!(s.ci_timeout, Some(Duration::from_secs(600)));
}

#[test]
fn the_private_file_wins_key_by_key_and_the_environment_wins_over_both() {
    let r = repo("precedence");
    shared(&r, "forge = \"gh\"\nbase_branch = \"develop\"\n");
    private(&r, "base_branch = \"release\"\n");
    let s = load_run_settings(&r, &no_env).unwrap();
    assert_eq!((s.forge.as_deref(), s.base_branch.as_deref()), (Some("gh"), Some("release")));

    let env = |key: &str| match key {
        "ORQ_BASE_BRANCH" => Some("hotfix".to_string()),
        "ORQ_GATE" => Some("[\"just\",\"ci\"]".to_string()),
        "ORQ_CI_TIMEOUT_SECS" => Some("90".to_string()),
        "ORQ_FORGE" => Some("glab".to_string()),
        _ => None,
    };
    let s = load_run_settings(&r, &env).unwrap();
    assert_eq!((s.forge.as_deref(), s.base_branch.as_deref()), (Some("glab"), Some("hotfix")));
    assert_eq!(s.gate, Some(vec!["just".to_string(), "ci".to_string()]));
    assert_eq!(s.ci_timeout, Some(Duration::from_secs(90)));
}

fn invalid(text: &str, tag: &str) -> String {
    let r = repo(tag);
    shared(&r, text);
    match load_run_settings(&r, &no_env) {
        Err(SettingsError::Invalid { source, reason }) => format!("{source}: {reason}"),
        other => panic!("{text:?} should be invalid, got {other:?}"),
    }
}

#[test]
fn bad_values_are_refused_and_say_where_they_came_from() {
    assert!(invalid("forge = \"bitbucket\"\n", "forge").contains(".orq/run.toml"));
    assert!(invalid("base_branch = \"-x\"\n", "dash").contains("base_branch"));
    assert!(invalid("base_branch = \"a b\"\n", "space").contains("base_branch"));
    assert!(invalid("base_branch = \"release//next\"\n", "doubleslash").contains("base_branch"));
    assert!(invalid("base_branch = \"release/.hidden\"\n", "dotcomponent").contains("base_branch"));
    assert!(invalid("base_branch = \"release.lock\"\n", "lock").contains("base_branch"));
    assert!(invalid("gate = []\n", "emptygate").contains("gate"));
    assert!(invalid("gate = [\"\"]\n", "blankgate").contains("gate"));
    assert!(invalid("gate = \"make check\"\n", "stringgate").contains("gate"));
    assert!(invalid("setup = [[]]\n", "emptysetup").contains("setup"));
    assert!(invalid("ci_timeout_secs = 0\n", "zero").contains("ci_timeout_secs"));
    assert!(invalid("ci_timeout_secs = 999999999\n", "huge").contains("ci_timeout_secs"));
    assert!(invalid("colour = \"red\"\n", "unknown").contains("colour"));
    assert!(invalid("forge = ", "syntax").contains(".orq/run.toml"));
}

#[test]
fn a_bad_environment_value_is_refused_by_name() {
    let r = repo("badenv");
    let env = |key: &str| (key == "ORQ_GATE").then(|| "make check".to_string());
    match load_run_settings(&r, &env) {
        Err(SettingsError::Invalid { source, .. }) => assert_eq!(source, "ORQ_GATE"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn branch_names_git_accepts_are_accepted() {
    for name in ["release+fix", "feature/ünï", "v1.2", "team/x_y"] {
        let r = repo(&format!("ok-{}", name.len()));
        shared(&r, &format!("base_branch = \"{name}\"\n"));
        let s = load_run_settings(&r, &no_env).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(s.base_branch.as_deref(), Some(name));
    }
}
