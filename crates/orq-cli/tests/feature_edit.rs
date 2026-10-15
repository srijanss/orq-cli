use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

fn edit(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["feature", "edit", "--repo"])
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
fn given_fields_change_and_everything_else_is_kept_byte_for_byte() {
    let repo = GitFixture::new();
    write_ledger(
        repo.root(),
        &[LedgerEntry::new("base", "completed"), LedgerEntry::new("alpha", "draft").description("old")],
    );
    let before = ledger_text(repo.root());

    let out = edit(
        repo.root(),
        &["--name", "alpha", "--description", "new text", "--depends-on", "base", "--parallelizable", "yes"],
    );

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed["name"], "alpha");
    assert_eq!(printed["description"], "new text");
    let file: serde_json::Value = serde_json::from_str(&ledger_text(repo.root())).unwrap();
    assert_eq!(file[1]["description"], "new text");
    assert_eq!(file[1]["dependsOn"], serde_json::json!(["base"]));
    assert_eq!(file[1]["parallelizable"], true);
    assert_eq!(file[1]["status"], "draft");
    assert_eq!(file[0], serde_json::from_str::<serde_json::Value>(&before).unwrap()[0]);
}

#[test]
fn an_edit_without_field_flags_changes_nothing() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let before = ledger_text(repo.root());

    let out = edit(repo.root(), &["--name", "alpha"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(ledger_text(repo.root()), before);
}

#[test]
fn there_is_no_rename_flag_and_unknown_features_are_rejected() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let before = ledger_text(repo.root());

    let renamed = edit(repo.root(), &["--name", "alpha", "--new-name", "beta"]);
    let unknown = edit(repo.root(), &["--name", "ghost", "--description", "x"]);
    let missing = edit(repo.root(), &["--description", "x"]);

    for (out, needle) in [(&renamed, "--new-name"), (&unknown, "ghost"), (&missing, "--name")] {
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{stderr}");
    }
    assert_eq!(ledger_text(repo.root()), before);
}

#[test]
fn validation_errors_leave_the_file_unchanged() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let before = ledger_text(repo.root());

    let selfdep = edit(repo.root(), &["--name", "alpha", "--depends-on", "alpha"]);
    let ghost = edit(repo.root(), &["--name", "alpha", "--depends-on", "ghost"]);

    assert!(!selfdep.status.success());
    assert!(String::from_utf8_lossy(&selfdep.stderr).contains("itself"));
    assert!(!ghost.status.success());
    assert!(String::from_utf8_lossy(&ghost.stderr).contains("ghost"));
    assert_eq!(ledger_text(repo.root()), before);
}
