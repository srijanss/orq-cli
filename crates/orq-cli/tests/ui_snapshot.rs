use std::path::Path;
use std::process::{Command, Output};

use orq_test_support::GitFixture;

/// `orq ui --snapshot` as launched by Herdr: only the active pane's directory
/// is communicated, through the environment.
fn ui_snapshot(pane_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orq"))
        .args(["ui", "--snapshot"])
        .env("HERDR_ACTIVE_PANE_CWD", pane_dir)
        // Never walk up from a plain temp dir into some enclosing repository.
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap()
}

fn screen(output: &Output) -> String {
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn status(root: &Path) -> String {
    let out = Command::new("git").arg("-C").arg(root).args(["status", "--porcelain"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn ui_shows_the_project_of_the_active_pane_directory() {
    let repo_a = GitFixture::new();
    let repo_b = GitFixture::new();
    let root_a = repo_a.root().canonicalize().unwrap();
    let root_b = repo_b.root().canonicalize().unwrap();

    let shown_a = screen(&ui_snapshot(repo_a.nested_dir()));
    let shown_b = screen(&ui_snapshot(repo_b.nested_dir()));

    assert!(shown_a.contains("repo"), "{shown_a}");
    assert!(shown_a.contains(root_a.to_str().unwrap()), "{shown_a}");
    assert!(!shown_a.contains(root_b.to_str().unwrap()), "{shown_a}");
    assert!(shown_b.contains(root_b.to_str().unwrap()), "{shown_b}");
    assert!(!shown_b.contains(root_a.to_str().unwrap()), "{shown_b}");
    assert_eq!(status(repo_a.root()), "");
    assert_eq!(status(repo_b.root()), "");
}

#[test]
fn ui_shows_the_owning_project_for_a_linked_worktree() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("feature");

    let shown = screen(&ui_snapshot(&worktree));

    assert!(shown.contains("repo"), "{shown}");
    assert!(shown.contains(worktree.canonicalize().unwrap().to_str().unwrap()), "{shown}");
    assert!(!shown.contains("not a Git repository"), "{shown}");
}

#[test]
fn ui_shows_the_empty_state_outside_a_repository() {
    let plain = std::env::temp_dir().join(format!("orq-ui-plain-{}", std::process::id()));
    std::fs::create_dir_all(&plain).unwrap();

    let shown = screen(&ui_snapshot(&plain));

    assert!(shown.contains("not a Git repository"), "{shown}");
    assert!(shown.contains(plain.to_str().unwrap()), "{shown}");
    let _ = std::fs::remove_dir_all(&plain);
}
