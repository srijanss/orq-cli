use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{plan_resume, record_run_completed, RunRecord, RunStore};

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

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "base"]);
}

/// What happened to the worktree of the stopped run since it stopped.
#[derive(Clone, Copy, PartialEq)]
enum Tamper {
    None,
    /// Detached, as a rebase stopped on conflicts leaves it.
    Detached,
    /// Removed and replaced by a checkout of another repository.
    ReplacedByForeign,
    /// A link to a checkout of this repository that is not the run's.
    Symlinked,
    /// The run's worktree, but not under the repository's worktrees directory.
    Elsewhere,
}

struct World {
    store: RunStore,
    repo: PathBuf,
}

fn world(tag: &str, tamper: Tamper) -> World {
    let root = orq_test_support::process_temp_dir().join(format!("orq-resume-orch-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    init_repo(&repo);
    let worktree = if tamper == Tamper::Elsewhere { root.join("elsewhere") } else { repo.join(".claude/worktrees/fix--COM-02") };
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "fix--COM-02", worktree.to_str().unwrap()]);
    match tamper {
        Tamper::None | Tamper::Elsewhere => {}
        Tamper::Detached => {
            git(&worktree, &["checkout", "-q", "--detach"]);
        }
        Tamper::ReplacedByForeign => {
            git(&repo, &["worktree", "remove", "--force", worktree.to_str().unwrap()]);
            let other = root.join("other");
            init_repo(&other);
            git(&other, &["worktree", "add", "-q", "-b", "fix--COM-02", worktree.to_str().unwrap()]);
        }
        Tamper::Symlinked => {
            git(&repo, &["worktree", "add", "-q", "-b", "someone-else", root.join("someone-else").to_str().unwrap()]);
            git(&repo, &["worktree", "remove", "--force", worktree.to_str().unwrap()]);
            std::os::unix::fs::symlink(root.join("someone-else"), &worktree).unwrap();
        }
    }
    let store = RunStore::new(&root.join("state"));
    let record = RunRecord {
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: git(&repo, &["rev-parse", "main"]),
        branch: "fix--COM-02".to_string(),
        worktree,
        repository: "https://github.example/o/r".to_string(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    record_run_completed(&store, &record, "failed", Some("the gate failed")).unwrap();
    World { store, repo }
}

#[test]
fn only_the_stopped_runs_own_checkout_of_its_branch_can_be_resumed() {
    let ok = world("ok", Tamper::None);
    let plan = plan_resume(&ok.store, &ok.repo, "repo-1", "COM-02").expect("an untouched worktree is resumable");
    assert_eq!(plan.from.run_id, "run-1");

    let refused = [
        ("detached", Tamper::Detached, "not on the branch"),
        ("foreign", Tamper::ReplacedByForeign, "does not belong to this repository"),
        ("symlink", Tamper::Symlinked, "not under"),
        ("elsewhere", Tamper::Elsewhere, "not under"),
    ];
    for (tag, tamper, why) in refused {
        let w = world(tag, tamper);
        let reason = plan_resume(&w.store, &w.repo, "repo-1", "COM-02").expect_err(tag);
        assert!(reason.contains(why), "{tag}: {reason}");
    }
}
