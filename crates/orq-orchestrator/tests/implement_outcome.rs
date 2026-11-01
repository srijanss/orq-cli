use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{check_implement_outcome, ImplementCheck, RunRecord, RunStore};

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

struct World {
    store: RunStore,
    worktree: PathBuf,
}

fn world(tag: &str) -> World {
    world_with_base(tag, None)
}

fn world_with_base(tag: &str, base_override: Option<&str>) -> World {
    let root = std::env::temp_dir().join(format!("orq-outcome-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "-q", "-b", "main"]);
    fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "base"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            remote: "git@github.com:acme/app.git".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: base_override.map(str::to_string).unwrap_or(base),
            branch: "fix--COM-02".to_string(),
            worktree: worktree.clone(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store, worktree }
}

fn commit_change(w: &World, name: &str) {
    fs::write(w.worktree.join(name), "x\n").unwrap();
    git(&w.worktree, &["add", "."]);
    git(&w.worktree, &["commit", "-q", "-m", name]);
}

fn blocked_events(w: &World) -> Vec<orq_orchestrator::Event> {
    w.store.events("run-1").unwrap().into_iter().filter(|e| e.kind == "run.blocked").collect()
}

#[test]
fn a_clean_worktree_with_new_commits_passes_and_records_nothing() {
    let w = world("ok");
    commit_change(&w, "b.txt");
    commit_change(&w, "c.txt");

    let check = check_implement_outcome(&w.store, "run-1").unwrap();

    assert_eq!(check, ImplementCheck::Ready { commits: 2 });
    assert!(w.store.events("run-1").unwrap().is_empty());
}

#[test]
fn no_new_commits_blocks_the_run_with_the_reason_and_keeps_the_worktree() {
    let w = world("nocommits");

    let check = check_implement_outcome(&w.store, "run-1").unwrap();

    let ImplementCheck::Blocked { reason } = check else { panic!("expected blocked, got {check:?}") };
    assert!(reason.contains("no new commits"), "{reason}");
    let events = blocked_events(&w);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].stage.as_deref(), Some("implement_and_review"));
    assert_eq!(events[0].payload["reason"], reason.as_str());
    assert!(w.worktree.join("a.txt").is_file(), "the worktree is kept");
}

#[test]
fn uncommitted_changes_block_the_run_even_with_new_commits() {
    let w = world("dirty");
    commit_change(&w, "b.txt");
    fs::write(w.worktree.join("a.txt"), "edited\n").unwrap();
    fs::write(w.worktree.join("untracked.txt"), "new\n").unwrap();

    let check = check_implement_outcome(&w.store, "run-1").unwrap();

    let ImplementCheck::Blocked { reason } = check else { panic!("expected blocked, got {check:?}") };
    assert!(reason.contains("uncommitted"), "{reason}");
    assert_eq!(blocked_events(&w).len(), 1);
    assert_eq!(fs::read_to_string(w.worktree.join("a.txt")).unwrap(), "edited\n", "changes are left alone");
}

#[test]
fn a_dirty_worktree_is_blocked_without_needing_the_base_commit() {
    let w = world_with_base("dirty-badbase", Some("0000000000000000000000000000000000000000"));
    fs::write(w.worktree.join("a.txt"), "edited\n").unwrap();

    let check = check_implement_outcome(&w.store, "run-1").unwrap();

    let ImplementCheck::Blocked { reason } = check else { panic!("expected blocked, got {check:?}") };
    assert!(reason.contains("uncommitted"), "{reason}");
    assert_eq!(blocked_events(&w).len(), 1);
}

#[test]
fn changes_inside_a_submodule_block_even_when_its_ignore_setting_hides_them() {
    let w = world("submodule");
    let sub = w.worktree.parent().unwrap().join("sub-origin");
    fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q", "-b", "main"]);
    fs::write(sub.join("s.txt"), "s\n").unwrap();
    git(&sub, &["add", "."]);
    git(&sub, &["commit", "-q", "-m", "sub"]);
    git(&w.worktree, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", sub.to_str().unwrap(), "vendor"]);
    git(&w.worktree, &["commit", "-q", "-m", "add submodule"]);
    git(&w.worktree, &["config", "submodule.vendor.ignore", "all"]);
    fs::write(w.worktree.join("vendor/s.txt"), "edited\n").unwrap();

    let check = check_implement_outcome(&w.store, "run-1").unwrap();

    let ImplementCheck::Blocked { reason } = check else { panic!("expected blocked, got {check:?}") };
    assert!(reason.contains("uncommitted"), "{reason}");
}

#[test]
fn an_unknown_run_is_an_error() {
    let w = world("unknown");
    assert!(check_implement_outcome(&w.store, "ghost").is_err());
}
