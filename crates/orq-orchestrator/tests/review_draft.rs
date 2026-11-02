use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use orq_orchestrator::{draft_review_description, DraftSession, DraftSource, RunRecord, RunStore};

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

/// A worktree whose branch has the commits `subjects` on top of a base commit.
fn world(tag: &str, subjects: &[&str]) -> World {
    let root = std::env::temp_dir().join(format!("orq-draft-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let worktree = root.join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "-q", "-b", "main"]);
    fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "base"]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    git(&worktree, &["checkout", "-q", "-b", "fix--COM-02"]);
    for (index, subject) in subjects.iter().enumerate() {
        fs::write(worktree.join(format!("f{index}.txt")), "x\n").unwrap();
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-q", "-m", subject]);
    }
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
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store, worktree }
}

fn session(w: &World, script: &str) -> DraftSession {
    DraftSession {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        env: Vec::new(),
        cwd: w.worktree.clone(),
    }
}

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn the_agents_first_line_is_the_title_and_the_rest_the_description() {
    let w = world("agent", &["add the thing", "fix the thing"]);
    let seen = RefCell::new(String::new());

    let draft = draft_review_description(&w.store, "run-1", WAIT, |prompt| {
        *seen.borrow_mut() = prompt.to_string();
        session(&w, "printf 'Add the thing\\n\\nIt adds the thing.\\n'")
    })
    .unwrap();

    assert_eq!(draft.title, "Add the thing");
    assert_eq!(draft.body, "It adds the thing.");
    assert_eq!(draft.source, DraftSource::Agent);
    let prompt = seen.borrow();
    assert!(prompt.contains("add the thing") && prompt.contains("fix the thing"), "{prompt}");
}

#[test]
fn a_failing_agent_falls_back_to_the_first_commit_subject_and_the_commit_list() {
    let w = world("fail", &["add the thing", "fix the thing"]);

    let draft = draft_review_description(&w.store, "run-1", WAIT, |_| session(&w, "echo oops; exit 3")).unwrap();

    assert_eq!(draft.title, "add the thing");
    assert!(draft.body.contains("- add the thing") && draft.body.contains("- fix the thing"), "{}", draft.body);
    assert!(matches!(draft.source, DraftSource::Fallback { .. }));
}

#[test]
fn empty_output_or_a_program_that_cannot_start_also_fall_back() {
    let w = world("empty", &["add the thing"]);

    let empty = draft_review_description(&w.store, "run-1", WAIT, |_| session(&w, "printf '  \\n'")).unwrap();
    let mut missing = session(&w, "");
    missing.program = "/nonexistent/orq-drafter".to_string();
    let broken = draft_review_description(&w.store, "run-1", WAIT, |_| missing).unwrap();

    for draft in [empty, broken] {
        assert_eq!(draft.title, "add the thing");
        assert!(matches!(draft.source, DraftSource::Fallback { .. }));
    }
}

#[test]
fn a_run_without_commits_falls_back_to_the_feature_id() {
    let w = world("none", &[]);

    let draft = draft_review_description(&w.store, "run-1", WAIT, |_| session(&w, "exit 1")).unwrap();

    assert_eq!(draft.title, "COM-02");
}

#[test]
fn agent_output_is_cleaned_and_size_limited() {
    let w = world("limits", &["add the thing"]);
    let script = "printf '\\033[31mRed title\\033[0m'; head -c 300 /dev/zero | tr '\\0' 'T'; printf '\\n\\n'; head -c 40000 /dev/zero | tr '\\0' 'b'";

    let draft = draft_review_description(&w.store, "run-1", WAIT, |_| session(&w, script)).unwrap();

    assert!(!draft.title.contains('\u{1b}') && !draft.body.contains('\u{1b}'));
    assert!(draft.title.chars().count() <= 120, "{}", draft.title.len());
    assert!(draft.body.chars().count() <= 8000, "{}", draft.body.len());
    assert_eq!(draft.title.chars().count(), 120, "an over-long title is cut, not dropped");
    assert_eq!(draft.source, DraftSource::Agent);
}

#[test]
fn an_agent_that_never_finishes_is_stopped_and_the_fallback_used() {
    let w = world("hang", &["add the thing"]);
    let started = Instant::now();

    let draft =
        draft_review_description(&w.store, "run-1", Duration::from_millis(300), |_| session(&w, "sleep 30")).unwrap();

    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    assert_eq!(draft.title, "add the thing");
    assert!(matches!(draft.source, DraftSource::Fallback { .. }));
}

#[test]
fn the_stage_records_how_the_draft_was_made() {
    let w = world("events", &["add the thing"]);

    draft_review_description(&w.store, "run-1", WAIT, |_| session(&w, "exit 1")).unwrap();

    let events = w.store.events("run-1").unwrap();
    let kinds: Vec<_> = events.iter().map(|e| (e.stage.clone().unwrap_or_default(), e.kind.clone())).collect();
    assert_eq!(
        kinds,
        [("review_draft".to_string(), "stage.started".to_string()), ("review_draft".to_string(), "stage.completed".to_string())]
    );
    assert_eq!(events[1].payload["source"], "fallback");
}

#[test]
fn an_unknown_run_is_an_error_and_no_session_is_built() {
    let w = world("unknown", &["add the thing"]);
    let built = RefCell::new(false);

    let result = draft_review_description(&w.store, "ghost", WAIT, |_| {
        *built.borrow_mut() = true;
        session(&w, "exit 0")
    });

    assert!(result.is_err());
    assert!(!*built.borrow());
}
