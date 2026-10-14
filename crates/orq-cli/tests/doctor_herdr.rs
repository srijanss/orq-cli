use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::{write_ledger, write_ledger_bytes, GitFixture, LedgerEntry};

const VARS: [&str; 6] = [
    "HERDR_ACTIVE_PANE_CWD",
    "HERDR_ACTIVE_PANE_ID",
    "HERDR_ACTIVE_TAB_ID",
    "HERDR_ACTIVE_WORKSPACE_ID",
    "HERDR_SOCKET_PATH",
    "HERDR_BIN_PATH",
];

fn doctor(pane_cwd: Option<&Path>, process_cwd: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
    command.args(["doctor", "herdr"]).current_dir(process_cwd);
    for var in VARS {
        command.env_remove(var);
    }
    if let Some(cwd) = pane_cwd {
        command.env("HERDR_ACTIVE_PANE_CWD", cwd);
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_pane_in_a_nested_directory_reports_the_repo_root_and_its_ledger() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);

    let output = doctor(Some(repo.nested_dir()), Path::new("/"));
    let text = stdout(&output);

    assert!(output.status.success(), "{text} {}", String::from_utf8_lossy(&output.stderr));
    assert!(text.contains(&format!("HERDR_ACTIVE_PANE_CWD: {}", repo.nested_dir().display())), "{text}");
    assert!(text.contains("HERDR_SOCKET_PATH: (not set)"), "{text}");
    assert!(text.contains(&format!("repo root: {}", repo.root().display())), "{text}");
    assert!(text.contains(&format!("ledger: {}", repo.root().join(".tdd-features.json").display())), "{text}");
    assert!(text.contains("ledger status: ok (1 features)"), "{text}");
}

#[test]
fn a_linked_worktree_without_its_own_ledger_reports_the_main_checkouts() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[LedgerEntry::new("alpha", "draft")]);
    let linked = repo.add_linked_worktree("wt");

    let text = stdout(&doctor(Some(&linked), Path::new("/")));

    assert!(text.contains("linked worktree: yes"), "{text}");
    assert!(text.contains(&format!("ledger: {}", repo.root().join(".tdd-features.json").display())), "{text}");
}

#[test]
fn a_pane_directory_that_no_longer_exists_is_an_error_and_never_falls_back_to_the_process_directory() {
    let repo = GitFixture::new();
    write_ledger(repo.root(), &[]);

    let output = doctor(Some(Path::new("/nonexistent/orq-gone")), repo.root());

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HERDR_ACTIVE_PANE_CWD") && stderr.contains("/nonexistent/orq-gone"), "{stderr}");
    assert!(!stdout(&output).contains("repo root:"), "{}", stdout(&output));
}

#[test]
fn a_directory_outside_any_repository_is_reported_not_guessed() {
    let outside = std::env::temp_dir().join(format!("orq-doctor-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();

    let output = doctor(Some(&outside), Path::new("/"));
    let text = stdout(&output);

    assert!(!output.status.success());
    assert!(text.contains("repo root: none (not a Git repository)"), "{text}");
    assert!(!text.contains("ledger:"), "{text}");
}

#[test]
fn a_missing_or_malformed_ledger_is_reported_and_never_written() {
    let repo = GitFixture::new();
    let ledger = repo.root().join(".tdd-features.json");

    let text = stdout(&doctor(Some(repo.root()), Path::new("/")));
    assert!(text.contains("ledger status: missing"), "{text}");
    assert!(!ledger.exists());

    write_ledger_bytes(repo.root(), b"{ not json");
    let text = stdout(&doctor(Some(repo.root()), Path::new("/")));
    assert!(text.contains("ledger status: malformed"), "{text}");
    assert_eq!(std::fs::read(&ledger).unwrap(), b"{ not json");
}
