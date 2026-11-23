//! A frontend run's design review step: capture and review, at most `max_correction_rounds`
//! consolidated correction batches and one more review to verify, with the round counter kept in the
//! run's events so resume, rebase, gate and CI never reset it. Fake agents and capture.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;

use orq_orchestrator::{
    run_design_loop, CorrectionOutcome, DesignLoopOutcome, DesignLoopSteps, DesignReview,
    DesignReviewOutcome, DesignVerdict, IssueStatus, ReviewIssue, RunRecord, RunStore, Severity,
    StoreError,
};

struct World {
    store: RunStore,
    root: PathBuf,
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-design-loop-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let store = RunStore::new(&root.join("state"));
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "page".to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--page".to_string(),
            worktree: root.join("worktree"),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    World { store, root }
}

fn issue(id: &str, status: IssueStatus) -> ReviewIssue {
    ReviewIssue {
        id: id.to_string(),
        section: "hero".to_string(),
        viewport: "desktop".to_string(),
        source: "references/desktop.png".to_string(),
        expected: "a 48px heading".to_string(),
        actual: "a 32px heading".to_string(),
        evidence: vec!["round-1/desktop/index.png".to_string()],
        severity: Severity::Major,
        status,
        fix_direction: "raise the heading size".to_string(),
        selector: None,
        confidence: None,
        basis: None,
        copy: None,
        quote: None,
        token: None,
        reference: None,
        downgraded_from: None,
    }
}

fn outcome(verdict: DesignVerdict, issues: Vec<ReviewIssue>) -> DesignReviewOutcome {
    DesignReviewOutcome {
        verdict,
        reason: None,
        review: Some(DesignReview {
            run_id: "run-1".to_string(),
            reference_hash: "abc".to_string(),
            capture_revision: "rev".to_string(),
            evaluation: "compared".to_string(),
            verdict,
            checks: Vec::new(),
            issues,
            suggestions: Vec::new(),
            blockers: Vec::new(),
        }),
        report: PathBuf::from("/logs/run-1/design/report.md"),
    }
}

fn pass() -> DesignReviewOutcome {
    outcome(DesignVerdict::Pass, Vec::new())
}

fn changes() -> DesignReviewOutcome {
    outcome(
        DesignVerdict::ChangesRequired,
        vec![issue("hero-size", IssueStatus::Open)],
    )
}

/// The fake agents: what each review answers, in order, and what each correction does to the head.
struct Fake {
    head: String,
    reviews: VecDeque<DesignReviewOutcome>,
    /// The rounds reviewed, in order.
    reviewed: Vec<u32>,
    /// The batches given to the implementer, with the issue ids in each.
    batches: Vec<(u32, Vec<String>)>,
    /// The code reviews of a batch's fix commits: the batch and the head its fix commits start from.
    code_reviews: Vec<(u32, String)>,
    /// Stop the run (as an interruption) right after this batch, before its code review.
    interrupt_after_batch: Option<u32>,
}

impl Fake {
    fn new(head: &str, reviews: Vec<DesignReviewOutcome>) -> Fake {
        Fake {
            head: head.to_string(),
            reviews: reviews.into(),
            reviewed: Vec::new(),
            batches: Vec::new(),
            code_reviews: Vec::new(),
            interrupt_after_batch: None,
        }
    }
}

impl DesignLoopSteps for Fake {
    fn head(&mut self) -> Result<String, StoreError> {
        Ok(self.head.clone())
    }

    fn review(&mut self, round: u32) -> Result<DesignReviewOutcome, StoreError> {
        self.reviewed.push(round);
        // Past the scripted reviews the process is interrupted.
        self.reviews
            .pop_front()
            .ok_or_else(|| StoreError::Corrupt("interrupted".to_string()))
    }

    fn correct(
        &mut self,
        batch: u32,
        issues: &[ReviewIssue],
        _suggestions: &[String],
    ) -> Result<CorrectionOutcome, StoreError> {
        self.batches
            .push((batch, issues.iter().map(|i| i.id.clone()).collect()));
        self.head = format!("fix-{batch}");
        Ok(CorrectionOutcome::Done)
    }

    fn review_code(&mut self, batch: u32, from: &str) -> Result<CorrectionOutcome, StoreError> {
        if self.interrupt_after_batch.take() == Some(batch) {
            return Err(StoreError::Corrupt("interrupted".to_string()));
        }
        self.code_reviews.push((batch, from.to_string()));
        Ok(CorrectionOutcome::Done)
    }
}

#[test]
fn each_correction_batch_has_its_fix_commits_code_reviewed_again_once_even_across_a_resume() {
    let w = world("review-again");
    // Batch 1's fix commits are made, and the run is interrupted before their code review.
    let mut before = Fake::new("head-1", vec![changes()]);
    before.interrupt_after_batch = Some(1);
    assert!(run_design_loop(&w.store, "run-1", 3, &mut before).is_err());
    assert_eq!(before.batches.len(), 1);
    assert!(before.code_reviews.is_empty());

    // The resumed run reviews batch 1's fix commits (from the head before it) without giving the batch again.
    let mut after = Fake::new("fix-1", vec![changes(), pass()]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut after).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Passed {
            head: "fix-2".to_string()
        }
    );
    assert_eq!(after.batches.iter().map(|b| b.0).collect::<Vec<_>>(), [2]);
    assert_eq!(
        after.code_reviews,
        [(1, "head-1".to_string()), (2, "fix-1".to_string())]
    );
    assert_eq!(after.reviewed, [2, 3]);
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_code_review_of_a_batch_that_stops_the_run_stops_the_design_step() {
    struct Stops(Fake);
    impl DesignLoopSteps for Stops {
        fn head(&mut self) -> Result<String, StoreError> {
            self.0.head()
        }
        fn review(&mut self, round: u32) -> Result<DesignReviewOutcome, StoreError> {
            self.0.review(round)
        }
        fn correct(
            &mut self,
            batch: u32,
            issues: &[ReviewIssue],
            suggestions: &[String],
        ) -> Result<CorrectionOutcome, StoreError> {
            self.0.correct(batch, issues, suggestions)
        }
        fn review_code(
            &mut self,
            _batch: u32,
            _from: &str,
        ) -> Result<CorrectionOutcome, StoreError> {
            Ok(CorrectionOutcome::Stopped(
                "the code review escalated".to_string(),
            ))
        }
    }
    let w = world("review-again-stops");
    let mut steps = Stops(Fake::new("head-1", vec![changes(), pass()]));
    let got = run_design_loop(&w.store, "run-1", 3, &mut steps).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Stopped {
            reason: "the code review escalated".to_string()
        }
    );
    assert_eq!(
        steps.0.reviewed,
        [1],
        "the page is not reviewed again before its fix commits pass code review"
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_page_that_passes_the_first_review_gets_no_correction() {
    let w = world("pass-first");
    let mut fake = Fake::new("head-1", vec![pass()]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Passed {
            head: "head-1".to_string()
        }
    );
    assert_eq!(fake.reviewed, [1]);
    assert!(fake.batches.is_empty());
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_page_that_passes_after_one_batch_is_reviewed_twice_and_only_the_open_issues_go_in_the_batch() {
    let w = world("pass-after-one");
    let first = outcome(
        DesignVerdict::ChangesRequired,
        vec![
            issue("hero-size", IssueStatus::Open),
            issue("logo", IssueStatus::Resolved),
            issue("footer", IssueStatus::Regressed),
        ],
    );
    let mut fake = Fake::new("head-1", vec![first, pass()]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Passed {
            head: "fix-1".to_string()
        }
    );
    assert_eq!(fake.reviewed, [1, 2]);
    assert_eq!(
        fake.batches,
        [(1, vec!["hero-size".to_string(), "footer".to_string()])]
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_page_failing_through_every_batch_gets_three_batches_four_reviews_and_pauses_with_the_report() {
    let w = world("fail-through");
    let mut fake = Fake::new("head-1", vec![changes(), changes(), changes(), changes()]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    match got {
        DesignLoopOutcome::NeedsReview { reason, report } => {
            assert!(reason.contains("3 correction rounds"), "{reason}");
            assert_eq!(report, Some(PathBuf::from("/logs/run-1/design/report.md")));
        }
        other => panic!("expected a pause, got {other:?}"),
    }
    assert_eq!(fake.reviewed, [1, 2, 3, 4]);
    assert_eq!(
        fake.batches.iter().map(|b| b.0).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_run_resumed_mid_round_keeps_its_round_count_and_does_not_repeat_a_finished_batch() {
    let w = world("resume");
    // Review 1 asks for changes, batch 1 finishes, and the run is interrupted before review 2.
    let mut before = Fake::new("head-1", vec![changes()]);
    assert!(run_design_loop(&w.store, "run-1", 3, &mut before).is_err());
    assert_eq!(before.batches.len(), 1);

    let mut after = Fake::new("fix-1", vec![changes(), changes(), changes()]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut after).unwrap();
    assert!(
        matches!(got, DesignLoopOutcome::NeedsReview { .. }),
        "{got:?}"
    );
    assert_eq!(
        after.reviewed,
        [2, 3, 4],
        "the resumed run goes on from round 2"
    );
    assert_eq!(
        after.batches.iter().map(|b| b.0).collect::<Vec<_>>(),
        [2, 3],
        "batch 1 is not given again and the budget is not reset"
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_gate_fix_after_the_design_pass_forces_one_verification_review_and_failing_it_pauses_without_a_batch(
) {
    let w = world("gate-fix");
    let mut first = Fake::new("head-1", vec![pass()]);
    assert_eq!(
        run_design_loop(&w.store, "run-1", 3, &mut first).unwrap(),
        DesignLoopOutcome::Passed {
            head: "head-1".to_string()
        }
    );

    // Nothing changed since: the pass holds, nothing is captured again.
    let mut same = Fake::new("head-1", vec![]);
    assert_eq!(
        run_design_loop(&w.store, "run-1", 3, &mut same).unwrap(),
        DesignLoopOutcome::Passed {
            head: "head-1".to_string()
        }
    );
    assert!(same.reviewed.is_empty());

    // A gate fix moved the head: one verification review, with no new correction budget.
    let mut fixed = Fake::new("gate-fix", vec![changes()]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut fixed).unwrap();
    match got {
        DesignLoopOutcome::NeedsReview { reason, .. } => {
            assert!(reason.contains("verification"), "{reason}")
        }
        other => panic!("expected a pause, got {other:?}"),
    }
    assert_eq!(fixed.reviewed, [2]);
    assert!(
        fixed.batches.is_empty(),
        "a verification review gets no correction batch"
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn the_events_show_the_design_review_step_with_its_round_for_the_run_page() {
    let w = world("events");
    let mut fake = Fake::new("head-1", vec![changes(), pass()]);
    run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    let events = w.store.events("run-1").unwrap();
    let started: Vec<(u32, serde_json::Value)> = events
        .iter()
        .filter(|e| e.kind == "stage.started" && e.stage.as_deref() == Some("design_review"))
        .map(|e| (e.attempt, e.payload.clone()))
        .collect();
    assert_eq!(
        started,
        [
            (1, serde_json::json!({"round": 1, "of": 4})),
            (2, serde_json::json!({"round": 2, "of": 4}))
        ],
        "each review starts the step again with its round, of one per batch and one to verify"
    );
    let last = events.last().unwrap();
    assert_eq!(
        (last.kind.as_str(), last.stage.as_deref()),
        ("stage.completed", Some("design_review"))
    );
    let activity =
        orq_orchestrator::run_activity(&events, None, std::time::Duration::from_secs(60));
    assert_eq!(activity.attempt, 2);
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn the_run_page_names_each_design_review_event_with_its_round_of_the_budget() {
    let w = world("round-label");
    let mut fake = Fake::new("head-1", vec![changes(), pass()]);
    run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    let events = w.store.events("run-1").unwrap();
    let labels: Vec<Option<String>> = events
        .iter()
        .filter(|e| e.stage.as_deref() == Some("design_review"))
        .map(orq_orchestrator::design_round_label)
        .collect();
    assert!(!labels.is_empty());
    assert!(
        labels.iter().all(Option::is_some),
        "every event of the step names its round: {labels:?}"
    );
    assert_eq!(
        labels.first().unwrap().as_deref(),
        Some("design_review round 1 of 4")
    );
    assert_eq!(
        labels.last().unwrap().as_deref(),
        Some("design_review round 2 of 4")
    );
    let other = events
        .iter()
        .find(|e| e.stage.as_deref() != Some("design_review"));
    assert!(other.map_or(true, |e| orq_orchestrator::design_round_label(e).is_none()));
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_verification_review_that_passes_lets_delivery_go_on() {
    let w = world("verify-pass");
    let mut first = Fake::new("head-1", vec![pass()]);
    run_design_loop(&w.store, "run-1", 3, &mut first).unwrap();
    let mut rebased = Fake::new("rebased", vec![pass()]);
    assert_eq!(
        run_design_loop(&w.store, "run-1", 3, &mut rebased).unwrap(),
        DesignLoopOutcome::Passed {
            head: "rebased".to_string()
        }
    );
    assert_eq!(rebased.reviewed, [2]);
    let _ = fs::remove_dir_all(&w.root);
}

/// run-1 froze its package and was reviewed once (manifest and the round's review in its logs); run-2
/// resumes it.
fn frozen_and_resumed(w: &World) -> (RunRecord, PathBuf, PathBuf) {
    orq_orchestrator::record_design_package(
        &w.store,
        "run-1",
        "stage.completed",
        serde_json::json!({}),
    )
    .unwrap();
    run_design_loop(&w.store, "run-1", 3, &mut Fake::new("head-1", vec![pass()])).unwrap();
    let from_logs = w.root.join("logs/run-1");
    fs::create_dir_all(from_logs.join("design/round-1")).unwrap();
    fs::write(from_logs.join("design/manifest.json"), "{\"frozen\":true}").unwrap();
    fs::write(from_logs.join("design/round-1/review.json"), "{}").unwrap();
    let mut resumed = w.store.load("run-1").unwrap();
    resumed.run_id = "run-2".to_string();
    w.store.create(&resumed).unwrap();
    (resumed, from_logs, w.root.join("logs/run-2"))
}

#[test]
fn a_carry_whose_copy_fails_leaves_the_half_copied_folder_and_a_retry_over_its_leftover_installs_the_package(
) {
    use std::os::unix::fs::PermissionsExt;
    let w = world("carry-copy-fails");
    let (resumed, from_logs, to_logs) = frozen_and_resumed(&w);
    fs::create_dir_all(to_logs.join("design/round-1")).unwrap();
    fs::write(to_logs.join("design/round-1/note.txt"), "kept").unwrap();
    let review = from_logs.join("design/round-1/review.json");
    fs::set_permissions(&review, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&review).is_ok() {
        // Running with permissions that read anything: the copy cannot be made to fail this way.
        let _ = fs::remove_dir_all(&w.root);
        return;
    }

    let failed = orq_orchestrator::carry_design(&w.store, &resumed, "run-1", &from_logs, &to_logs);

    assert!(failed.is_err(), "a copy that fails is refused: {failed:?}");
    assert_eq!(
        fs::read_to_string(to_logs.join("design/round-1/note.txt")).unwrap(),
        "kept",
        "the destination is untouched"
    );
    assert!(!to_logs.join("design/manifest.json").exists());
    assert!(
        to_logs.join(".design.carrying").exists(),
        "the failed copy is left aside, never in place"
    );

    fs::set_permissions(&review, fs::Permissions::from_mode(0o644)).unwrap();
    orq_orchestrator::carry_design(&w.store, &resumed, "run-1", &from_logs, &to_logs).unwrap();

    assert_eq!(
        fs::read_to_string(to_logs.join("design/manifest.json")).unwrap(),
        "{\"frozen\":true}"
    );
    assert_eq!(
        fs::read_to_string(to_logs.join("design/round-1/review.json")).unwrap(),
        "{}"
    );
    assert!(
        !to_logs.join(".design.carrying").exists(),
        "the leftover copy is gone"
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_design_folder_already_carried_whole_is_kept_on_every_later_carry() {
    let w = world("carry-repeated");
    let (resumed, from_logs, to_logs) = frozen_and_resumed(&w);
    orq_orchestrator::carry_design(&w.store, &resumed, "run-1", &from_logs, &to_logs).unwrap();
    // The resumed run writes its own round into its design folder.
    fs::create_dir_all(to_logs.join("design/round-2")).unwrap();
    fs::write(to_logs.join("design/round-2/review.json"), "{\"round\":2}").unwrap();

    orq_orchestrator::carry_design(&w.store, &resumed, "run-1", &from_logs, &to_logs).unwrap();
    orq_orchestrator::carry_design(&w.store, &resumed, "run-1", &from_logs, &to_logs).unwrap();

    assert_eq!(
        fs::read_to_string(to_logs.join("design/round-2/review.json")).unwrap(),
        "{\"round\":2}"
    );
    assert_eq!(
        fs::read_to_string(to_logs.join("design/manifest.json")).unwrap(),
        "{\"frozen\":true}"
    );
    let reviews = |run: &str| {
        w.store
            .events(run)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "design_review")
            .count()
    };
    assert_eq!(reviews("run-1"), 1);
    assert_eq!(
        reviews("run-2"),
        1,
        "run-1's one review is carried once, never again"
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_resume_into_a_half_copied_design_folder_still_gets_the_frozen_manifest() {
    let w = world("carry-half-copied");
    let (resumed, from_logs, to_logs) = frozen_and_resumed(&w);
    // An earlier carry into run-2 was interrupted: its design folder is there, its manifest is not.
    fs::create_dir_all(to_logs.join("design/round-1")).unwrap();

    orq_orchestrator::carry_design(&w.store, &resumed, "run-1", &from_logs, &to_logs).unwrap();

    assert_eq!(
        fs::read_to_string(to_logs.join("design/manifest.json")).unwrap(),
        "{\"frozen\":true}"
    );
    assert_eq!(
        fs::read_to_string(to_logs.join("design/round-1/review.json")).unwrap(),
        "{}"
    );
    let _ = fs::remove_dir_all(&w.root);
}
