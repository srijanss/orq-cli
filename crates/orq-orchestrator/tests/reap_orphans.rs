use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{
    plan_orphans, private_ref_name, reap_orphan_ref, record_run_completed, RunRecord, RunStore,
};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn record(run_id: &str, branch: &str, worktree: PathBuf) -> RunRecord {
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: branch.to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0".repeat(40),
        branch: branch.to_string(),
        worktree,
        repository: "https://github.example/o/r".to_string(),
        profiles: BTreeMap::new(),
    }
}

struct World {
    repo: PathBuf,
    store: RunStore,
}

/// Runs: `kept` (worktree present), `gone` (finished, worktree removed), `busy` (still running, worktree removed).
fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-orphans-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let store = RunStore::new(&root.join("state"));
    let present = repo.join(".claude/worktrees/kept");
    fs::create_dir_all(&present).unwrap();
    let kept = record("run-kept", "fix--kept", present);
    let gone = record("run-gone", "fix--gone", repo.join(".claude/worktrees/gone"));
    let busy = record("run-busy", "fix--busy", repo.join(".claude/worktrees/busy"));
    for r in [&kept, &gone, &busy] {
        store.create(r).unwrap();
    }
    record_run_completed(&store, &kept, "review_ready", None).unwrap();
    record_run_completed(&store, &gone, "review_ready", None).unwrap();
    World { repo, store }
}

fn refs(repo: &Path) -> Vec<String> {
    git(repo, &["for-each-ref", "--format=%(refname)", "refs/orq"])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_private_ref_no_live_run_owns_is_an_orphan_and_only_it_is_removed() {
    let w = world("refs");
    let owned = private_ref_name("run-kept");
    let of_gone = private_ref_name("run-gone");
    let of_busy = private_ref_name("run-busy");
    let unknown = private_ref_name("no-such-run");
    for name in [&owned, &of_gone, &of_busy, &unknown] {
        git(&w.repo, &["update-ref", name, "HEAD"]);
    }
    git(&w.repo, &["update-ref", "refs/orq/other/thing", "HEAD"]);

    let plan = plan_orphans(&w.store, &w.repo, "repo-1").unwrap();

    let mut found = plan.refs.clone();
    found.sort();
    let mut expected = vec![of_gone.clone(), unknown.clone()];
    expected.sort();
    assert_eq!(found, expected);

    for name in &plan.refs {
        reap_orphan_ref(&w.repo, name).unwrap();
    }
    let mut left = refs(&w.repo);
    left.sort();
    let mut keep = vec![owned, of_busy, "refs/orq/other/thing".to_string()];
    keep.sort();
    assert_eq!(left, keep);
}

#[test]
fn only_a_private_rebase_ref_can_be_removed_as_an_orphan() {
    let w = world("guard");
    git(&w.repo, &["update-ref", "refs/orq/other/thing", "HEAD"]);
    assert!(reap_orphan_ref(&w.repo, "refs/orq/other/thing").is_err());
    assert!(reap_orphan_ref(&w.repo, "refs/heads/main").is_err());
    assert_eq!(
        git(&w.repo, &["rev-parse", "--verify", "refs/heads/main"]).len(),
        40
    );
    assert_eq!(refs(&w.repo), ["refs/orq/other/thing"]);
}

#[test]
fn the_databases_of_a_finished_run_whose_worktree_is_gone_are_orphans() {
    let w = world("dbs");

    let plan = plan_orphans(&w.store, &w.repo, "repo-1").unwrap();

    // `kept` still has its worktree; `busy` may still be using its databases.
    assert_eq!(
        plan.database_branches,
        [("run-gone".to_string(), "fix--gone".to_string())]
    );
}

#[test]
fn a_branch_a_present_worktree_still_uses_is_not_an_orphan_even_if_an_older_run_had_it() {
    let w = world("shared");
    let present = w.repo.join(".claude/worktrees/again");
    fs::create_dir_all(&present).unwrap();
    let again = record("run-again", "fix--gone", present);
    w.store.create(&again).unwrap();

    let plan = plan_orphans(&w.store, &w.repo, "repo-1").unwrap();

    assert!(
        plan.database_branches.is_empty(),
        "{:?}",
        plan.database_branches
    );
}

#[test]
fn a_symbolic_ref_in_the_private_namespace_is_removed_without_touching_what_it_points_at() {
    let w = world("symbolic");
    git(
        &w.repo,
        &["symbolic-ref", "refs/orq/rebase/aa/end", "refs/heads/main"],
    );

    let plan = plan_orphans(&w.store, &w.repo, "repo-1").unwrap();
    assert_eq!(plan.refs, ["refs/orq/rebase/aa/end"]);
    reap_orphan_ref(&w.repo, "refs/orq/rebase/aa/end").unwrap();

    assert_eq!(
        git(&w.repo, &["rev-parse", "--verify", "refs/heads/main"]).len(),
        40,
        "the branch it pointed at survives"
    );
    assert!(refs(&w.repo).is_empty());
}

fn run_dir(w: &World, run_id: &str) -> PathBuf {
    w.repo.parent().unwrap().join("state/runs").join(run_id)
}

#[test]
fn uncertain_ownership_from_an_unreadable_record_keeps_its_ref_and_withholds_database_cleanup() {
    let w = world("uncertain-record");
    fs::write(run_dir(&w, "run-kept").join("run.json"), "{ not json").unwrap();
    let owned = private_ref_name("run-kept");
    git(&w.repo, &["update-ref", &owned, "HEAD"]);

    let plan = plan_orphans(&w.store, &w.repo, "repo-1").unwrap();

    assert!(plan.refs.is_empty(), "{:?}", plan.refs);
    assert!(
        plan.database_branches.is_empty(),
        "run-gone's branch may be the unreadable run's: {:?}",
        plan.database_branches
    );
}

#[test]
fn uncertain_ownership_from_unreadable_events_counts_as_in_use() {
    let w = world("uncertain-events");
    let events = run_dir(&w, "run-gone").join("events.jsonl");
    fs::remove_file(&events).unwrap();
    fs::create_dir(&events).unwrap();
    git(
        &w.repo,
        &["update-ref", &private_ref_name("run-gone"), "HEAD"],
    );

    let plan = plan_orphans(&w.store, &w.repo, "repo-1").unwrap();

    assert!(plan.refs.is_empty(), "{:?}", plan.refs);
    assert!(
        plan.database_branches.is_empty(),
        "{:?}",
        plan.database_branches
    );
}

#[test]
fn uncertain_existence_of_a_worktree_counts_as_in_use() {
    let w = world("uncertain-worktree");
    // A path below a plain file cannot be inspected: that is not the same as being gone.
    let file = w.repo.join("plain-file");
    fs::write(&file, "x").unwrap();
    let blocked = record("run-blocked", "fix--blocked", file.join("worktree"));
    w.store.create(&blocked).unwrap();
    record_run_completed(&w.store, &blocked, "review_ready", None).unwrap();
    git(
        &w.repo,
        &["update-ref", &private_ref_name("run-blocked"), "HEAD"],
    );

    let plan = plan_orphans(&w.store, &w.repo, "repo-1").unwrap();

    assert!(plan.refs.is_empty(), "{:?}", plan.refs);
    assert_eq!(
        plan.database_branches,
        [("run-gone".to_string(), "fix--gone".to_string())]
    );
}
