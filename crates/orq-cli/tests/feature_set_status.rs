use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, write_ledger_bytes, GitFixture, LedgerEntry};

fn set_status(repo: &Path, id: &str, status: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["feature", "set-status", "--repo"])
        .arg(repo)
        .args(["--id", id, "--status", status])
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

fn ledger_bytes(repo: &Path) -> Vec<u8> {
    fs::read(repo.join(".tdd-features.json")).unwrap()
}

fn assert_refused(out: &Output, before: &[u8], repo: &Path, needle: &str) {
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(needle), "{stderr}");
    assert_eq!(ledger_bytes(repo), before);
}

#[test]
fn promoting_a_draft_prints_the_new_entry_and_updates_the_ledger() {
    let repo = GitFixture::new();
    write_ledger(
        repo.root(),
        &[LedgerEntry::new("alpha", "draft").description("first"), LedgerEntry::new("beta", "draft")],
    );

    let out = set_status(repo.nested_dir(), "alpha", "pending");

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let entry: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(entry["name"], "alpha");
    assert_eq!(entry["status"], "pending");
    assert_eq!(entry["description"], "first");
    let listed = Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["features", "--repo"])
        .arg(repo.root())
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(list[0]["status"], "pending");
    assert_eq!(list[1]["status"], "draft");
}

#[test]
fn any_other_target_status_is_refused_and_the_file_is_untouched() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let before = ledger_bytes(repo.root());

    let out = set_status(repo.root(), "alpha", "completed");

    assert_refused(&out, &before, repo.root(), "only --status pending");
}

#[test]
fn a_feature_that_is_not_a_draft_is_refused_and_the_file_is_untouched() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "completed")]);
    let before = ledger_bytes(repo.root());

    let out = set_status(repo.root(), "alpha", "pending");

    assert_refused(&out, &before, repo.root(), "not allowed");
}

#[test]
fn an_unknown_id_is_refused_and_the_file_is_untouched() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let before = ledger_bytes(repo.root());

    let out = set_status(repo.root(), "nope", "pending");

    assert_refused(&out, &before, repo.root(), "unknown feature");
}

#[test]
fn a_malformed_ledger_is_refused_and_the_file_is_untouched() {
    let repo = GitFixture::new();
    write_ledger_bytes(repo.root(), b"{ not json");
    let before = ledger_bytes(repo.root());

    let out = set_status(repo.root(), "alpha", "pending");

    assert_refused(&out, &before, repo.root(), "malformed");
}

#[test]
fn a_held_lock_is_refused_and_the_file_is_untouched() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let before = ledger_bytes(repo.root());
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(repo.root().join(".tdd-features.json.lock"))
        .unwrap();
    lock.lock().unwrap();

    let out = set_status(repo.root(), "alpha", "pending");

    assert_refused(&out, &before, repo.root(), "locked");
}

#[test]
fn an_unrecognised_status_is_refused_and_the_file_is_untouched() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("odd", "weird")]);
    let before = ledger_bytes(repo.root());

    let out = set_status(repo.root(), "odd", "pending");

    assert_refused(&out, &before, repo.root(), "unrecognised status");
    assert!(String::from_utf8_lossy(&out.stderr).contains("weird"));
}

#[test]
fn control_characters_in_the_id_never_reach_stderr_raw() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let before = ledger_bytes(repo.root());

    let out = set_status(repo.root(), "x\u{1b}[31m\nforged: line", "pending");

    assert_refused(&out, &before, repo.root(), "unknown feature");
    assert!(!out.stderr.contains(&0x1b));
    assert_eq!(out.stderr.iter().filter(|b| **b == b'\n').count(), 1);
}
