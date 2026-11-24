//! `orq run resume <run-id> --design-issues <file.json>`: a person's issues for a frontend run whose design
//! review step ended needing review. Anything else, and a file that is empty or invalid, is refused with a
//! readable reason before anything starts: no event is recorded and no run is made.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::process::Output;

use orq_orchestrator::{
    record_run_completed, run_design_loop, CorrectionOutcome, CorrectionPart, DesignLoopSteps,
    DesignReview, DesignReviewOutcome, DesignVerdict, Event, IssueStatus, ReviewIssue, RunRecord,
    RunStore, Severity, StoreError,
};
use orq_test_support::GitFixture;

struct World {
    repo: GitFixture,
    base: PathBuf,
    store: RunStore,
    record: RunRecord,
}

fn world(tag: &str) -> World {
    let repo = GitFixture::new();
    let base = orq_test_support::process_temp_dir().join(format!(
        "orq-resume-design-issues-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("config/orq")).unwrap();
    std::fs::create_dir_all(base.join("state")).unwrap();
    let store = RunStore::new(&base.join("state/orq"));
    let record = RunRecord {
        delivery: Default::default(),
        run_id: "run-1".to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "page".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0".repeat(40),
        branch: "fix--page".to_string(),
        worktree: base.join("worktree"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    World {
        repo,
        base,
        store,
        record,
    }
}

fn orq(w: &World, args: &[&str]) -> Output {
    orq_test_support::sandboxed_command(env!("CARGO_BIN_EXE_orq"))
        .args(args)
        .env("XDG_CONFIG_HOME", w.base.join("config"))
        .env("XDG_STATE_HOME", w.base.join("state"))
        .env_remove("HERDR_ACTIVE_PANE_CWD")
        .env_remove("HERDR_SOCKET_PATH")
        .output()
        .unwrap()
}

fn resume_with(w: &World, text: &str) -> Output {
    let file = w.base.join("issues.json");
    std::fs::write(&file, text).unwrap();
    orq(
        w,
        &[
            "run",
            "resume",
            "run-1",
            "--yes",
            "--design-issues",
            file.to_str().unwrap(),
            "--repo",
            w.repo.root().to_str().unwrap(),
        ],
    )
}

fn issue() -> ReviewIssue {
    ReviewIssue {
        id: "hero-size".to_string(),
        section: "hero".to_string(),
        viewport: "desktop".to_string(),
        source: "references/desktop.png".to_string(),
        expected: "a 48px heading".to_string(),
        actual: "a 32px heading".to_string(),
        evidence: vec![],
        severity: Severity::Major,
        status: IssueStatus::Open,
        fix_direction: "raise the heading size".to_string(),
        selector: None,
        confidence: None,
        basis: None,
        copy: None,
        quote: None,
        token: None,
        reference: None,
        downgraded_from: None,
        needs: None,
    }
}

fn changes() -> DesignReviewOutcome {
    DesignReviewOutcome {
        verdict: DesignVerdict::ChangesRequired,
        reason: None,
        review: Some(DesignReview {
            run_id: "run-1".to_string(),
            reference_hash: "abc".to_string(),
            capture_revision: "rev".to_string(),
            evaluation: "compared".to_string(),
            verdict: DesignVerdict::ChangesRequired,
            checks: Vec::new(),
            issues: vec![issue()],
            suggestions: Vec::new(),
            blockers: Vec::new(),
        }),
        report: PathBuf::from("/logs/run-1/design/report.md"),
    }
}

struct Fake(VecDeque<DesignReviewOutcome>);

impl DesignLoopSteps for Fake {
    fn head(&mut self) -> Result<String, StoreError> {
        Ok("head-1".to_string())
    }
    fn review(&mut self, _round: u32) -> Result<DesignReviewOutcome, StoreError> {
        self.0
            .pop_front()
            .ok_or_else(|| StoreError::Corrupt("interrupted".to_string()))
    }
    fn correct(
        &mut self,
        _: u32,
        _: &[ReviewIssue],
        _: &[String],
    ) -> Result<CorrectionOutcome, StoreError> {
        Ok(CorrectionOutcome::Done)
    }
    fn correct_part(
        &mut self,
        _: u32,
        _: &CorrectionPart,
    ) -> Result<CorrectionOutcome, StoreError> {
        Ok(CorrectionOutcome::Done)
    }
    fn review_code(&mut self, _: u32, _: &str) -> Result<CorrectionOutcome, StoreError> {
        Ok(CorrectionOutcome::Done)
    }
}

/// The run's design review step used its one correction round and the run ended needing review.
fn ended_needing_review(w: &World) {
    let mut steps = Fake(vec![changes(), changes()].into());
    run_design_loop(&w.store, "run-1", 1, &mut steps).unwrap();
    record_run_completed(
        &w.store,
        &w.record,
        "needs_review",
        Some("still requires changes"),
    )
    .unwrap();
}

fn stage_event(w: &World, kind: &str, stage: &str) {
    let n = w.store.events("run-1").unwrap().len();
    w.store
        .append_event(&Event {
            schema_version: 1,
            event_id: format!("evt-test-{n}"),
            run_id: "run-1".to_string(),
            project_id: "repo-1".to_string(),
            feature_id: "page".to_string(),
            kind: kind.to_string(),
            stage: Some(stage.to_string()),
            attempt: 1,
            occurred_at: "2026-11-25T00:00:00Z".to_string(),
            payload: serde_json::json!({}),
        })
        .unwrap();
}

/// Nothing started: no person's issues recorded and no run made.
fn nothing_started(w: &World) {
    let events = w.store.events("run-1").unwrap();
    assert!(
        !events.iter().any(|e| e.kind == "design_person_issues"),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| e.kind == "run.resumed"),
        "{events:?}"
    );
    assert_eq!(w.store.run_ids().unwrap(), ["run-1"]);
}

fn refused(output: &Output, reason: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_ne!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("cannot resume run-1"), "{stderr}");
    assert!(stderr.contains(reason), "{stderr}");
}

const ISSUES: &str = r#"[{"section": "Footer", "viewport": "all", "expected": "a transparent logo",
  "actual": "the logo sits on a white box"}]"#;

fn cleanup(w: &World) {
    let _ = std::fs::remove_dir_all(&w.base);
}

#[test]
fn a_run_that_is_not_a_design_run_is_refused_and_nothing_starts() {
    let w = world("not-design");
    stage_event(&w, "stage.started", "implement");
    record_run_completed(&w.store, &w.record, "needs_review", Some("x")).unwrap();
    let out = resume_with(&w, ISSUES);
    refused(&out, "not a frontend run with a design review step");
    nothing_started(&w);
    cleanup(&w);
}

#[test]
fn a_design_run_that_did_not_end_in_its_design_review_step_is_refused_and_nothing_starts() {
    let w = world("ended-later");
    ended_needing_review(&w);
    stage_event(&w, "stage.started", "gate");
    stage_event(&w, "stage.failed", "gate");
    let out = resume_with(&w, ISSUES);
    refused(&out, "did not end");
    nothing_started(&w);
    cleanup(&w);
}

#[test]
fn issues_recorded_by_a_resume_that_then_did_not_start_do_not_block_a_retry_with_a_corrected_file() {
    let w = world("retry");
    ended_needing_review(&w);
    // A resume recorded the person's issues, then its run did not start.
    orq_orchestrator::record_design_person_issues(&w.store, "run-1", ISSUES, &[]).unwrap();
    let corrected = r#"[{"section": "Footer", "viewport": "all", "expected": "a transparent logo",
      "actual": "the logo sits on a white box", "fix_direction": "drop the white box"}]"#;
    let again = resume_with(&w, corrected);
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(!stderr.contains("already has issues from you"), "{stderr}");
    assert!(!stderr.contains("issues file"), "{stderr}");
    cleanup(&w);
}

#[test]
fn issues_given_again_before_their_round_ran_replace_the_first_and_grant_one_extra_round() {
    let w = world("given-again");
    ended_needing_review(&w);
    orq_orchestrator::record_design_person_issues(&w.store, "run-1", ISSUES, &[]).unwrap();
    let corrected = r#"[{"section": "Footer", "viewport": "all", "expected": "a transparent logo",
      "actual": "the logo sits on a white box", "fix_direction": "drop the white box"}]"#;
    let issues = orq_orchestrator::record_design_person_issues(&w.store, "run-1", corrected, &[]).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].id, "person-1");
    let mut steps = Fake(vec![changes(), changes()].into());
    run_design_loop(&w.store, "run-1", 1, &mut steps).unwrap();
    let events = w.store.events("run-1").unwrap();
    let extra: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind == "design_correction" && e.payload.get("extra").is_some())
        .collect();
    assert_eq!(extra.len(), 1, "{events:?}");
    assert_eq!(extra[0].payload["extra"], 1);
    cleanup(&w);
}

#[test]
fn an_empty_or_invalid_file_is_refused_and_nothing_starts() {
    let w = world("invalid");
    ended_needing_review(&w);
    refused(&resume_with(&w, "[]"), "lists no issue");
    refused(
        &resume_with(
            &w,
            r#"[{"section": "Footer", "viewport": "all", "expected": "", "actual": "b"}]"#,
        ),
        "expected",
    );
    refused(
        &resume_with(
            &w,
            r#"[{"section": "Footer", "viewport": "tablet", "expected": "a", "actual": "b"}]"#,
        ),
        "viewport",
    );
    let missing = orq(
        &w,
        &[
            "run",
            "resume",
            "run-1",
            "--yes",
            "--design-issues",
            "/nonexistent/issues.json",
            "--repo",
            w.repo.root().to_str().unwrap(),
        ],
    );
    refused(&missing, "issues.json");
    nothing_started(&w);
    cleanup(&w);
}
