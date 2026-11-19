//! Pulling a project's target branch after an MR merged: fetch, then fast-forward only, and say what
//! happened when that is not possible. Nothing is ever merged, rebased, reset, stashed or pushed.

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_git::{pull_target, PullOutcome};

/// Runs git in `dir` and gives back what it printed, trimmed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]).args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A project cloned from a bare origin, and another clone standing for the forge.
struct Repos {
    _base: tempdir::Dir,
    project: PathBuf,
    forge: PathBuf,
}

mod tempdir {
    /// A directory removed when dropped.
    pub struct Dir(pub std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn repos(label: &str) -> Repos {
    let base = std::env::temp_dir().join(format!("orq-pull-target-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let base = std::fs::canonicalize(base).unwrap();
    let origin = base.join("origin.git");
    git(&base, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    let project = base.join("project");
    git(&base, &["clone", "-q", origin.to_str().unwrap(), project.to_str().unwrap()]);
    git(&project, &["checkout", "-q", "-b", "main"]);
    std::fs::write(project.join("README"), "start\n").unwrap();
    git(&project, &["add", "README"]);
    git(&project, &["commit", "-q", "-m", "start"]);
    git(&project, &["push", "-q", "origin", "main"]);
    let forge = base.join("forge");
    git(&base, &["clone", "-q", origin.to_str().unwrap(), forge.to_str().unwrap()]);
    Repos { _base: tempdir::Dir(base), project, forge }
}

/// A commit adding file `name` in `dir`; its sha.
fn commit(dir: &Path, name: &str) -> String {
    std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", name]);
    git(dir, &["rev-parse", "HEAD"])
}

/// The forge merges an MR: origin's main gets commit `name`. Its sha.
fn merge_on_origin(r: &Repos, name: &str) -> String {
    let sha = commit(&r.forge, name);
    git(&r.forge, &["push", "-q", "origin", "main"]);
    sha
}

#[test]
fn a_checkout_on_the_target_behind_origin_is_fast_forwarded() {
    let r = repos("ff-on-target");
    let from = git(&r.project, &["rev-parse", "HEAD"]);
    let to = merge_on_origin(&r, "merged.txt");
    assert_eq!(pull_target(&r.project, "origin", "main"), PullOutcome::FastForwarded { from, to: to.clone() });
    assert_eq!(git(&r.project, &["rev-parse", "HEAD"]), to);
    assert_eq!(git(&r.project, &["status", "--porcelain"]), "", "the checkout's files moved with it");
    assert!(r.project.join("merged.txt").exists());
}

#[test]
fn a_checkout_on_another_branch_has_only_the_target_ref_moved() {
    let r = repos("ff-other-branch");
    let from = git(&r.project, &["rev-parse", "refs/heads/main"]);
    git(&r.project, &["checkout", "-q", "-b", "work"]);
    let work = commit(&r.project, "work.txt");
    let to = merge_on_origin(&r, "merged.txt");
    assert_eq!(pull_target(&r.project, "origin", "main"), PullOutcome::FastForwarded { from, to: to.clone() });
    assert_eq!(git(&r.project, &["rev-parse", "refs/heads/main"]), to, "the local target ref moved");
    assert_eq!(git(&r.project, &["rev-parse", "HEAD"]), work, "the checkout's HEAD did not");
    assert_eq!(git(&r.project, &["symbolic-ref", "HEAD"]), "refs/heads/work");
    assert!(!r.project.join("merged.txt").exists(), "nor its files");
}

#[test]
fn a_target_level_with_or_ahead_of_origin_or_diverged_from_it_is_left_alone() {
    let r = repos("no-ff");
    assert_eq!(pull_target(&r.project, "origin", "main"), PullOutcome::UpToDate);

    let local = commit(&r.project, "one.txt");
    commit(&r.project, "two.txt");
    let tip = git(&r.project, &["rev-parse", "HEAD"]);
    assert_eq!(pull_target(&r.project, "origin", "main"), PullOutcome::Ahead { n: 2 });
    assert_eq!(git(&r.project, &["rev-parse", "HEAD"]), tip);

    merge_on_origin(&r, "a.txt");
    merge_on_origin(&r, "b.txt");
    merge_on_origin(&r, "c.txt");
    assert_eq!(pull_target(&r.project, "origin", "main"), PullOutcome::Diverged { local_only: 2, remote_only: 3 });
    assert_eq!(git(&r.project, &["rev-parse", "refs/heads/main"]), tip, "nothing touched");
    assert_eq!(git(&r.project, &["status", "--porcelain"]), "");
    let _ = local;
}

#[test]
fn a_checkout_on_the_target_with_changed_tracked_files_is_not_moved() {
    let r = repos("dirty");
    let from = git(&r.project, &["rev-parse", "HEAD"]);
    std::fs::write(r.project.join("README"), "changed by hand\n").unwrap();
    std::fs::write(r.project.join("notes.txt"), "untracked, left alone\n").unwrap();
    merge_on_origin(&r, "merged.txt");
    assert_eq!(pull_target(&r.project, "origin", "main"), PullOutcome::Dirty { files: vec!["README".to_string()] });
    assert_eq!(git(&r.project, &["rev-parse", "HEAD"]), from, "nothing moved");
    assert_eq!(std::fs::read_to_string(r.project.join("README")).unwrap(), "changed by hand\n");

    // Only untracked files: the pull goes ahead, unless git would overwrite one, which is then the reason.
    git(&r.project, &["checkout", "--", "README"]);
    std::fs::write(r.project.join("merged.txt"), "mine\n").unwrap();
    match pull_target(&r.project, "origin", "main") {
        PullOutcome::Refused { reason } => assert!(reason.contains("merged.txt"), "{reason}"),
        other => panic!("expected a refusal naming the untracked file, got {other:?}"),
    }
    assert_eq!(git(&r.project, &["rev-parse", "HEAD"]), from);
    std::fs::remove_file(r.project.join("merged.txt")).unwrap();
    assert!(matches!(pull_target(&r.project, "origin", "main"), PullOutcome::FastForwarded { .. }));
    assert!(r.project.join("notes.txt").exists(), "an untracked file is left");
}

#[test]
fn a_failed_fetch_is_said_with_credentials_redacted_and_nothing_moves() {
    let r = repos("fetch-failed");
    let from = git(&r.project, &["rev-parse", "HEAD"]);
    git(&r.project, &["remote", "set-url", "origin", "https://orq:s3cr3t-token@127.0.0.1:9/alpha.git"]);
    match pull_target(&r.project, "origin", "main") {
        PullOutcome::FetchFailed { message } => {
            assert!(!message.contains("s3cr3t-token"), "{message}");
            assert!(!message.is_empty());
        }
        other => panic!("expected a failed fetch, got {other:?}"),
    }
    assert_eq!(git(&r.project, &["rev-parse", "HEAD"]), from);
    assert!(matches!(pull_target(&r.project, "--upload-pack=x", "main"), PullOutcome::FetchFailed { .. }));
}

#[test]
fn a_target_checked_out_in_another_worktree_is_not_moved_under_it() {
    let r = repos("other-worktree");
    git(&r.project, &["checkout", "-q", "-b", "work"]);
    let elsewhere = r.project.parent().unwrap().join("elsewhere");
    git(&r.project, &["worktree", "add", "-q", elsewhere.to_str().unwrap(), "main"]);
    let from = git(&r.project, &["rev-parse", "refs/heads/main"]);
    merge_on_origin(&r, "merged.txt");
    assert!(matches!(pull_target(&r.project, "origin", "main"), PullOutcome::Refused { .. }));
    assert_eq!(git(&r.project, &["rev-parse", "refs/heads/main"]), from, "the worktree's branch is not moved under it");
}

/// Puts a `git` that logs its arguments ahead of the real one on PATH, once for the whole binary;
/// the log, whose lines for one repository start `-C <repo>`.
fn logging_git() -> PathBuf {
    static LOG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        let path = std::env::var_os("PATH").unwrap();
        let real = std::env::split_paths(&path).map(|dir| dir.join("git")).find(|git| git.is_file()).expect("git on PATH");
        let dir = std::env::temp_dir().join(format!("orq-pull-target-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("git.log");
        let script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n", log.display(), real.display());
        let wrapper = dir.join("git");
        std::fs::write(&wrapper, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![dir];
        paths.extend(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        log
    })
    .clone()
}

#[test]
fn only_fetching_reading_and_fast_forward_commands_are_run() {
    let log = logging_git();
    let r = repos("commands");
    merge_on_origin(&r, "merged.txt");
    assert!(matches!(pull_target(&r.project, "origin", "main"), PullOutcome::FastForwarded { .. }));
    git(&r.project, &["checkout", "-q", "-b", "work"]);
    merge_on_origin(&r, "again.txt");
    assert!(matches!(pull_target(&r.project, "origin", "main"), PullOutcome::FastForwarded { .. }));
    let prefix = format!("-C {} ", r.project.display());
    let ran: Vec<String> = std::fs::read_to_string(&log).unwrap().lines().filter_map(|line| line.strip_prefix(&prefix)).map(str::to_string).collect();
    assert!(ran.iter().any(|line| line.starts_with("fetch ")), "{ran:#?}");
    assert!(ran.iter().any(|line| line.starts_with("merge --ff-only")), "{ran:#?}");
    assert!(ran.iter().any(|line| line.starts_with("update-ref refs/heads/main ")), "{ran:#?}");
    let reading = ["remote get-url", "ls-remote", "check-ref-format", "rev-parse", "rev-list", "symbolic-ref", "status", "worktree list", "fetch ", "merge --ff-only", "update-ref refs/heads/main "];
    for line in &ran {
        assert!(reading.iter().any(|allowed| line.starts_with(allowed)), "unexpected git command: {line}");
    }
}

#[test]
fn each_outcome_says_whether_a_run_may_start_and_why_not() {
    let ff = PullOutcome::FastForwarded { from: "a".repeat(40), to: "b".repeat(40) };
    assert_eq!((ff.kind(), ff.may_start()), ("fast_forwarded", true));
    assert_eq!((PullOutcome::UpToDate.kind(), PullOutcome::UpToDate.may_start()), ("up_to_date", true));
    assert_eq!((PullOutcome::Ahead { n: 2 }.kind(), PullOutcome::Ahead { n: 2 }.may_start()), ("ahead", true));
    let diverged = PullOutcome::Diverged { local_only: 2, remote_only: 3 };
    assert_eq!((diverged.kind(), diverged.may_start()), ("diverged", false));
    assert_eq!(diverged.describe("origin", "main"), "main has 2 commits not on origin and origin has 3 not here");
    let dirty = PullOutcome::Dirty { files: vec!["README".into(), "src/a.rs".into()] };
    assert_eq!((dirty.kind(), dirty.may_start()), ("dirty", false));
    assert_eq!(dirty.describe("origin", "main"), "main has uncommitted changes to README, src/a.rs");
    let failed = PullOutcome::FetchFailed { message: "could not resolve host".into() };
    assert_eq!((failed.kind(), failed.may_start()), ("fetch_failed", false));
    assert_eq!(failed.describe("origin", "main"), "main could not be fetched from origin: could not resolve host");
    let refused = PullOutcome::Refused { reason: "untracked merged.txt would be overwritten".into() };
    assert_eq!((refused.kind(), refused.may_start()), ("refused", false));
    assert_eq!(refused.describe("origin", "main"), "main could not be fast-forwarded: untracked merged.txt would be overwritten");
    assert_eq!(PullOutcome::Ahead { n: 1 }.describe("origin", "main"), "main is 1 commit ahead of origin");
    assert_eq!(PullOutcome::UpToDate.describe("origin", "main"), "main is up to date with origin");
    assert_eq!(ff.describe("origin", "main"), format!("main fast-forwarded from aaaaaaaa to bbbbbbbb"));
}

#[test]
fn at_most_ten_changed_files_are_named() {
    let r = repos("dirty-many");
    for i in 0..12 {
        std::fs::write(r.project.join(format!("f{i:02}")), "x\n").unwrap();
    }
    git(&r.project, &["add", "."]);
    merge_on_origin(&r, "merged.txt");
    assert!(matches!(pull_target(&r.project, "origin", "main"), PullOutcome::Dirty { files } if files.len() == 10));
}
