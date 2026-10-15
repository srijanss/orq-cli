use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn add(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["feature", "add", "--repo"])
        .arg(repo)
        .args(args)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

fn ledger_text(repo: &Path) -> String {
    fs::read_to_string(repo.join(".tdd-features.json")).unwrap()
}

#[test]
fn a_name_alone_creates_a_minimal_draft_and_prints_it() {
    let repo = GitFixture::new();

    let out = add(repo.root(), &["--name", "alpha"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let entry: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(entry["name"], "alpha");
    assert_eq!(entry["status"], "draft");
    let file: serde_json::Value = serde_json::from_str(&ledger_text(repo.root())).unwrap();
    assert_eq!(file, serde_json::json!([{"featureName": "alpha", "status": "draft"}]));
}

#[test]
fn every_flag_is_written_and_unset_flags_stay_omitted() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("base", "completed")]);

    let out = add(
        repo.root(),
        &[
            "--name", "beta", "--description", "does things", "--depends-on", "base",
            "--parallelizable", "yes", "--test-file", "tests/beta.rs", "--target-file", "src/a.rs",
            "--target-file", "src/b.rs",
        ],
    );

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let file: serde_json::Value = serde_json::from_str(&ledger_text(repo.root())).unwrap();
    let beta = &file[1];
    assert_eq!(beta["featureName"], "beta");
    assert_eq!(beta["description"], "does things");
    assert_eq!(beta["dependsOn"], serde_json::json!(["base"]));
    assert_eq!(beta["parallelizable"], true);
    assert_eq!(beta["testFile"], "tests/beta.rs");
    assert_eq!(beta["targetFiles"], serde_json::json!(["src/a.rs", "src/b.rs"]));
    assert_eq!(beta["status"], "draft");

    let out = add(repo.root(), &["--name", "gamma", "--parallelizable", "no"]);
    assert!(out.status.success());
    let file: serde_json::Value = serde_json::from_str(&ledger_text(repo.root())).unwrap();
    assert_eq!(file[2], serde_json::json!({"featureName": "gamma", "status": "draft", "parallelizable": false}));
}

#[test]
fn validation_errors_exit_non_zero_and_leave_the_file_unchanged() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("a", "draft")]);
    let before = ledger_text(repo.root());

    for (args, needle) in [
        (vec!["--name", "a"], "repeated"),
        (vec!["--name", "b", "--depends-on", "ghost"], "ghost"),
        (vec!["--name", "b", "--depends-on", "b"], "itself"),
        (vec!["--name", "  "], "blank"),
        (vec!["--name", "b", "--parallelizable", "maybe"], "parallelizable"),
        (vec!["--description", "no name"], "--name"),
    ] {
        let out = add(repo.root(), &args);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
        assert_eq!(ledger_text(repo.root()), before, "{args:?}");
    }
}

#[test]
fn a_malformed_ledger_is_never_written() {
    let repo = GitFixture::new();
    fs::write(repo.root().join(".tdd-features.json"), "{\"not\":\"an array\"}").unwrap();

    let out = add(repo.root(), &["--name", "x"]);

    assert!(!out.status.success());
    assert_eq!(ledger_text(repo.root()), "{\"not\":\"an array\"}");
}
