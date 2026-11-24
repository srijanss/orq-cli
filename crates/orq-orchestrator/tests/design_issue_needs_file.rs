//! A design review issue that needs a file the package does not have (artwork, copy, a document the package
//! says is supplied separately) goes to no fix session: the run pauses as `design_files_needed` with the
//! file listed under `designs/<feature>/`, and on resume the package is frozen anew and the page reviewed
//! again. Fake design steps, like design_loop.rs.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;

use orq_orchestrator::{
    drop_refused_needs, run_design_loop, CorrectionOutcome, DesignFilesNeeded, DesignLoopOutcome,
    DesignLoopSteps,
    DesignReview, DesignReviewOutcome, DesignVerdict, IssueNeeds, IssueStatus, NeededFile,
    Refreeze, ReviewIssue, RunRecord, RunStore, Severity, StoreError,
};

struct World {
    store: RunStore,
    root: PathBuf,
}

fn record(run_id: &str, root: &std::path::Path) -> RunRecord {
    RunRecord {
        delivery: Default::default(),
        run_id: run_id.to_string(),
        project_id: "repo-1".to_string(),
        feature_id: "landing-page".to_string(),
        remote: "origin".to_string(),
        provider: "github".to_string(),
        target_branch: "main".to_string(),
        base_sha: "0".repeat(40),
        branch: "fix--landing-page".to_string(),
        worktree: root.join("worktree"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    }
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-design-needs-file-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let store = RunStore::new(&root.join("state"));
    store.create(&record("run-1", &root)).unwrap();
    World { store, root }
}

fn issue(id: &str, needs: Option<&str>) -> ReviewIssue {
    ReviewIssue {
        id: id.to_string(),
        section: "Testimonials".to_string(),
        viewport: "desktop".to_string(),
        source: "references/desktop.png".to_string(),
        expected: "a signature under each testimonial".to_string(),
        actual: "no signature".to_string(),
        evidence: Vec::new(),
        severity: Severity::Major,
        status: IssueStatus::Open,
        fix_direction: "add the signature".to_string(),
        selector: None,
        confidence: None,
        basis: None,
        copy: None,
        quote: None,
        token: None,
        reference: None,
        downgraded_from: None,
        needs: needs.map(|path| IssueNeeds {
            what: "the approved signature artwork".to_string(),
            path: path.to_string(),
        }),
    }
}

fn reviewed(verdict: DesignVerdict, issues: Vec<ReviewIssue>) -> DesignReviewOutcome {
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

fn changes(issues: Vec<ReviewIssue>) -> DesignReviewOutcome {
    reviewed(DesignVerdict::ChangesRequired, issues)
}

/// The fake steps: what each review answers, in order; the batches given; the package frozen anew.
struct Fake {
    head: String,
    reviews: VecDeque<DesignReviewOutcome>,
    /// Each step taken, in order: `review 2`, `batch 1`, `refreeze`.
    log: Vec<String>,
    /// The batches given to the implementer, with the issue ids in each.
    batches: Vec<(u32, Vec<String>)>,
}

impl Fake {
    fn new(reviews: Vec<DesignReviewOutcome>) -> Fake {
        Fake {
            head: "head-1".to_string(),
            reviews: reviews.into(),
            log: Vec::new(),
            batches: Vec::new(),
        }
    }
}

impl DesignLoopSteps for Fake {
    fn head(&mut self) -> Result<String, StoreError> {
        Ok(self.head.clone())
    }

    fn review(&mut self, round: u32) -> Result<DesignReviewOutcome, StoreError> {
        self.log.push(format!("review {round}"));
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
        self.log.push(format!("batch {batch}"));
        self.batches
            .push((batch, issues.iter().map(|i| i.id.clone()).collect()));
        self.head = format!("fix-{batch}");
        Ok(CorrectionOutcome::Done)
    }

    fn review_code(&mut self, _batch: u32, _from: &str) -> Result<CorrectionOutcome, StoreError> {
        Ok(CorrectionOutcome::Done)
    }

    fn refreeze(&mut self, requested: &DesignFilesNeeded) -> Result<Refreeze, StoreError> {
        self.log.push(refreezing(requested));
        Ok(Refreeze::Frozen)
    }
}

/// The log line of a new freeze: the files the review asked for, which it is to check.
fn refreezing(requested: &DesignFilesNeeded) -> String {
    let paths: Vec<&str> = requested.files.iter().map(|f| f.path.as_str()).collect();
    format!("refreeze {} {}", requested.feature, paths.join(" "))
}

fn signature_file() -> NeededFile {
    NeededFile {
        what: "the approved signature artwork".to_string(),
        path: "designs/landing-page/assets/signatures.svg".to_string(),
    }
}

#[test]
fn a_review_whose_only_open_issue_needs_a_file_runs_no_batch_and_pauses_for_that_file() {
    let w = world("only-needs");
    let mut fake = Fake::new(vec![changes(vec![issue(
        "testimonials-signatures",
        Some("assets/signatures.svg"),
    )])]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    let DesignLoopOutcome::FilesNeeded {
        reason,
        request,
        open,
        ..
    } = got
    else {
        panic!("expected a files needed pause, got {got:?}");
    };
    assert_eq!(request.feature, "landing-page");
    assert_eq!(request.files, [signature_file()]);
    assert!(open.is_empty(), "{open:?}");
    assert!(
        reason.starts_with("the design needs files the package does not have: "),
        "{reason}"
    );
    assert!(reason.contains("designs/landing-page/assets/signatures.svg"), "{reason}");
    assert!(fake.batches.is_empty(), "no fix session: {:?}", fake.batches);
    assert_eq!(fake.log, ["review 1"]);
    // The run page's panel reads the request from the run's events.
    let events = w.store.events("run-1").unwrap();
    let asked = events
        .iter()
        .rev()
        .find(|e| e.kind == "design.files_needed")
        .expect("a design.files_needed event");
    assert_eq!(
        asked.payload["files"],
        serde_json::json!([{"what": "the approved signature artwork", "path": "designs/landing-page/assets/signatures.svg"}])
    );
    assert_eq!(asked.payload["feature"], "landing-page");
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_fixable_issue_goes_to_the_batch_without_the_needs_one_and_a_spent_budget_pauses_for_the_file() {
    let w = world("mixed");
    let both = || {
        changes(vec![
            issue("hero-spacing", None),
            issue("testimonials-signatures", Some("assets/signatures.svg")),
        ])
    };
    let mut fake = Fake::new(vec![both(), both(), both()]);
    let got = run_design_loop(&w.store, "run-1", 2, &mut fake).unwrap();
    assert_eq!(
        fake.batches,
        [
            (1, vec!["hero-spacing".to_string()]),
            (2, vec!["hero-spacing".to_string()])
        ],
        "only the fixable issue is given to each batch"
    );
    let DesignLoopOutcome::FilesNeeded {
        reason,
        request,
        open,
        report,
    } = got
    else {
        panic!("expected a files needed pause, got {got:?}");
    };
    assert_eq!(request.files, [signature_file()]);
    let open: Vec<&str> = open.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(open, ["hero-spacing"]);
    assert!(
        reason.starts_with("the design needs files the package does not have: "),
        "{reason}"
    );
    assert!(reason.contains("hero-spacing"), "the other open issue is named: {reason}");
    assert!(!reason.contains("correction rounds"), "{reason}");
    assert_eq!(report, Some(PathBuf::from("/logs/run-1/design/report.md")));
    let _ = fs::remove_dir_all(&w.root);
}

/// Pauses run-1 for the signature file after one batch, then resumes it as run-2.
fn paused_then_resumed(w: &World) {
    let mut first = Fake::new(vec![
        changes(vec![
            issue("hero-spacing", None),
            issue("testimonials-signatures", Some("assets/signatures.svg")),
        ]),
        changes(vec![issue(
            "testimonials-signatures",
            Some("assets/signatures.svg"),
        )]),
    ]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut first).unwrap();
    assert!(
        matches!(got, DesignLoopOutcome::FilesNeeded { .. }),
        "{got:?}"
    );
    assert_eq!(first.log, ["review 1", "batch 1", "review 2"]);
    w.store.create(&record("run-2", &w.root)).unwrap();
    let (from_logs, to_logs) = (w.root.join("logs-1"), w.root.join("logs-2"));
    fs::create_dir_all(&from_logs).unwrap();
    fs::create_dir_all(&to_logs).unwrap();
    orq_orchestrator::carry_design(
        &w.store,
        &record("run-2", &w.root),
        "run-1",
        &from_logs,
        &to_logs,
    )
    .unwrap();
}

#[test]
fn resuming_after_the_file_is_added_freezes_the_package_anew_keeps_the_round_count_and_reviews_again()
{
    let w = world("resume");
    paused_then_resumed(&w);
    let mut resumed = Fake::new(vec![reviewed(DesignVerdict::Pass, Vec::new())]);
    resumed.head = "fix-1".to_string();
    let got = run_design_loop(&w.store, "run-2", 3, &mut resumed).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Passed {
            head: "fix-1".to_string()
        }
    );
    assert_eq!(
        resumed.log,
        [
            "refreeze landing-page designs/landing-page/assets/signatures.svg",
            "review 3"
        ],
        "the package is frozen anew once the file asked for is checked, then the page reviewed again as round 3"
    );
    let events = w.store.events("run-2").unwrap();
    assert!(
        events.iter().any(|e| e.kind == "design_package_refrozen"),
        "the new freeze is recorded"
    );
    let _ = fs::remove_dir_all(&w.root);
}

/// A fake whose package check on resume answers `answer`.
struct Refreezing {
    fake: Fake,
    answer: Refreeze,
}

impl DesignLoopSteps for Refreezing {
    fn head(&mut self) -> Result<String, StoreError> {
        self.fake.head()
    }
    fn review(&mut self, round: u32) -> Result<DesignReviewOutcome, StoreError> {
        self.fake.review(round)
    }
    fn correct(
        &mut self,
        batch: u32,
        issues: &[ReviewIssue],
        suggestions: &[String],
    ) -> Result<CorrectionOutcome, StoreError> {
        self.fake.correct(batch, issues, suggestions)
    }
    fn review_code(&mut self, batch: u32, from: &str) -> Result<CorrectionOutcome, StoreError> {
        self.fake.review_code(batch, from)
    }
    fn refreeze(&mut self, requested: &DesignFilesNeeded) -> Result<Refreeze, StoreError> {
        self.fake.log.push(refreezing(requested));
        Ok(self.answer.clone())
    }
}

#[test]
fn resuming_while_the_package_still_lacks_files_pauses_again_and_a_refused_package_stops_the_run() {
    let w = world("still-missing");
    paused_then_resumed(&w);
    let still = orq_orchestrator::DesignFilesNeeded {
        feature: "landing-page".to_string(),
        files: vec![signature_file()],
    };
    let mut resumed = Refreezing {
        fake: Fake::new(Vec::new()),
        answer: Refreeze::FilesNeeded(still.clone()),
    };
    let got = run_design_loop(&w.store, "run-2", 3, &mut resumed).unwrap();
    let DesignLoopOutcome::FilesNeeded { reason, request, .. } = got else {
        panic!("expected a files needed pause, got {got:?}");
    };
    assert_eq!(request, still);
    assert!(
        reason.starts_with("the design needs files the package does not have: "),
        "{reason}"
    );
    assert_eq!(
        resumed.fake.log,
        ["refreeze landing-page designs/landing-page/assets/signatures.svg"],
        "no review without the files"
    );

    let mut refused = Refreezing {
        fake: Fake::new(Vec::new()),
        answer: Refreeze::Stopped("DESIGN.md is not UTF-8".to_string()),
    };
    let got = run_design_loop(&w.store, "run-2", 3, &mut refused).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Stopped {
            reason: "DESIGN.md is not UTF-8".to_string()
        }
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_verification_review_that_needs_a_file_pauses_for_it_and_on_resume_the_page_is_verified_again()
{
    let w = world("verification");
    let mut fake = Fake::new(vec![
        reviewed(DesignVerdict::Pass, Vec::new()),
        changes(vec![issue(
            "testimonials-signatures",
            Some("assets/signatures.svg"),
        )]),
    ]);
    let got = run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    assert!(matches!(got, DesignLoopOutcome::Passed { .. }), "{got:?}");
    // A later change gets a verification review, which asks for the signature artwork.
    fake.head = "later".to_string();
    let got = run_design_loop(&w.store, "run-1", 3, &mut fake).unwrap();
    let DesignLoopOutcome::FilesNeeded {
        reason,
        request,
        open,
        ..
    } = got
    else {
        panic!("expected a files needed pause, got {got:?}");
    };
    assert_eq!(request.files, [signature_file()]);
    assert!(open.is_empty(), "{open:?}");
    assert!(
        reason.starts_with("the design needs files the package does not have: "),
        "{reason}"
    );
    assert!(fake.batches.is_empty(), "a verification review spends no round");
    assert!(w
        .store
        .events("run-1")
        .unwrap()
        .iter()
        .any(|e| e.kind == "design.files_needed"));

    // Resumed with the file added, the package is frozen anew and the page verified again.
    w.store.create(&record("run-2", &w.root)).unwrap();
    let (from_logs, to_logs) = (w.root.join("logs-1"), w.root.join("logs-2"));
    fs::create_dir_all(&from_logs).unwrap();
    fs::create_dir_all(&to_logs).unwrap();
    orq_orchestrator::carry_design(
        &w.store,
        &record("run-2", &w.root),
        "run-1",
        &from_logs,
        &to_logs,
    )
    .unwrap();
    let mut resumed = Fake::new(vec![reviewed(DesignVerdict::Pass, Vec::new())]);
    resumed.head = "later".to_string();
    let got = run_design_loop(&w.store, "run-2", 3, &mut resumed).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Passed {
            head: "later".to_string()
        }
    );
    assert_eq!(
        resumed.log,
        [
            "refreeze landing-page designs/landing-page/assets/signatures.svg",
            "review 3"
        ]
    );
    assert!(resumed.batches.is_empty());
    let _ = fs::remove_dir_all(&w.root);
}

/// A fake whose design package is the folder of feature `package`.
struct Packaged {
    fake: Fake,
    package: String,
}

impl DesignLoopSteps for Packaged {
    fn head(&mut self) -> Result<String, StoreError> {
        self.fake.head()
    }
    fn review(&mut self, round: u32) -> Result<DesignReviewOutcome, StoreError> {
        self.fake.review(round)
    }
    fn correct(
        &mut self,
        batch: u32,
        issues: &[ReviewIssue],
        suggestions: &[String],
    ) -> Result<CorrectionOutcome, StoreError> {
        self.fake.correct(batch, issues, suggestions)
    }
    fn review_code(&mut self, batch: u32, from: &str) -> Result<CorrectionOutcome, StoreError> {
        self.fake.review_code(batch, from)
    }
    fn package_feature(&mut self) -> Option<String> {
        Some(self.package.clone())
    }
}

#[test]
fn the_files_asked_for_go_in_the_folder_of_the_runs_design_package() {
    let w = world("package-folder");
    let mut steps = Packaged {
        fake: Fake::new(vec![changes(vec![issue(
            "testimonials-signatures",
            Some("assets/signatures.svg"),
        )])]),
        package: "Landing Page".to_string(),
    };
    let got = run_design_loop(&w.store, "run-1", 3, &mut steps).unwrap();
    let DesignLoopOutcome::FilesNeeded { request, .. } = got else {
        panic!("expected a files needed pause, got {got:?}");
    };
    assert_eq!(request.feature, "Landing Page");
    assert_eq!(
        request.files[0].path,
        "designs/Landing Page/assets/signatures.svg"
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_run_paused_for_design_files_may_be_accepted_as_it_stands_like_one_waiting_on_a_review() {
    let w = world("accept");
    orq_orchestrator::record_run_completed(
        &w.store,
        &record("run-1", &w.root),
        "design_files_needed",
        Some("the design needs files the package does not have: designs/landing-page/assets/signatures.svg"),
    )
    .unwrap();
    // The phase is acceptable: what is refused is only that this test run has no worktree.
    assert_eq!(
        orq_orchestrator::accept_check(&w.store, "run-1"),
        Err("the worktree of run-1 is gone; there is no work to accept".to_string())
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn the_fix_task_of_a_round_leaves_out_an_issue_that_needs_a_file() {
    let task = orq_orchestrator::design_fix_task(
        1,
        3,
        &[
            issue("hero-spacing", None),
            issue("testimonials-signatures", Some("assets/signatures.svg")),
        ],
        &[],
    );
    assert!(task.contains("hero-spacing"), "{task}");
    assert!(!task.contains("testimonials-signatures"), "{task}");
}

const SCHEMA: &str = include_str!("../../orq-agents/design_skills/design-review/review.schema.json");
const SKILL: &str = include_str!("../../orq-agents/design_skills/design-review/SKILL.md");

fn answer(needs: Option<serde_json::Value>) -> String {
    let mut issue = serde_json::json!({
        "id": "testimonials-signatures",
        "section": "Testimonials",
        "viewport": "desktop",
        "source": "references/desktop.png",
        "expected": "a signature under each testimonial",
        "actual": "no signature",
        "evidence": [],
        "severity": "major",
        "status": "open",
        "fix_direction": "add the approved signature artwork",
        "basis": "structure",
        "copy": false,
        "reference": "references/desktop.png"
    });
    if let Some(needs) = needs {
        issue["needs"] = needs;
    }
    serde_json::json!({
        "run_id": "run-1",
        "reference_hash": "abc",
        "capture_revision": "rev",
        "evaluation": "compared",
        "verdict": "changes_required",
        "checks": [],
        "issues": [issue],
        "suggestions": [],
        "blockers": []
    })
    .to_string()
}

#[test]
fn the_review_schema_accepts_an_issue_with_needs_and_one_without_and_the_skill_says_when_to_use_it() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let issue = &schema["properties"]["issues"]["items"];
    let needs = &issue["properties"]["needs"];
    assert_eq!(needs["type"], "object");
    assert_eq!(needs["additionalProperties"], false);
    assert_eq!(needs["required"], serde_json::json!(["what", "path"]));
    assert_eq!(needs["properties"]["what"]["type"], "string");
    assert_eq!(needs["properties"]["path"]["type"], "string");
    let required: Vec<&str> = issue["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(!required.contains(&"needs"), "needs is optional");

    // orq reads both answers.
    let with: DesignReview = serde_json::from_str(&answer(Some(serde_json::json!({
        "what": "the approved signature artwork",
        "path": "assets/signatures.svg"
    }))))
    .unwrap();
    let needs = with.issues[0].needs.as_ref().unwrap();
    assert_eq!(needs.what, "the approved signature artwork");
    assert_eq!(needs.path, "assets/signatures.svg");
    let without: DesignReview = serde_json::from_str(&answer(None)).unwrap();
    assert_eq!(without.issues[0].needs, None);

    // The skill tells the reviewer when to set it, and when never to.
    assert!(SKILL.contains("`needs`"), "the skill names needs");
    for words in [
        "the package does not have",
        "to be supplied",
        "must not be invented",
        "never for something code can build",
    ] {
        assert!(SKILL.contains(words), "the skill says {words:?}");
    }
}

fn needing(path: &str) -> DesignReview {
    serde_json::from_str(&answer(Some(serde_json::json!({
        "what": "the approved signature artwork",
        "path": path
    }))))
    .unwrap()
}

#[test]
fn a_needs_path_that_is_absolute_has_dot_segments_or_is_outside_assets_and_the_documents_is_dropped() {
    for path in [
        "../secrets.txt",
        "assets/../DESIGN.md",
        "assets/./signature.svg",
        "./assets/signature.svg",
        "/etc/passwd",
        "src/signature.svg",
        "references/desktop.png",
        "assets",
        "assets/",
        "assets//signature.svg",
        "assets\\signature.svg",
        "NOTES.md",
        "",
    ] {
        let mut review = needing(path);
        let notes = drop_refused_needs(&mut review);
        assert_eq!(review.issues.len(), 1, "the issue is kept for {path:?}");
        assert_eq!(review.issues[0].needs, None, "needs is dropped for {path:?}");
        assert_eq!(notes.len(), 1, "one note for {path:?}");
        assert!(notes[0].contains("testimonials-signatures"), "{}", notes[0]);
    }
    for path in [
        "assets/signature.svg",
        "assets/testimonials/signature-1.png",
        "DESIGN.md",
        "UX.md",
        "RESPONSIVE.md",
        "tokens.css",
    ] {
        let mut review = needing(path);
        assert!(drop_refused_needs(&mut review).is_empty(), "{path:?} is kept");
        assert_eq!(review.issues[0].needs.as_ref().unwrap().path, path);
    }
}
