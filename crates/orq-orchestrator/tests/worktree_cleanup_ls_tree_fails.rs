//! A finished run whose installed packages cannot be checked against what is committed keeps its
//! worktree, and the reason says the check failed rather than that a file is missing. Its own test
//! binary: it puts a git that refuses to list trees on `PATH`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use orq_orchestrator::{cleanup_worktree, CiOutcome, CleanupOutcome, RunRecord, RunStore};

/// Puts `PATH` back as it was when dropped, also when the test panics.
struct RestorePath(OsString);

impl Drop for RestorePath {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.0);
    }
}

/// From now until the guard drops, `git ls-tree` fails; other commands run the real git.
fn git_that_cannot_list_trees(root: &Path) -> RestorePath {
    let path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&path).map(|dir| dir.join("git")).find(|git| git.is_file()).expect("a git on PATH");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in ls-tree) echo 'fatal: probe refused' >&2; exit 128;; esac; done\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
    RestorePath(path)
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, path: &str, text: &str) {
    let file = dir.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

#[test]
fn installed_packages_that_cannot_be_checked_against_the_commit_keep_the_worktree_and_say_so() {
    let root = orq_test_support::process_temp_dir().join(format!("orq-cleanup-ls-tree-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let worktree = root.join("worktree");
    git(&repo, &["worktree", "add", "-q", "-b", "fix--COM-02", worktree.to_str().unwrap()]);
    write(&repo, ".git/info/exclude", "node_modules/\n");
    write(&worktree, "web/package.json", "{}\n");
    write(&worktree, "web/package-lock.json", "{}\n");
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "manifests"]);
    write(&worktree, "web/node_modules/.bin/esbuild", "installed\n");
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: base,
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let _git = git_that_cannot_list_trees(&root);

    let outcome = cleanup_worktree(&store, "run-1", true, &CiOutcome::Passed).unwrap();

    let CleanupOutcome::Kept { reason } = outcome else { panic!("{outcome:?}") };
    assert!(
        reason.ends_with("; cannot tell what web/ has committed: fatal: probe refused)"),
        "{reason}"
    );
    assert!(worktree.join("web/node_modules/.bin/esbuild").exists());
}
