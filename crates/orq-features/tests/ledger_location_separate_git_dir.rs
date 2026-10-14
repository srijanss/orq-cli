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

#[test]
fn a_git_dir_outside_the_checkout_never_redirects_the_ledger_to_the_metadata_directory() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("separate-git-dir");
    let _ = std::fs::remove_dir_all(&base);
    let main = base.join("main");
    let meta = base.join("elsewhere").join("meta");
    let worktree = base.join("wt");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
    git(&main, &["init", "-q", "--separate-git-dir", meta.to_str().unwrap()]);
    git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&main, &["worktree", "add", "-q", worktree.to_str().unwrap()]);
    std::fs::write(main.join(".tdd-features.json"), "[]\n").unwrap();

    let project = resolve_project(&worktree).unwrap();

    // The main checkout cannot be recovered from the metadata directory, so
    // the answer stays the worktree's own path rather than a directory that
    // is not a checkout.
    assert_eq!(ledger_path(&project), project.root.join(".tdd-features.json"));
}
