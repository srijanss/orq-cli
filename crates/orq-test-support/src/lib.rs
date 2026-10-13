use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway Git repository in a unique temp directory, removed on drop.
pub struct GitFixture {
    base: PathBuf,
    root: PathBuf,
    nested: PathBuf,
}

impl GitFixture {
    pub fn new() -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("orq-fixture-{}-{unique}", std::process::id()));
        let root = base.join("repo");
        let nested = root.join("src").join("nested");
        fs::create_dir_all(&nested).expect("create fixture dirs");
        fs::write(root.join("README.md"), "fixture\n").expect("write README");

        run_git(&root, &["init", "--quiet"]);
        run_git(&root, &["add", "README.md"]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Orq Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        );

        Self { base, root, nested }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn nested_dir(&self) -> &Path {
        &self.nested
    }

    /// Adds a linked worktree on a new branch `name`, as a sibling of the repo.
    pub fn add_linked_worktree(&self, name: &str) -> PathBuf {
        let path = self.base.join(format!("worktree-{name}"));
        let path_arg = path.to_str().expect("utf-8 fixture path");
        run_git(&self.root, &["worktree", "add", "--quiet", "-b", name, path_arg]);
        path
    }

    /// Creates a bare repository and registers it as the `origin` remote.
    pub fn add_bare_remote(&self) -> PathBuf {
        let path = self.base.join("remote.git");
        let path_arg = path.to_str().expect("utf-8 fixture path");
        run_git(&self.base, &["init", "--quiet", "--bare", path_arg]);
        run_git(&self.root, &["remote", "add", "origin", path_arg]);
        path
    }
}

impl Default for GitFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}
