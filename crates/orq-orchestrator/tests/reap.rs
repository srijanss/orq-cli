use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orq_orchestrator::{plan_reap, reap_run, record_run_completed, Event, ReapVerdict, RunRecord, RunStore};
use orq_test_support::FakeProvider;

const WAIT: Duration = Duration::from_secs(10);
const OTHER: &str = "3333333333333333333333333333333333333333";

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

#[derive(Clone, Copy)]
struct Case {
    state: &'static str,
    /// The provider reports the branch tip as the review's head.
    pushed: bool,
    dirty: bool,
    pinned: bool,
    finished: bool,
    /// Where the worktree lives relative to the repository.
    inside_worktrees_dir: bool,
    /// The worktree was detached and given a commit of its own, so it is not on the recorded branch.
    detached_ahead: bool,
    /// The provider CLI fails instead of answering.
    provider_fails: bool,
}

const GOOD: Case =
    Case { state: "MERGED", pushed: true, dirty: false, pinned: false, finished: true, inside_worktrees_dir: true, detached_ahead: false, provider_fails: false };

struct Planned {
    verdict: ReapVerdict,
    repo: PathBuf,
    worktree: PathBuf,
    item: orq_orchestrator::ReapItem,
}

fn scenario(tag: &str, case: Case) -> Planned {
    let root = orq_test_support::process_temp_dir().join(format!("orq-reap-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["remote", "add", "origin", "https://github.example/o/r.git"]);
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join(".git/info/exclude"), "*.log\n").unwrap();
    let worktree = if case.inside_worktrees_dir { repo.join(".claude/worktrees/fix--COM-02") } else { root.join("elsewhere") };
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(&repo, &["worktree", "add", "-q", "-b", "fix--COM-02", worktree.to_str().unwrap()]);
    fs::write(worktree.join("mine.txt"), "mine\n").unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-q", "-m", "mine"]);
    let tip = git(&worktree, &["rev-parse", "HEAD"]);
    if case.detached_ahead {
        git(&worktree, &["checkout", "-q", "--detach"]);
        fs::write(worktree.join("only-here.txt"), "unpushed\n").unwrap();
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-q", "-m", "only here"]);
    }
    if case.dirty {
        fs::write(worktree.join("scratch.txt"), "x\n").unwrap();
    }

    let store = RunStore::new(&root.join("state"));
    let record = RunRecord {
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "COM-02".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: base,
        branch: "fix--COM-02".to_string(),
        worktree: worktree.clone(),
        repository: "https://github.example/o/r".to_string(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    store
        .append_event(&Event {
            schema_version: 1,
            event_id: "e-created".to_string(),
            run_id: record.run_id.clone(),
            project_id: record.project_id.clone(),
            feature_id: record.feature_id.clone(),
            kind: "review.created".to_string(),
            stage: Some("review_request".to_string()),
            attempt: 1,
            occurred_at: "2026-01-01T00:00:00Z".to_string(),
            payload: serde_json::json!({ "url": "https://github.example/o/r/pull/7", "id": "7", "provider": "github" }),
        })
        .unwrap();
    if case.finished {
        record_run_completed(&store, &record, "review_ready", None).unwrap();
    }
    let head = if case.pushed { tip } else { OTHER.to_string() };
    let row = format!(
        r#"[{{"number":7,"url":"https://github.example/o/r/pull/7","state":"{}","headRefOid":"{head}","isCrossRepository":false}}]"#,
        case.state
    );
    let provider = if case.provider_fails {
        FakeProvider::builder().fail(&["pr", "list"], 1, "HTTP 502: Bad Gateway\n").build()
    } else {
        FakeProvider::builder().reply(&["pr", "list"], &format!("{row}\n")).build()
    };
    let gh = provider.gh();
    let pinned = if case.pinned { vec!["run-1".to_string()] } else { Vec::new() };
    let items = plan_reap(&store, &repo, "repo-1", &pinned, &|_| Some(gh.clone()), WAIT).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    let item = items.into_iter().next().unwrap();
    Planned { verdict: item.verdict.clone(), repo, worktree, item }
}

#[test]
fn a_finished_run_whose_review_is_merged_and_fully_pushed_is_reaped_worktree_and_branch() {
    for (tag, state) in [("merged", "MERGED"), ("closed", "CLOSED")] {
        let planned = scenario(tag, Case { state, ..GOOD });
        assert_eq!(planned.verdict, ReapVerdict::Reap, "{state}");
        reap_run(&planned.repo, &planned.item).unwrap();
        assert!(!planned.worktree.exists(), "the worktree is gone");
        let branches = git(&planned.repo, &["branch", "--list", "fix--COM-02"]);
        assert!(branches.is_empty(), "the branch is gone: {branches}");
    }
}

#[test]
fn reaping_a_run_also_removes_the_private_ref_a_crashed_rebase_left_behind() {
    let planned = scenario("leftover-ref", GOOD);
    let private = orq_orchestrator::private_ref_name(&planned.item.run_id);
    let other = orq_orchestrator::private_ref_name("some-other-run");
    for name in [&private, &other] {
        git(&planned.repo, &["update-ref", name, "HEAD"]);
    }

    reap_run(&planned.repo, &planned.item).unwrap();

    let left = git(&planned.repo, &["for-each-ref", "--format=%(refname)", "refs/orq"]);
    assert_eq!(left, other, "only the reaped run's own ref goes");
}

#[test]
fn a_private_ref_that_cannot_be_deleted_is_reported_as_an_error() {
    let planned = scenario("stuck-ref", GOOD);
    let private = orq_orchestrator::private_ref_name(&planned.item.run_id);
    git(&planned.repo, &["update-ref", &private, "HEAD"]);
    // A held lock makes git refuse the deletion.
    let lock = planned.repo.join(".git").join(format!("{private}.lock"));
    fs::write(&lock, "").unwrap();

    let err = reap_run(&planned.repo, &planned.item).expect_err("a ref left behind must not pass silently");

    assert!(err.to_string().contains("private ref"), "{err}");
    // Nothing else was removed, so the run is still planned and the reap can be retried.
    assert!(planned.worktree.exists(), "the worktree stays");
    assert!(!git(&planned.repo, &["branch", "--list", "fix--COM-02"]).is_empty(), "the branch stays");
}

#[test]
fn anything_not_provably_finished_and_safe_is_kept_with_its_reason() {
    let cases = [
        ("open", Case { state: "OPEN", ..GOOD }, "not merged or closed"),
        ("pinned", Case { pinned: true, ..GOOD }, "pinned"),
        ("running", Case { finished: false, ..GOOD }, "still running"),
        ("dirty", Case { dirty: true, ..GOOD }, "uncommitted"),
        ("unpushed", Case { pushed: false, ..GOOD }, "not pushed"),
        ("outside", Case { inside_worktrees_dir: false, ..GOOD }, "not under"),
        ("detached", Case { detached_ahead: true, ..GOOD }, "not on the branch"),
        ("providerdown", Case { provider_fails: true, ..GOOD }, "could not be checked"),
    ];
    for (tag, case, why) in cases {
        let planned = scenario(tag, case);
        match &planned.verdict {
            ReapVerdict::Keep(reason) => assert!(reason.contains(why), "{tag}: {reason}"),
            ReapVerdict::Reap => panic!("{tag} must be kept"),
        }
        assert!(planned.worktree.exists(), "{tag}: nothing was touched");
    }
}

#[test]
fn a_plan_gone_stale_removes_nothing() {
    // Work that lands after the plan was made: an ignored scratch file, an uncommitted edit, a new commit.
    let changes: [(&str, fn(&Planned)); 3] = [
        ("untracked", |p| fs::write(p.worktree.join("late.txt"), "x\n").unwrap()),
        ("commit", |p| {
            fs::write(p.worktree.join("late.txt"), "x\n").unwrap();
            git(&p.worktree, &["add", "."]);
            git(&p.worktree, &["commit", "-q", "-m", "late"]);
        }),
        // The ignore rule exists from the start (info/exclude), so only an ignored file appears later.
        ("ignored", |p| fs::write(p.worktree.join("keep.log"), "x\n").unwrap()),
    ];
    for (tag, change) in changes {
        let planned = scenario(&format!("stale-{tag}"), GOOD);
        assert_eq!(planned.verdict, ReapVerdict::Reap);
        change(&planned);
        let refused = reap_run(&planned.repo, &planned.item).expect_err("a stale plan must be refused");
        if tag != "commit" {
            assert!(format!("{refused:?}").contains("uncommitted"), "{tag}: {refused:?}");
        }
        assert!(planned.worktree.exists(), "{tag}: the worktree stays");
        assert!(!git(&planned.repo, &["branch", "--list", "fix--COM-02"]).is_empty(), "{tag}: the branch stays");
    }
}
