use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{plan_resume, record_launch, RunRecord, RunStore};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with a run that was launched by `pid` and never recorded an ending.
fn launched_run(tag: &str, pid: u32) -> (RunStore, PathBuf) {
    let root = orq_test_support::process_temp_dir().join(format!("orq-resume-interrupted-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let worktree = repo.join(".claude/worktrees/fix--COM-02");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "fix--COM-02", worktree.to_str().unwrap()]);
    let store = RunStore::new(&root.join("state"));
    let record = RunRecord {
        run_id: "run-1".into(),
        project_id: "repo-1".into(),
        feature_id: "COM-02".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: git(&repo, &["rev-parse", "main"]),
        branch: "fix--COM-02".into(),
        worktree,
        repository: "https://github.example/o/r".into(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    record_launch(&store, "run-1", pid, Path::new("/tmp/x.log")).unwrap();
    (store, repo)
}

/// The pid of a process that has already exited.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn a_run_killed_while_running_is_resumable() {
    let (store, repo) = launched_run("killed", dead_pid());
    let plan = plan_resume(&store, &repo, "repo-1", "COM-02").expect("a run whose process is gone can be resumed");
    assert_eq!(plan.from.run_id, "run-1");
}

#[test]
fn a_run_whose_process_is_still_alive_is_not_resumable() {
    let (store, repo) = launched_run("alive", std::process::id());
    let reason = plan_resume(&store, &repo, "repo-1", "COM-02").expect_err("its process is still running");
    assert!(reason.contains("may still be running"), "{reason}");
}
