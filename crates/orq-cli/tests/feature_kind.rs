//! `orq feature kind --repo <path> --id <name> frontend|standard` sets a feature's kind, kept in orq.db by project
//! and feature name, never in the ledger; `orq features` says each feature's kind.

use std::path::Path;
use std::process::Output;

use orq_test_support::{write_ledger, GitFixture, LedgerEntry};

/// The orq binary with this test's own orq folders, kept beside its repository, so tests in parallel never
/// create the same orq.db at once.
fn orq(repo: &GitFixture) -> std::process::Command {
    let home = repo.root().with_extension("orq-home");
    let mut command = orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"));
    command
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH");
    command
}

fn set_kind(repo: &GitFixture, from: &Path, id: &str, kind: &str) -> Output {
    orq(repo).args(["feature", "kind", "--repo"]).arg(from).args(["--id", id, kind]).output().unwrap()
}

fn kinds(repo: &GitFixture) -> Vec<(String, String)> {
    kinds_at(repo, repo.root())
}

fn kinds_at(repo: &GitFixture, at: &Path) -> Vec<(String, String)> {
    let out = orq(repo).args(["features", "--repo"]).arg(at).output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    list.as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap().to_string(), f["kind"].as_str().unwrap().to_string())).collect()
}

fn pair(name: &str, kind: &str) -> (String, String) {
    (name.to_string(), kind.to_string())
}

#[test]
fn a_kind_set_reads_back_in_the_features_list_and_the_ledger_is_untouched() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("page", "pending"), LedgerEntry::new("engine", "draft")]);
    let ledger = std::fs::read(repo.root().join(".tdd-features.json")).unwrap();
    assert_eq!(kinds(&repo), [pair("page", "standard"), pair("engine", "standard")], "standard by default");

    let out = set_kind(&repo, repo.nested_dir(), "page", "frontend");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(kinds(&repo), [pair("page", "frontend"), pair("engine", "standard")]);

    let out = set_kind(&repo, repo.root(), "page", "standard");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(kinds(&repo), [pair("page", "standard"), pair("engine", "standard")]);
    assert_eq!(std::fs::read(repo.root().join(".tdd-features.json")).unwrap(), ledger, "the ledger gets no new field");
}

/// A linked worktree is the same project as its main checkout: a kind set from either reads back from both.
#[test]
fn a_linked_worktree_shares_the_kinds_of_its_main_checkout() {
    let repo = GitFixture::new();
    let linked = repo.add_linked_worktree("side");
    for root in [repo.root(), linked.as_path()] {
        write_ledger(root, &[LedgerEntry::new("page", "pending")]);
    }

    let out = set_kind(&repo, &linked, "page", "frontend");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(kinds_at(&repo, repo.root()), [pair("page", "frontend")], "set from the linked worktree, read from the main checkout");
    assert_eq!(kinds_at(&repo, &linked), [pair("page", "frontend")]);

    let out = set_kind(&repo, repo.root(), "page", "standard");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(kinds_at(&repo, &linked), [pair("page", "standard")], "reset from the main checkout, read from the linked worktree");
    assert_eq!(kinds_at(&repo, repo.root()), [pair("page", "standard")]);
}

#[test]
fn an_unknown_kind_or_a_feature_not_in_the_ledger_is_refused_with_the_reason() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("page", "pending")]);

    let out = set_kind(&repo, repo.root(), "nowhere", "frontend");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nowhere is not in this project's ledger"), "{stderr}");

    let out = set_kind(&repo, repo.root(), "page", "backend");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("frontend or standard"), "{stderr}");

    assert_eq!(kinds(&repo), [pair("page", "standard")], "nothing refused was kept");
}
