//! `orq worktree list --repo <path>` lists the worktrees orq made that runs left behind, and
//! `orq worktree remove --repo <path> <branch> [--yes] [--force]` deletes one with its branch: a merged one
//! with only ignored files after a yes, unmerged work only with --force and its branch name typed back.
//! `orq storage status` counts them.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use orq_orchestrator::{Event, RunRecord, RunStore};

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

struct Sandbox {
    home: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let home = orq_test_support::process_temp_dir().join(format!("orq-worktree-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        std::fs::write(repo.join(".gitignore"), ".claude/settings.local.json\n.claude/worktrees/\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        Sandbox { home, repo }
    }

    fn orq(&self, args: &[&str], stdin: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orq"));
        command.args(args);
        command.env("HOME", &self.home);
        command.env("XDG_DATA_HOME", self.home.join("data"));
        command.env("XDG_STATE_HOME", self.home.join("state"));
        command.env("XDG_CONFIG_HOME", self.home.join("config"));
        command.env("XDG_CACHE_HOME", self.home.join("cache"));
        command.env_remove("HERDR_ACTIVE_PANE_CWD");
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn repo_arg(&self) -> &str {
        self.repo.to_str().unwrap()
    }

    /// A worktree on `branch` that run `run_id` made and cleanup kept.
    fn kept(&self, run_id: &str, branch: &str) -> PathBuf {
        let path = self.repo.join(".claude/worktrees").join(branch);
        git(&self.repo, &["worktree", "add", "-q", "-b", branch, path.to_str().unwrap()]);
        let store = RunStore::new(&self.home.join("state/orq"));
        store
            .create(&RunRecord {
                delivery: Default::default(),
                run_id: run_id.to_string(),
                project_id: "p".to_string(),
                feature_id: branch.trim_start_matches("fix--").to_string(),
                remote: "origin".to_string(),
                provider: "github".to_string(),
                target_branch: "main".to_string(),
                base_sha: git(&self.repo, &["rev-parse", "main"]),
                branch: branch.to_string(),
                worktree: path.clone(),
                repository: String::new(),
                profiles: BTreeMap::new(),
            })
            .unwrap();
        store
            .append_event(&Event {
                schema_version: 1,
                event_id: format!("{run_id}-0"),
                run_id: run_id.to_string(),
                project_id: "p".to_string(),
                feature_id: branch.trim_start_matches("fix--").to_string(),
                kind: "worktree.kept".to_string(),
                stage: Some("cleanup".to_string()),
                attempt: 1,
                occurred_at: "2026-11-16T00:00:00Z".to_string(),
                payload: serde_json::json!({ "worktree": path, "reason": "the worktree has uncommitted changes", "run_succeeded": true }),
            })
            .unwrap();
        path
    }

    /// A kept worktree with one commit `main` does not have.
    fn unmerged(&self, run_id: &str, branch: &str) -> PathBuf {
        let path = self.kept(run_id, branch);
        std::fs::write(path.join("b.txt"), "b\n").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-q", "-m", "b"]);
        path
    }

    fn branch_exists(&self, branch: &str) -> bool {
        Command::new("git").arg("-C").arg(&self.repo).args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).output().unwrap().status.success()
    }
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn list_shows_each_kept_worktree_with_its_run_commits_files_and_size() {
    let s = Sandbox::new("list");
    let none = s.orq(&["worktree", "list", "--repo", s.repo_arg()], "");
    assert!(none.status.success(), "{}", text(&none));
    assert!(text(&none).contains("No kept worktrees"), "{}", text(&none));

    let merged = s.kept("run-1", "fix--A");
    std::fs::create_dir_all(merged.join(".claude")).unwrap();
    std::fs::write(merged.join(".claude/settings.local.json"), "{}\n").unwrap();
    s.unmerged("run-2", "fix--B");

    let out = s.orq(&["worktree", "list", "--repo", s.repo_arg()], "");
    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("Kept worktrees: 2"), "{shown}");
    let line_a = shown.lines().find(|l| l.contains("fix--A")).expect(&shown);
    for part in ["run run-1", "completed", "merged", "safe to delete"] {
        assert!(line_a.contains(part), "{part}: {line_a}");
    }
    assert!(shown.contains(".claude/settings.local.json (ignored)"), "{shown}");
    let line_b = shown.lines().find(|l| l.contains("fix--B")).expect(&shown);
    assert!(line_b.contains("1 commit not on main") && line_b.contains("needs --force"), "{line_b}");
    assert!(shown.contains("kept because the worktree has uncommitted changes"), "{shown}");
}

#[test]
fn remove_deletes_a_merged_worktree_after_a_yes_and_keeps_it_on_a_no() {
    let s = Sandbox::new("remove");
    let path = s.kept("run-1", "fix--A");

    let declined = s.orq(&["worktree", "remove", "--repo", s.repo_arg(), "fix--A"], "n\n");
    assert!(text(&declined).contains("Delete this worktree and its branch?"), "{}", text(&declined));
    assert!(!declined.status.success() && path.exists() && s.branch_exists("fix--A"), "{}", text(&declined));

    let out = s.orq(&["worktree", "remove", "--repo", s.repo_arg(), "fix--A"], "y\n");
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Deleted fix--A"), "{}", text(&out));
    assert!(!path.exists() && !s.branch_exists("fix--A"));
}

#[test]
fn unmerged_work_needs_force_and_its_branch_typed_back_unless_yes() {
    let s = Sandbox::new("force");
    let path = s.unmerged("run-1", "fix--B");

    let refused = s.orq(&["worktree", "remove", "--repo", s.repo_arg(), "fix--B", "--yes"], "");
    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(text(&refused).contains("--force"), "{}", text(&refused));
    assert!(path.exists() && s.branch_exists("fix--B"));

    let mistyped = s.orq(&["worktree", "remove", "--repo", s.repo_arg(), "fix--B", "--force"], "fix--\n");
    assert!(text(&mistyped).contains("1 commit not on main"), "{}", text(&mistyped));
    assert!(!mistyped.status.success() && path.exists(), "{}", text(&mistyped));

    let typed = s.orq(&["worktree", "remove", "--repo", s.repo_arg(), "fix--B", "--force"], "fix--B\n");
    assert!(typed.status.success(), "{}", text(&typed));
    assert!(!path.exists() && !s.branch_exists("fix--B"));

    let other = s.unmerged("run-2", "fix--C");
    let yes = s.orq(&["worktree", "remove", "--repo", s.repo_arg(), "fix--C", "--force", "--yes"], "");
    assert!(yes.status.success(), "{}", text(&yes));
    assert!(!other.exists());
}

#[test]
fn an_unknown_branch_or_a_bad_command_line_is_refused() {
    let s = Sandbox::new("unknown");
    let out = s.orq(&["worktree", "remove", "--repo", s.repo_arg(), "fix--Nope", "--yes"], "");
    assert!(!out.status.success());
    assert!(text(&out).contains("no kept worktree"), "{}", text(&out));
    let bad = s.orq(&["worktree", "remove", "--repo", s.repo_arg()], "");
    assert_eq!(bad.status.code(), Some(2), "{}", text(&bad));
    assert!(text(&bad).contains("orq worktree list|remove"), "{}", text(&bad));
}

#[test]
fn storage_status_counts_the_kept_worktrees() {
    let s = Sandbox::new("status");
    let none = s.orq(&["storage", "status"], "");
    assert!(text(&none).contains("kept worktrees: none"), "{}", text(&none));
    s.kept("run-1", "fix--A");
    s.unmerged("run-2", "fix--B");
    let out = s.orq(&["storage", "status"], "");
    assert!(out.status.success(), "{}", text(&out));
    let line = text(&out).lines().find(|l| l.starts_with("kept worktrees:")).map(String::from).expect(&text(&out));
    assert!(line.starts_with("kept worktrees: 2 in 1 project(s)"), "{line}");
    assert!(line.contains("orq worktree list --repo"), "{line}");
}
