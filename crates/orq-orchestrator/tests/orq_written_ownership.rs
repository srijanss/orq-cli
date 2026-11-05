//! Cleanup exempts the ignored files orq wrote into a worktree only while they are still what orq wrote.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{
    cleanup_worktree, record_orq_written, CiOutcome, CleanupOutcome, RunRecord, RunStore,
};

const DOTENV: &str = concat!(".", "env");

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

struct World {
    store: RunStore,
    repo: PathBuf,
    worktree: PathBuf,
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-owned-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let worktree = root.join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fix--COM-02",
            worktree.to_str().unwrap(),
        ],
    );
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
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
    World {
        store,
        repo,
        worktree,
    }
}

/// The files orq writes into a worktree, ignored through the repository's local exclude.
fn orq_files(w: &World) {
    let exclude = w.repo.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    fs::write(&exclude, ".mcp.json\n.claude/settings.local.json\n").unwrap();
    fs::write(w.worktree.join(".mcp.json"), "{\"mcpServers\":{}}\n").unwrap();
    fs::create_dir_all(w.worktree.join(".claude")).unwrap();
    fs::write(
        w.worktree.join(".claude/settings.local.json"),
        "{\"env\":{}}\n",
    )
    .unwrap();
}

fn cleanup(w: &World) -> CleanupOutcome {
    cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap()
}

#[test]
fn files_orq_wrote_and_recorded_do_not_keep_the_worktree() {
    let w = world("unchanged");
    orq_files(&w);
    assert!(record_orq_written(&w.worktree).is_empty());

    assert_eq!(cleanup(&w), CleanupOutcome::Removed);
}

#[test]
fn an_orq_file_changed_after_it_was_recorded_is_work_and_keeps_the_worktree() {
    let w = world("edited");
    orq_files(&w);
    record_orq_written(&w.worktree);
    fs::write(
        w.worktree.join(".mcp.json"),
        "{\"mcpServers\":{\"mine\":{}}}\n",
    )
    .unwrap();

    let outcome = cleanup(&w);

    assert!(
        matches!(outcome, CleanupOutcome::Kept { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        fs::read_to_string(w.worktree.join(".mcp.json")).unwrap(),
        "{\"mcpServers\":{\"mine\":{}}}\n"
    );
}

#[test]
fn an_orq_named_file_orq_never_recorded_is_not_exempt() {
    let w = world("unrecorded");
    orq_files(&w);

    let outcome = cleanup(&w);

    assert!(
        matches!(outcome, CleanupOutcome::Kept { .. }),
        "{outcome:?}"
    );
}

#[test]
fn an_ignored_directory_holding_other_files_is_enumerated_not_exempted() {
    let w = world("directory");
    orq_files(&w);
    fs::write(w.repo.join(".git/info/exclude"), ".mcp.json\n.claude/\n").unwrap();
    record_orq_written(&w.worktree);
    fs::write(w.worktree.join(".claude/notes.md"), "mine\n").unwrap();

    let outcome = cleanup(&w);

    assert!(
        matches!(outcome, CleanupOutcome::Kept { .. }),
        "{outcome:?}"
    );
    assert!(w.worktree.join(".claude/notes.md").exists());
}

#[test]
fn the_dotenv_link_is_exempt_only_while_it_points_where_orq_pointed_it() {
    let w = world("link");
    orq_files(&w);
    fs::write(
        w.repo.join(".git/info/exclude"),
        format!(".mcp.json\n.claude/settings.local.json\n{DOTENV}\n"),
    )
    .unwrap();
    let main_env = w.repo.join(DOTENV);
    fs::write(&main_env, "A=1\n").unwrap();
    std::os::unix::fs::symlink(&main_env, w.worktree.join(DOTENV)).unwrap();
    record_orq_written(&w.worktree);
    let elsewhere = w.repo.join("elsewhere");
    fs::write(&elsewhere, "B=2\n").unwrap();
    fs::remove_file(w.worktree.join(DOTENV)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, w.worktree.join(DOTENV)).unwrap();

    let outcome = cleanup(&w);

    assert!(
        matches!(outcome, CleanupOutcome::Kept { .. }),
        "{outcome:?}"
    );
}

#[test]
fn the_dotenv_link_orq_made_does_not_keep_the_worktree() {
    let w = world("link-kept");
    orq_files(&w);
    fs::write(
        w.repo.join(".git/info/exclude"),
        format!(".mcp.json\n.claude/settings.local.json\n{DOTENV}\n"),
    )
    .unwrap();
    let main_env = w.repo.join(DOTENV);
    fs::write(&main_env, "A=1\n").unwrap();
    std::os::unix::fs::symlink(&main_env, w.worktree.join(DOTENV)).unwrap();
    record_orq_written(&w.worktree);

    assert_eq!(cleanup(&w), CleanupOutcome::Removed);
}

#[test]
fn a_regular_dotenv_file_is_never_orqs_even_when_recorded_with_the_rest() {
    let w = world("regular-dotenv");
    orq_files(&w);
    fs::write(
        w.repo.join(".git/info/exclude"),
        format!(".mcp.json\n.claude/settings.local.json\n{DOTENV}\n"),
    )
    .unwrap();
    fs::write(w.worktree.join(DOTENV), "SECRET=1\n").unwrap();
    record_orq_written(&w.worktree);

    let outcome = cleanup(&w);

    assert!(
        matches!(outcome, CleanupOutcome::Kept { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        fs::read_to_string(w.worktree.join(DOTENV)).unwrap(),
        "SECRET=1\n"
    );
}

#[test]
fn recording_and_checking_run_no_configured_filter_and_see_raw_bytes() {
    let w = world("filters");
    orq_files(&w);
    let marker = w.repo.parent().unwrap().join("filter-ran");
    git(
        &w.repo,
        &[
            "config",
            "filter.spy.clean",
            &format!("touch {}; cat", marker.display()),
        ],
    );
    fs::write(
        w.repo.join(".git/info/attributes"),
        ".mcp.json filter=spy\n",
    )
    .unwrap();
    record_orq_written(&w.worktree);

    let outcome = cleanup(&w);

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!marker.exists(), "a clean filter ran while fingerprinting");
}

#[test]
fn a_malformed_or_unreadable_record_leaves_every_ignored_file_unowned() {
    let w = world("bad-record");
    orq_files(&w);
    record_orq_written(&w.worktree);
    let git_dir = git(&w.worktree, &["rev-parse", "--absolute-git-dir"]);
    let manifest = PathBuf::from(git_dir).join("orq-written.json");
    fs::write(&manifest, "{not json").unwrap();
    assert!(
        matches!(cleanup(&w), CleanupOutcome::Kept { .. }),
        "malformed record"
    );

    fs::remove_file(&manifest).unwrap();
    fs::create_dir(&manifest).unwrap();
    assert!(
        matches!(cleanup(&w), CleanupOutcome::Kept { .. }),
        "unreadable record"
    );
    assert!(w.worktree.join(".mcp.json").exists());
}

#[test]
fn a_record_that_cannot_be_written_is_reported_as_a_note() {
    let w = world("unwritable-record");
    orq_files(&w);
    let git_dir = git(&w.worktree, &["rev-parse", "--absolute-git-dir"]);
    fs::create_dir(PathBuf::from(git_dir).join("orq-written.json")).unwrap();

    let notes = record_orq_written(&w.worktree);

    assert!(
        notes.iter().any(|n| n.contains("not recorded")),
        "{notes:?}"
    );
}

#[test]
fn a_recorded_link_replaced_by_a_regular_file_is_work() {
    let w = world("link-to-file");
    orq_files(&w);
    fs::write(
        w.repo.join(".git/info/exclude"),
        format!(".mcp.json\n.claude/settings.local.json\n{DOTENV}\n"),
    )
    .unwrap();
    let main_env = w.repo.join(DOTENV);
    fs::write(&main_env, "A=1\n").unwrap();
    std::os::unix::fs::symlink(&main_env, w.worktree.join(DOTENV)).unwrap();
    record_orq_written(&w.worktree);
    fs::remove_file(w.worktree.join(DOTENV)).unwrap();
    fs::write(w.worktree.join(DOTENV), "A=1\n").unwrap();

    assert!(matches!(cleanup(&w), CleanupOutcome::Kept { .. }));
}
