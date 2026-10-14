use std::path::Path;
use std::process::Command;

use orq_features::ledger_path;
use orq_git::resolve_project;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn fresh(name: &str) -> std::path::PathBuf {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn a_bare_repository_named_dot_git_is_not_taken_for_a_checkout() {
    let base = fresh("bare-dot-git");
    let bare = base.join("holder").join(".git");
    let worktree = base.join("wt");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    let tree = Command::new("git").arg("-C").arg(&bare).args(["hash-object", "-t", "tree", "-w", "--stdin"])
        .stdin(std::process::Stdio::null()).output().unwrap();
    let tree = String::from_utf8(tree.stdout).unwrap();
    let commit = Command::new("git").arg("-C").arg(&bare)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "commit-tree", tree.trim(), "-m", "init"])
        .output().unwrap();
    let commit = String::from_utf8(commit.stdout).unwrap();
    git(&bare, &["worktree", "add", "-q", "-b", "wt", worktree.to_str().unwrap(), commit.trim()]);
    std::fs::write(base.join("holder").join(".tdd-features.json"), "[]\n").unwrap();

    let project = resolve_project(&worktree).unwrap();

    assert_eq!(ledger_path(&project), project.root.join(".tdd-features.json"));
}
