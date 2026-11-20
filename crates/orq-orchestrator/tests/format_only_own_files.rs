//! A session formats only its own files: its settings allow `cargo fmt -- <file>...` and refuse a
//! formatting of the whole workspace or a package, and after the implement step a file outside the
//! features' declared files whose change is formatting only is put back in a commit of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use orq_orchestrator::{
    allows_command, refresh_run_permissions, undo_formatting_outside, RunRecord, RunStore,
};
use orq_test_support::GitFixture;

fn git(worktree: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "-c",
            "user.name=Orq Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Well formatted, and the same code written so rustfmt changes it.
const TIDY: &str = "pub fn tidy(a: u32) -> u32 {\n    a + 1\n}\n";
const UNTIDY: &str = "pub fn tidy( a:u32 )->u32{ a+1 }\n";

/// A run whose worktree's base holds `files` (path, contents) untidy, then the session's commit
/// writing `changes`: the store, the record and the worktree.
fn run_with(
    tag: &str,
    files: &[(&str, &str)],
    changes: &[(&str, &str)],
) -> (GitFixture, RunStore, RunRecord) {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree(tag);
    git(&worktree, &["config", "user.name", "Orq Test"]);
    git(&worktree, &["config", "user.email", "test@example.invalid"]);
    let commit = |files: &[(&str, &str)], message: &str| {
        for (path, contents) in files {
            let file = worktree.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
            git(&worktree, &["add", path]);
        }
        git(&worktree, &["commit", "--quiet", "-m", message]);
    };
    commit(files, "base");
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    commit(changes, "session");
    let store = RunStore::new(&repo.root().join(format!("state-{tag}")));
    let record = RunRecord {
        delivery: Default::default(),
        run_id: "run-1".into(),
        project_id: "p".into(),
        feature_id: "F".into(),
        remote: "origin".into(),
        provider: "github".into(),
        target_branch: "main".into(),
        base_sha: base,
        branch: "orq-run-1".into(),
        worktree,
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.start_run(&record).unwrap();
    (repo, store, record)
}

fn declared(files: &[&str]) -> BTreeSet<String> {
    files.iter().map(|file| file.to_string()).collect()
}

fn reverted_events(store: &RunStore) -> Vec<serde_json::Value> {
    store
        .events("run-1")
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "session.format_reverted")
        .map(|event| event.payload)
        .collect()
}

#[test]
fn formatting_of_an_undeclared_file_is_undone_in_a_commit_of_its_own() {
    let (_repo, store, record) = run_with(
        "fmt-undeclared",
        &[("src/a.rs", UNTIDY), ("src/other.rs", UNTIDY)],
        &[("src/a.rs", "pub fn changed() {}\n"), ("src/other.rs", TIDY)],
    );
    let worktree = &record.worktree;
    let before = git(worktree, &["rev-parse", "HEAD"]);

    let reverted = undo_formatting_outside(
        &store,
        &record,
        &record.base_sha,
        &declared(&["src/a.rs"]),
        "implement_and_review",
    )
    .unwrap();

    assert_eq!(reverted, vec!["src/other.rs".to_string()]);
    assert_eq!(git(worktree, &["rev-parse", "HEAD~1"]), before, "a new commit on the session's");
    assert_eq!(
        git(worktree, &["log", "-1", "--format=%s"]),
        "chore: undo formatting outside the feature's files"
    );
    assert_eq!(git(worktree, &["show", "HEAD:src/other.rs"]), UNTIDY.trim_end());
    assert_eq!(git(worktree, &["show", "HEAD:src/a.rs"]), "pub fn changed() {}");
    let events = reverted_events(&store);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["files"], serde_json::json!(["src/other.rs"]));
}

#[test]
fn an_undeclared_file_with_a_real_change_is_not_undone() {
    let real = "pub fn tidy(a: u32) -> u32 {\n    a + 2\n}\n";
    let (_repo, store, record) = run_with("fmt-real", &[("src/other.rs", UNTIDY)], &[("src/other.rs", real)]);
    let head = git(&record.worktree, &["rev-parse", "HEAD"]);

    let reverted = undo_formatting_outside(&store, &record, &record.base_sha, &declared(&[]), "implement_and_review").unwrap();

    assert!(reverted.is_empty(), "{reverted:?}");
    assert_eq!(git(&record.worktree, &["rev-parse", "HEAD"]), head, "no commit");
    assert_eq!(git(&record.worktree, &["show", "HEAD:src/other.rs"]), real.trim_end());
    assert!(reverted_events(&store).is_empty());
}

#[test]
fn a_declared_file_reformatted_is_kept() {
    let (_repo, store, record) = run_with("fmt-declared", &[("src/a.rs", UNTIDY)], &[("src/a.rs", TIDY)]);
    let head = git(&record.worktree, &["rev-parse", "HEAD"]);

    let reverted = undo_formatting_outside(&store, &record, &record.base_sha, &declared(&["src/a.rs"]), "implement_and_review").unwrap();

    assert!(reverted.is_empty(), "{reverted:?}");
    assert_eq!(git(&record.worktree, &["rev-parse", "HEAD"]), head, "no commit");
    assert_eq!(git(&record.worktree, &["show", "HEAD:src/a.rs"]), TIDY.trim_end());
    assert!(reverted_events(&store).is_empty());
}

#[test]
fn settings_an_earlier_orq_wrote_lose_the_bare_cargo_fmt_rule() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("fmt-retired");
    let settings = worktree.join(".claude/settings.local.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, r#"{"permissions":{"allow":["Bash(cargo fmt:*)"]}}"#).unwrap();
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    let rules = refresh_run_permissions(&worktree, &[], Path::new("/state/orq/logs/run-1"), &base)
        .unwrap()
        .allowed_commands;
    assert!(!allows_command(&rules, "cargo fmt --all"), "{rules:?}");
    let written = fs::read_to_string(&settings).unwrap();
    assert!(!written.contains("\"Bash(cargo fmt:*)\""), "{written}");
}

#[test]
fn settings_allow_formatting_named_files_and_refuse_the_workspace() {
    let repo = GitFixture::new();
    let worktree = repo.add_linked_worktree("fmt-rules");
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    let rules = refresh_run_permissions(&worktree, &[], Path::new("/state/orq/logs/run-1"), &base)
        .unwrap()
        .allowed_commands;
    assert!(allows_command(&rules, "cargo fmt -- src/a.rs"), "{rules:?}");
    for refused in ["cargo fmt", "cargo fmt --all", "cargo fmt -p x"] {
        assert!(!allows_command(&rules, refused), "{refused} allowed by {rules:?}");
    }
}
