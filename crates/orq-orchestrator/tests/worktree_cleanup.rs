use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orq_orchestrator::{cleanup_worktree, CiOutcome, CleanupOutcome, RunRecord, RunStore};

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

/// A repository on `main` with a linked worktree on `fix--COM-02` holding one commit of its own.
fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-cleanup-{}-{tag}", std::process::id()));
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
    fs::write(worktree.join("mine.txt"), "mine\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "mine"]);

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
    World {
        store,
        repo,
        worktree,
    }
}

fn kinds(w: &World) -> Vec<String> {
    w.store
        .events("run-1")
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn a_review_ready_run_with_ci_disabled_has_its_worktree_removed_and_its_branch_kept() {
    let w = world("disabled");

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Skipped).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists(), "the worktree directory is gone");
    let listed = git(&w.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains(w.worktree.to_str().unwrap()),
        "no longer registered: {listed}"
    );
    git(
        &w.repo,
        &["rev-parse", "--verify", "refs/heads/fix--COM-02"],
    );
    assert_eq!(kinds(&w), ["worktree.removed"]);
}

#[test]
fn a_run_whose_ci_passed_has_its_worktree_removed() {
    let w = world("ci-green");

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists());
}

#[test]
fn a_run_whose_ci_did_not_pass_keeps_its_worktree() {
    for (tag, ci) in [
        (
            "ci-red",
            CiOutcome::Failed {
                failing: vec!["build".to_string()],
            },
        ),
        ("ci-timeout", CiOutcome::TimedOut),
        (
            "ci-error",
            CiOutcome::Errored {
                reason: "provider down".to_string(),
            },
        ),
    ] {
        let w = world(tag);

        let outcome = cleanup_worktree(&w.store, "run-1", true, &ci).unwrap();

        assert!(
            matches!(outcome, CleanupOutcome::Kept { .. }),
            "{tag}: {outcome:?}"
        );
        assert!(
            w.worktree.join("mine.txt").exists(),
            "{tag}: the worktree is untouched"
        );
        assert_eq!(kinds(&w), ["worktree.kept"], "{tag}");
    }
}

#[test]
fn a_run_that_did_not_reach_review_ready_keeps_its_worktree() {
    let w = world("not-ready");

    let outcome = cleanup_worktree(&w.store, "run-1", false, &CiOutcome::Skipped).unwrap();

    assert!(
        matches!(outcome, CleanupOutcome::Kept { .. }),
        "{outcome:?}"
    );
    assert!(w.worktree.join("mine.txt").exists());
}

#[test]
fn a_worktree_holding_an_ignored_file_is_kept_with_it_intact() {
    let w = world("ignored");
    fs::write(w.worktree.join(".gitignore"), "local.env\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore local config"]);
    fs::write(w.worktree.join("local.env"), "SECRET=1\n").unwrap();

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    assert!(
        matches!(outcome, CleanupOutcome::Kept { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        fs::read_to_string(w.worktree.join("local.env")).unwrap(),
        "SECRET=1\n"
    );
    assert_eq!(kinds(&w), ["worktree.kept"]);
}

#[test]
fn a_worktree_holding_only_an_ignored_build_cache_has_it_removed_with_the_worktree() {
    let w = world("build-cache");
    fs::write(w.worktree.join(".gitignore"), "target/\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore build output"]);
    // What cargo leaves behind: a directory marked as a cache by the CACHEDIR.TAG convention.
    let target = w.worktree.join("target/debug");
    fs::create_dir_all(&target).unwrap();
    fs::write(
        w.worktree.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n",
    )
    .unwrap();
    fs::write(target.join("orq"), "binary").unwrap();

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Skipped).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists(), "the worktree directory is gone");
    assert_eq!(kinds(&w), ["worktree.removed"]);
}

const CACHE_SIGNATURE: &str =
    "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n";

/// A worktree that ignores `target/`, holds a tagged cache there, and has a git repository of its own
/// at `target/tmp/holder` (what a test leaves behind). Git lists such a repository as one folder entry.
fn world_with_nested_repo(tag: &str, cache_tag: Option<&str>) -> World {
    let w = world(tag);
    fs::write(w.worktree.join(".gitignore"), "target/\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore build output"]);
    let holder = w.worktree.join("target/tmp/holder");
    fs::create_dir_all(&holder).unwrap();
    git(&holder, &["init", "-q"]);
    if let Some(text) = cache_tag {
        fs::write(w.worktree.join("target/CACHEDIR.TAG"), text).unwrap();
    }
    w
}

#[test]
fn a_nested_repository_inside_a_build_cache_is_removed_with_the_worktree() {
    let w = world_with_nested_repo("nested-in-cache", Some(CACHE_SIGNATURE));

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Skipped).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists(), "the worktree directory is gone");
    assert_eq!(kinds(&w), ["worktree.removed"]);
}

#[test]
fn a_nested_repository_without_a_proven_cache_around_it_keeps_the_worktree_and_is_named() {
    use std::os::unix::fs::symlink;
    // Each case: how `target/CACHEDIR.TAG` is made (None: no tag at all).
    type Tag = Option<fn(&Path)>;
    let cases: [(&str, Tag); 4] = [
        ("no-tag", None),
        ("wrong-signature", Some(|p| fs::write(p, "Signature: not-the-cache-signature\n").unwrap())),
        ("empty-tag", Some(|p| fs::write(p, "").unwrap())),
        (
            "symlinked-tag",
            Some(|p| {
                let real = p.with_file_name("real-tag");
                fs::write(&real, CACHE_SIGNATURE).unwrap();
                symlink(&real, p).unwrap();
            }),
        ),
    ];
    for (tag, make_tag) in cases {
        let w = world_with_nested_repo(&format!("nested-not-cache-{tag}"), None);
        if let Some(make_tag) = make_tag {
            make_tag(&w.worktree.join("target/CACHEDIR.TAG"));
        }

        let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

        let CleanupOutcome::Kept { reason } = outcome else { panic!("{tag}: the worktree was removed") };
        // Git may list the odd tag file before the folder, so the reason names the first of them.
        assert!(reason.contains("target/"), "{tag}: the reason names what it kept: {reason}");
        assert!(w.worktree.join("target/tmp/holder/.git").exists(), "{tag}: the repository is intact");
        assert_eq!(kinds(&w), ["worktree.kept"], "{tag}");
    }
}

#[test]
fn a_nested_repository_beside_a_tagged_cache_not_inside_it_keeps_the_worktree() {
    let w = world("nested-beside-cache");
    fs::write(w.worktree.join(".gitignore"), "target/\nscratch/\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore"]);
    fs::create_dir_all(w.worktree.join("target")).unwrap();
    fs::write(w.worktree.join("target/CACHEDIR.TAG"), CACHE_SIGNATURE).unwrap();
    let holder = w.worktree.join("scratch/holder");
    fs::create_dir_all(&holder).unwrap();
    git(&holder, &["init", "-q"]);

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    let CleanupOutcome::Kept { reason } = outcome else { panic!("the worktree was removed") };
    assert!(reason.contains("scratch/holder"), "{reason}");
    assert!(holder.join(".git").exists(), "the repository is intact");
}

#[test]
fn a_build_cache_of_many_files_in_nested_directories_is_removed_with_the_worktree() {
    let w = world("build-cache-large");
    fs::write(w.worktree.join(".gitignore"), "target/\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore build output"]);
    fs::create_dir_all(w.worktree.join("target")).unwrap();
    fs::write(
        w.worktree.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    for dir in 0..10 {
        let nested = w.worktree.join(format!("target/debug/deps/{dir}/inner"));
        fs::create_dir_all(&nested).unwrap();
        for file in 0..30 {
            fs::write(nested.join(format!("artifact-{file}")), "built").unwrap();
        }
    }

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Skipped).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists(), "the worktree directory is gone");
}

#[test]
fn a_build_cache_in_a_directory_with_a_non_ascii_name_is_removed_with_the_worktree() {
    let w = world("build-cache-non-ascii");
    fs::write(w.worktree.join(".gitignore"), "bygg-ö/\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore build output"]);
    // Git reports such a name quoted, with octal escapes, unless asked for it plainly.
    let cache = w.worktree.join("bygg-ö/debug");
    fs::create_dir_all(&cache).unwrap();
    fs::write(
        w.worktree.join("bygg-ö/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    fs::write(cache.join("orq"), "binary").unwrap();

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Skipped).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists(), "the worktree directory is gone");
}

#[test]
fn a_non_utf8_directory_name_never_borrows_the_cache_tag_of_a_look_alike_directory() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let w = world("non-utf8-collision");
    fs::write(w.worktree.join(".gitignore"), "cache-*/\n").unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore build output"]);
    // A tagged cache named with the replacement character, and an untagged directory whose
    // invalid byte a lossy reading would turn into that same name.
    let tagged = w.worktree.join("cache-\u{FFFD}");
    fs::create_dir_all(&tagged).unwrap();
    fs::write(
        tagged.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    let raw = w.worktree.join(OsStr::from_bytes(b"cache-\xff"));
    fs::create_dir_all(&raw).unwrap();
    fs::write(raw.join("work.txt"), "not rebuildable").unwrap();

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Skipped).unwrap();

    assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{outcome:?}");
    assert!(raw.join("work.txt").exists(), "the file is intact");
}

#[test]
fn a_worktree_is_kept_with_everything_intact_unless_its_ignored_files_are_all_in_a_proven_build_cache() {
    use std::os::unix::fs::symlink;
    const SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";
    // Each case: how `target/CACHEDIR.TAG` is made (None: no tag), and a file outside the cache.
    type Tag = Option<fn(&Path)>;
    let cases: [(&str, Tag, bool); 7] = [
        // A pipe nobody writes to would block a reader forever.
        ("fifo-tag", Some(|p| assert!(Command::new("mkfifo").arg(p).status().unwrap().success())), false),
        ("wrong-signature", Some(|p| fs::write(p, "Signature: not-the-cache-signature\n").unwrap()), false),
        ("truncated-signature", Some(|p| fs::write(p, &SIGNATURE[..20]).unwrap()), false),
        ("empty-tag", Some(|p| fs::write(p, "").unwrap()), false),
        (
            "symlinked-tag",
            Some(|p| {
                let real = p.with_file_name("real-tag");
                fs::write(&real, SIGNATURE).unwrap();
                symlink(&real, p).unwrap();
            }),
            false,
        ),
        ("no-tag", None, false),
        ("valid-tag-beside-an-unrelated-ignored-file", Some(|p| fs::write(p, SIGNATURE).unwrap()), true),
    ];
    for (tag, make_tag, unrelated) in cases {
        let w = world(&format!("not-a-cache-{tag}"));
        fs::write(w.worktree.join(".gitignore"), "target/\nlocal.env\n").unwrap();
        git(&w.worktree, &["add", ".gitignore"]);
        git(&w.worktree, &["commit", "-q", "-m", "ignore"]);
        fs::create_dir_all(w.worktree.join("target")).unwrap();
        fs::write(w.worktree.join("target/artifact"), "built").unwrap();
        if let Some(make_tag) = make_tag {
            make_tag(&w.worktree.join("target/CACHEDIR.TAG"));
        }
        if unrelated {
            fs::write(w.worktree.join("local.env"), "SECRET=1\n").unwrap();
        }

        let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

        assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{tag}: {outcome:?}");
        assert_eq!(fs::read_to_string(w.worktree.join("target/artifact")).unwrap(), "built", "{tag}");
        if unrelated {
            assert_eq!(fs::read_to_string(w.worktree.join("local.env")).unwrap(), "SECRET=1\n", "{tag}");
        }
        assert_eq!(kinds(&w), ["worktree.kept"], "{tag}");
    }
}

#[test]
fn a_locked_worktree_is_kept_registered_with_its_branch() {
    let w = world("locked");
    git(
        &w.repo,
        &[
            "worktree",
            "lock",
            "--reason",
            "in use",
            w.worktree.to_str().unwrap(),
        ],
    );

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    let CleanupOutcome::Kept { reason } = outcome else {
        panic!("expected Kept, got {outcome:?}")
    };
    assert!(reason.contains("would not remove"), "{reason}");
    assert!(w.worktree.join("mine.txt").exists());
    let listed = git(&w.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        listed.contains(w.worktree.to_str().unwrap()),
        "still registered: {listed}"
    );
    git(
        &w.repo,
        &["rev-parse", "--verify", "refs/heads/fix--COM-02"],
    );
    assert_eq!(kinds(&w), ["worktree.kept"]);
}

#[test]
fn a_worktree_directory_that_is_already_gone_is_kept_with_a_recorded_reason() {
    let w = world("missing");
    fs::remove_dir_all(&w.worktree).unwrap();

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed);

    assert!(
        matches!(outcome, Ok(CleanupOutcome::Kept { .. })),
        "{outcome:?}"
    );
    assert_eq!(kinds(&w), ["worktree.kept"]);
    git(
        &w.repo,
        &["rev-parse", "--verify", "refs/heads/fix--COM-02"],
    );
}

#[test]
fn a_worktree_with_uncommitted_changes_is_kept_with_them_intact() {
    for (tag, edit) in [("modified", "mine.txt"), ("untracked", "scratch.txt")] {
        let w = world(tag);
        fs::write(w.worktree.join(edit), "unsaved work\n").unwrap();

        let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

        assert!(
            matches!(outcome, CleanupOutcome::Kept { .. }),
            "{tag}: {outcome:?}"
        );
        assert_eq!(
            fs::read_to_string(w.worktree.join(edit)).unwrap(),
            "unsaved work\n",
            "{tag}"
        );
        assert_eq!(kinds(&w), ["worktree.kept"], "{tag}");
    }
}

fn summary(w: &World) -> orq_orchestrator::RunSummary {
    orq_orchestrator::summarize_run(&w.store.events("run-1").unwrap())
}

fn record_failure(w: &World, kind: &str, reason: &str) {
    w.store
        .append_event(&orq_orchestrator::Event {
            schema_version: 1,
            event_id: format!("e-{kind}"),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "COM-02".to_string(),
            kind: kind.to_string(),
            stage: Some("verify".to_string()),
            attempt: 1,
            occurred_at: "2026-11-02T10:00:00Z".to_string(),
            payload: serde_json::json!({ "reason": reason }),
        })
        .unwrap();
}

#[test]
fn cleanup_events_of_a_failed_run_leave_its_summary_failed_with_the_reason() {
    let w = world("summary-failed");
    record_failure(&w, "run.failed", "gate fix budget spent");

    cleanup_worktree(&w.store, "run-1", false, &CiOutcome::Skipped).unwrap();

    let summary = summary(&w);
    assert_eq!(summary.phase, orq_orchestrator::RunPhase::Failed);
    assert_eq!(summary.reason.as_deref(), Some("gate fix budget spent"));
}

#[test]
fn cleanup_events_of_a_run_with_red_ci_do_not_complete_it() {
    let w = world("summary-ci-red");

    cleanup_worktree(
        &w.store,
        "run-1",
        true,
        &CiOutcome::Failed {
            failing: vec!["build".to_string()],
        },
    )
    .unwrap();

    assert_ne!(summary(&w).phase, orq_orchestrator::RunPhase::Completed);
}

#[test]
fn cleanup_events_of_a_successful_run_complete_it_whether_removed_or_kept_dirty() {
    let removed = world("summary-removed");
    cleanup_worktree(&removed.store, "run-1", true, &CiOutcome::Passed).unwrap();
    assert_eq!(
        summary(&removed).phase,
        orq_orchestrator::RunPhase::Completed
    );

    let dirty = world("summary-dirty");
    fs::write(dirty.worktree.join("scratch.txt"), "unsaved\n").unwrap();
    cleanup_worktree(&dirty.store, "run-1", true, &CiOutcome::Passed).unwrap();
    assert_eq!(summary(&dirty).phase, orq_orchestrator::RunPhase::Completed);
}

/// Commits a `.gitignore` in the worktree so the files a test then writes are ignored, not untracked.
fn ignore(w: &World, patterns: &str) {
    fs::write(w.worktree.join(".gitignore"), patterns).unwrap();
    git(&w.worktree, &["add", ".gitignore"]);
    git(&w.worktree, &["commit", "-q", "-m", "ignore local files"]);
}

fn write(w: &World, path: &str, text: &str) {
    let path = w.worktree.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const TDD_STATE_FILES: [&str; 8] = [
    ".tdd-config.json",
    ".tdd-state.json",
    ".tdd-state.json.lock",
    ".tdd-review-findings.json",
    ".tdd-review-findings.json.lock",
    ".tdd-review-rounds.json",
    ".tdd-session.log",
    ".project-mcp/index.db",
];

#[test]
fn a_worktree_holding_only_the_tdd_tooling_state_files_has_them_removed_with_the_worktree() {
    let w = world("tdd-state");
    ignore(&w, ".tdd-*\n.project-mcp/\n");
    for file in TDD_STATE_FILES {
        write(&w, file, "{}\n");
    }

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists(), "the worktree directory is gone");
    assert_eq!(kinds(&w), ["worktree.removed"]);
}

#[test]
fn an_ignored_file_of_the_agent_keeps_the_worktree_and_the_reason_names_it_not_the_tooling_files() {
    let w = world("named-reason");
    ignore(&w, ".tdd-*\nlocal.cfg\n");
    write(&w, ".tdd-state.json", "{}\n");
    write(&w, "local.cfg", "SECRET=1\n");

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    let CleanupOutcome::Kept { reason } = outcome else { panic!("{outcome:?}") };
    assert!(reason.contains("local.cfg"), "{reason}");
    assert!(!reason.contains(".tdd-state.json"), "{reason}");
    assert_eq!(fs::read_to_string(w.worktree.join("local.cfg")).unwrap(), "SECRET=1\n");
    assert_eq!(kinds(&w), ["worktree.kept"]);
}

#[test]
fn the_tooling_folders_a_session_installs_are_removed_with_the_worktree() {
    let w = world("tool-dirs");
    ignore(&w, ".agents/\n.codex/\n.pi/\n.claude/\n");
    for file in [
        ".agents/skills/tdd-start/SKILL.md",
        ".codex/config.toml",
        ".pi/APPEND_SYSTEM.md",
        ".claude/agents/reviewer.md",
        ".claude/commands/tdd.md",
        ".claude/settings.json",
    ] {
        write(&w, file, "x\n");
    }

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!w.worktree.exists());
}

#[test]
fn a_run_worktree_nested_under_the_claude_folder_is_never_swept_away_with_its_parent() {
    let w = world("nested-worktrees");
    ignore(&w, ".claude/\n");
    write(&w, ".claude/worktrees/other/work.txt", "someone's work\n");

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{outcome:?}");
    assert!(w.worktree.join(".claude/worktrees/other/work.txt").exists());
}

#[test]
fn files_orq_itself_writes_stay_work_unless_recorded_unchanged_even_though_the_tooling_names_them() {
    for file in [".mcp.json", ".mcpctl.toml", ".mcpctl.lock", ".claude/settings.local.json"] {
        let w = world("orq-named");
        ignore(&w, ".mcp.json\n.mcpctl*\n.claude/\n");
        write(&w, file, "mine\n");

        let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

        assert!(matches!(outcome, CleanupOutcome::Kept { .. }), "{file}: {outcome:?}");
    }
}

#[test]
fn the_kept_reason_shows_control_characters_in_a_file_name_escaped_not_raw() {
    let w = world("hostile-name");
    ignore(&w, "*.cfg\n");
    write(&w, "a\nforged: line\x1b[31m.cfg", "mine\n");

    let outcome = cleanup_worktree(&w.store, "run-1", true, &CiOutcome::Passed).unwrap();

    let CleanupOutcome::Kept { reason } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(!reason.contains('\n') && !reason.contains('\x1b'), "{reason:?}");
    assert!(reason.contains("forged: line"), "{reason:?}");
}
