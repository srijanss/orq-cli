use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, write_ledger_bytes, GitFixture, LedgerEntry};

fn features(repo: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["features", "--repo"])
        .arg(repo)
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("TDD_FEATURES_PATH")
        .output()
        .unwrap()
}

#[test]
fn each_repository_lists_only_its_own_features_as_json() {
    let repo_a = GitFixture::new();
    let repo_b = GitFixture::new();
    write_ledger(
        repo_a.root(),
        &[
            LedgerEntry::new("alpha", "draft").description("first"),
            LedgerEntry::new("beta", "pending").depends_on(&["alpha"]),
        ],
    );
    write_ledger(repo_b.root(), &[LedgerEntry::new("gamma", "completed")]);

    let a = features(repo_a.nested_dir());
    let b = features(repo_b.root());

    assert!(a.status.success(), "stderr: {}", String::from_utf8_lossy(&a.stderr));
    let a: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    let b: serde_json::Value = serde_json::from_slice(&b.stdout).unwrap();
    assert_eq!(a.as_array().unwrap().len(), 2);
    assert_eq!(a[0]["name"], "alpha");
    assert_eq!(a[0]["status"], "draft");
    assert_eq!(a[0]["description"], "first");
    assert_eq!(a[0]["dependsOn"], serde_json::json!([]));
    assert_eq!(a[1]["name"], "beta");
    assert_eq!(a[1]["dependsOn"], serde_json::json!(["alpha"]));
    assert_eq!(b.as_array().unwrap().len(), 1);
    assert_eq!(b[0]["name"], "gamma");
}

#[test]
fn an_unrecognised_status_is_printed_as_its_own_text() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("odd", "weird")]);

    let out = features(repo.root());

    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(list[0]["status"], "weird");
}

#[test]
fn a_missing_ledger_exits_non_zero_with_the_reason_and_no_list() {
    let repo = GitFixture::new();

    let out = features(repo.root());

    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing"), "{stderr}");
    assert!(stderr.contains(".tdd-features.json"), "{stderr}");
}

#[test]
fn a_malformed_ledger_exits_non_zero_with_the_reason_and_no_list() {
    let repo = GitFixture::new();
    write_ledger_bytes(repo.root(), b"{ not json");

    let out = features(repo.root());

    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("malformed"), "{stderr}");
}

#[test]
fn control_characters_from_the_ledger_never_reach_stderr_raw() {
    let repo = GitFixture::new();
    // A duplicate name whose JSON escape decodes to a literal ESC.
    write_ledger_bytes(
        repo.root(),
        br#"[{"featureName":"a\u001b[31mx","status":"draft"},{"featureName":"a\u001b[31mx","status":"draft"}]"#,
    );

    let out = features(repo.root());

    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.contains(&0x1b), "{:?}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("malformed"));
}
