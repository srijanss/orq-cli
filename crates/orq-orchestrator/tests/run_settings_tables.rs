//! One `.orq/run.toml`, every reader: the run settings, the release and after-merge parsers.

use std::fs;

use orq_core::project_settings::{AfterMergeCommands, ReleaseSettings};
use orq_orchestrator::load_run_settings;
use orq_test_support::GitFixture;

/// This project's real shape.
const REAL: &str = r#"gate = ["cargo", "test"]

[rebase]
generated = ["Cargo.lock"]
rebuild = ["cargo", "build"]

[release]
release_on_merge = "auto"
version_file = "Cargo.toml"
version_key = "workspace.package.version"
commands = [["cargo", "check"]]

[after_merge]
commands = [["cargo", "install", "--path", "."]]
"#;

fn repo_with(run_toml: &str) -> GitFixture {
    let repo = GitFixture::new();
    fs::create_dir_all(repo.root().join(".orq")).unwrap();
    fs::write(repo.root().join(".orq/run.toml"), run_toml).unwrap();
    repo
}

fn settings_error(repo: &GitFixture) -> String {
    match load_run_settings(repo.root(), &|_| None) {
        Ok(_) => String::new(),
        Err(err) => err.to_string(),
    }
}

#[test]
fn a_run_toml_with_release_and_after_merge_tables_loads_through_every_reader() {
    let repo = repo_with(REAL);

    let settings = load_run_settings(repo.root(), &|_| None);
    let release = ReleaseSettings::parse(REAL);
    let after = AfterMergeCommands::parse(REAL);

    assert!(settings.is_ok(), "run settings: {:?}", settings.err().map(|e| e.to_string()));
    assert!(release.unwrap().is_some());
    assert_eq!(after.unwrap().unwrap().commands.len(), 1);
}

#[test]
fn a_bad_value_inside_release_names_the_key_from_every_reader() {
    let bad = "[release]\nversion_file = 3\n";
    let repo = repo_with(bad);

    let run = settings_error(&repo);
    let release = ReleaseSettings::parse(bad).unwrap_err();

    assert!(run.contains("release.version_file"), "run settings: {run}");
    assert!(release.contains("release.version_file"), "release: {release}");
}

#[test]
fn a_bad_command_inside_after_merge_names_the_key() {
    let repo = repo_with("[after_merge]\ncommands = \"make\"\n");

    assert!(settings_error(&repo).contains("after_merge.commands"));
}

#[test]
fn a_plain_after_merge_value_is_refused_plainly() {
    let repo = repo_with("after_merge = \"auto\"\n");

    let error = settings_error(&repo);

    assert!(error.contains("after_merge must be a table"), "{error}");
}

#[test]
fn an_unknown_table_is_still_refused() {
    let repo = repo_with("[foo]\nbar = 1\n");

    assert!(settings_error(&repo).contains("foo"));
}
