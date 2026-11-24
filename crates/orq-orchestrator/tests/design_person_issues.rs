//! A person's issues for a frontend run whose design review step ended needing review: recorded as the
//! run's `design_person_issues` event with one extra correction round, given to that round's fix session
//! first, and kept by every later review under their own ids. Fake agents and capture.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    design_round_label, record_design_person_issues, record_run_completed,
    review_design_with_person, run_design_loop, CaptureOutcome, CaptureRecord, CaptureRun,
    CorrectionOutcome, CorrectionPart, DesignAgent, DesignLoopOutcome, DesignLoopSteps, DesignMode,
    DesignReview, DesignReviewOutcome, DesignReviewRun, DesignSettings, DesignVerdict, Event,
    IssueBasis, IssueStatus, PageCapture, PersonGrant, ReviewIssue, RunRecord, RunStore, Severity,
    StoreError, Viewport,
};
use orq_test_support::{GitFixture, TempDir};
use serde_json::{json, Value};

struct World {
    store: RunStore,
    root: PathBuf,
    record: RunRecord,
}

fn world(tag: &str) -> World {
    let root = orq_test_support::process_temp_dir()
        .join(format!("orq-design-person-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let store = RunStore::new(&root.join("state"));
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
        worktree: root.join("worktree"),
        repository: String::new(),
        profiles: BTreeMap::new(),
    };
    store.create(&record).unwrap();
    World {
        store,
        root,
        record,
    }
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
        needs: None,
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

fn changes() -> DesignReviewOutcome {
    outcome(
        DesignVerdict::ChangesRequired,
        vec![issue("hero-size", IssueStatus::Open)],
    )
}

fn pass() -> DesignReviewOutcome {
    outcome(DesignVerdict::Pass, Vec::new())
}

/// The fake agents: what each review answers, in order, and each fix session's part.
struct Fake {
    head: String,
    reviews: VecDeque<DesignReviewOutcome>,
    /// The batches given to the implementer: the batch and its part.
    parts: Vec<(u32, CorrectionPart)>,
    /// Each review's round and the person's issues it was given.
    reviewed: Vec<(u32, Vec<String>)>,
}

impl Fake {
    fn new(head: &str, reviews: Vec<DesignReviewOutcome>) -> Fake {
        Fake {
            head: head.to_string(),
            reviews: reviews.into(),
            parts: Vec::new(),
            reviewed: Vec::new(),
        }
    }
}

impl DesignLoopSteps for Fake {
    fn head(&mut self) -> Result<String, StoreError> {
        Ok(self.head.clone())
    }

    fn review(&mut self, round: u32) -> Result<DesignReviewOutcome, StoreError> {
        self.review_with_person(round, &[])
    }

    fn review_with_person(
        &mut self,
        round: u32,
        person: &[PersonGrant],
    ) -> Result<DesignReviewOutcome, StoreError> {
        self.reviewed.push((
            round,
            person
                .iter()
                .flat_map(|g| g.issues.iter().map(|i| i.id.clone()))
                .collect(),
        ));
        self.reviews
            .pop_front()
            .ok_or_else(|| StoreError::Corrupt("interrupted".to_string()))
    }

    fn correct(
        &mut self,
        _batch: u32,
        _issues: &[ReviewIssue],
        _suggestions: &[String],
    ) -> Result<CorrectionOutcome, StoreError> {
        unreachable!("the loop gives parts")
    }

    fn correct_part(
        &mut self,
        batch: u32,
        part: &CorrectionPart,
    ) -> Result<CorrectionOutcome, StoreError> {
        self.parts.push((batch, part.clone()));
        self.head = format!("fix-{batch}");
        Ok(CorrectionOutcome::Done)
    }

    fn review_code(&mut self, _batch: u32, _from: &str) -> Result<CorrectionOutcome, StoreError> {
        Ok(CorrectionOutcome::Done)
    }
}

const TWO_ISSUES: &str = r#"[
  {"section": "Footer", "viewport": "all", "expected": "the logo on a transparent background",
   "actual": "the logo sits on a white box", "fix_direction": "drop the logo's background"},
  {"section": "Testimonials", "viewport": "desktop", "expected": "the carousel has previous and next controls",
   "actual": "no controls are shown", "severity": "critical"}
]"#;

/// A run whose design review step used its one correction round and ended needing review.
fn ended_needing_review(w: &World) {
    let mut first = Fake::new("head-1", vec![changes(), changes()]);
    let got = run_design_loop(&w.store, "run-1", 1, &mut first).unwrap();
    assert!(
        matches!(got, DesignLoopOutcome::NeedsReview { .. }),
        "{got:?}"
    );
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

fn person_events(w: &World) -> usize {
    w.store
        .events("run-1")
        .unwrap()
        .iter()
        .filter(|e| e.kind == "design_person_issues")
        .count()
}

fn viewports() -> Vec<String> {
    vec!["desktop".to_string(), "mobile".to_string()]
}

#[test]
fn a_needs_review_design_run_given_two_issues_runs_one_extra_batch_with_them_first() {
    let w = world("extra-batch");
    ended_needing_review(&w);

    let recorded =
        record_design_person_issues(&w.store, "run-1", TWO_ISSUES, &viewports()).unwrap();
    assert_eq!(
        recorded.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["person-1", "person-2"]
    );
    assert!(recorded.iter().all(|i| i.basis == Some(IssueBasis::Person)));
    assert_eq!(recorded[0].severity, Severity::Major);
    assert_eq!(recorded[1].severity, Severity::Critical);

    let mut after = Fake::new("fix-1", vec![pass()]);
    let got = run_design_loop(&w.store, "run-1", 1, &mut after).unwrap();
    assert_eq!(
        got,
        DesignLoopOutcome::Passed {
            head: "fix-2".to_string()
        }
    );
    assert_eq!(after.parts.len(), 1, "one extra batch");
    let (batch, part) = &after.parts[0];
    assert_eq!(*batch, 2);
    assert_eq!(
        part.issues
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        ["person-1", "person-2", "hero-size"]
    );
    assert!(part.task.contains("from the person"), "{}", part.task);
    let person_at = part.task.find("person-2").unwrap();
    let reviewer_at = part.task.find("hero-size").unwrap();
    assert!(person_at < reviewer_at, "{}", part.task);
    let _ = fs::remove_dir_all(&w.root);
}

/// A run resuming `from` as `run_id`, with the design review step carried into it.
fn resumed(w: &World, from: &str, run_id: &str) -> RunRecord {
    let record = RunRecord {
        run_id: run_id.to_string(),
        ..w.record.clone()
    };
    w.store.create(&record).unwrap();
    let logs = w.root.join("logs");
    fs::create_dir_all(logs.join(from)).unwrap();
    orq_orchestrator::carry_design(&w.store, &record, from, &logs.join(from), &logs.join(run_id))
        .unwrap();
    record
}

#[test]
fn issues_recorded_on_the_stopped_run_reach_the_resumed_run_which_runs_one_extra_batch_and_a_later_resume_none(
) {
    let w = world("carried");
    ended_needing_review(&w);
    record_design_person_issues(&w.store, "run-1", TWO_ISSUES, &viewports()).unwrap();

    // The resume is a run of its own; the issues and their extra round come with the design review step.
    let second = resumed(&w, "run-1", "run-2");
    let mut steps = Fake::new("fix-1", vec![changes()]);
    let got = run_design_loop(&w.store, "run-2", 1, &mut steps).unwrap();
    assert!(matches!(got, DesignLoopOutcome::NeedsReview { .. }), "{got:?}");
    assert_eq!(steps.parts.len(), 1, "one extra batch");
    let (batch, part) = &steps.parts[0];
    assert_eq!(*batch, 2);
    assert_eq!(
        part.issues.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["person-1", "person-2", "hero-size"]
    );
    assert_eq!(
        steps.reviewed.last().unwrap().1,
        ["person-1", "person-2"],
        "the review after it is given the person's issues"
    );
    record_run_completed(&w.store, &second, "needs_review", Some("still requires changes")).unwrap();

    // A plain resume of that run gets no extra round again, but its reviews still keep the issues.
    resumed(&w, "run-2", "run-3");
    let mut plain = Fake::new("fix-2", vec![changes()]);
    let got = run_design_loop(&w.store, "run-3", 1, &mut plain).unwrap();
    assert!(matches!(got, DesignLoopOutcome::NeedsReview { .. }), "{got:?}");
    assert!(plain.parts.is_empty(), "{:?}", plain.parts);
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn a_run_that_is_not_a_design_run_or_did_not_end_in_its_design_review_or_an_invalid_file_is_refused(
) {
    // Not a design run: no design review step at all.
    let plain = world("not-design");
    stage_event(&plain, "stage.started", "implement");
    record_run_completed(&plain.store, &plain.record, "needs_review", Some("x")).unwrap();
    let err =
        record_design_person_issues(&plain.store, "run-1", TWO_ISSUES, &viewports()).unwrap_err();
    assert!(
        err.contains("not a frontend run with a design review step"),
        "{err}"
    );
    assert_eq!(person_events(&plain), 0);
    let _ = fs::remove_dir_all(&plain.root);

    // A design run that ended later, in its gate.
    let later = world("ended-later");
    ended_needing_review(&later);
    stage_event(&later, "stage.started", "gate");
    stage_event(&later, "stage.failed", "gate");
    let err =
        record_design_person_issues(&later.store, "run-1", TWO_ISSUES, &viewports()).unwrap_err();
    assert!(err.contains("did not end"), "{err}");
    assert!(err.contains("gate"), "{err}");
    assert_eq!(person_events(&later), 0);
    let _ = fs::remove_dir_all(&later.root);

    let w = world("invalid-file");
    ended_needing_review(&w);
    let long = "x".repeat(2001);
    let cases: Vec<(String, &str)> = vec![
        ("[]".to_string(), "lists no issue"),
        ("not json".to_string(), "JSON"),
        (
            r#"[{"section": "Footer", "viewport": "tablet", "expected": "a", "actual": "b"}]"#
                .to_string(),
            "viewport",
        ),
        (
            r#"[{"section": "", "viewport": "all", "expected": "a", "actual": "b"}]"#.to_string(),
            "section",
        ),
        (
            format!(
                r#"[{{"section": "Footer", "viewport": "all", "expected": "{long}", "actual": "b"}}]"#
            ),
            "expected",
        ),
        (
            r#"[{"section": "Footer", "viewport": "all", "expected": "a", "actual": "b\u0007"}]"#
                .to_string(),
            "actual",
        ),
        (
            r#"[{"section": "Footer", "viewport": "all", "expected": "a"}]"#.to_string(),
            "actual",
        ),
    ];
    for (text, named) in cases {
        let err = record_design_person_issues(&w.store, "run-1", &text, &viewports()).unwrap_err();
        assert!(err.contains(named), "{text}: {err}");
    }
    assert_eq!(person_events(&w), 0, "nothing recorded");
    let _ = fs::remove_dir_all(&w.root);
}

// ---------------------------------------------------------------- the review, with fake pi agents

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A black 4x3 RGB PNG.
fn png() -> Vec<u8> {
    let (width, height) = (4u32, 3u32);
    let raw = height as usize * (1 + 3 * width as usize);
    let mut zlib = vec![0x78, 0x01, 1];
    zlib.extend_from_slice(&(raw as u16).to_le_bytes());
    zlib.extend_from_slice(&(!(raw as u16)).to_le_bytes());
    zlib.extend(std::iter::repeat_n(0, raw));
    zlib.extend_from_slice(&((((raw % 65521) as u32) << 16) | 1).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &zlib);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// A fake pi that answers `answer.json` of its folder, `@RUN@`, `@REF@` and `@CAP@` replaced by what
/// the prompt names, and keeps its last prompt in `prompt`.
fn fake_pi(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let script = dir.join("pi");
    let d = dir.display();
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             for a; do prompt=$a; done\n\
             printf '%s\\n' \"$prompt\" > '{d}/prompt'\n\
             run=$(printf '%s\\n' \"$prompt\" | sed -n 's/^run_id: //p' | head -n 1)\n\
             ref=$(printf '%s\\n' \"$prompt\" | sed -n 's/^reference_hash: //p' | head -n 1)\n\
             cap=$(printf '%s\\n' \"$prompt\" | sed -n 's/^capture_revision: //p' | head -n 1)\n\
             sed -e \"s|@RUN@|$run|g\" -e \"s|@REF@|$ref|g\" -e \"s|@CAP@|$cap|g\" '{d}/answer.json'\n\
             exit 0\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn fake_capture(run: &CaptureRun) -> CaptureOutcome {
    let round = run
        .run_logs
        .join("design")
        .join(format!("round-{}", run.round));
    fs::create_dir_all(round.join("desktop")).unwrap();
    fs::write(round.join("desktop/index.png"), png()).unwrap();
    let record = CaptureRecord {
        round: run.round,
        commit: None,
        browser_version: "fake".to_string(),
        port: 1,
        pages: vec![PageCapture {
            route: "/".to_string(),
            viewport: "desktop".to_string(),
            viewport_width: 4,
            viewport_height: 3,
            image: Some("desktop/index.png".to_string()),
            image_width: Some(4),
            image_height: Some(3),
            failure: None,
        }],
        failures: vec![],
    };
    fs::write(
        round.join("capture.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    CaptureOutcome::Captured(record)
}

struct Reviewing {
    repo: GitFixture,
    logs: TempDir,
    agents: TempDir,
    settings: DesignSettings,
}

impl Reviewing {
    fn new() -> Reviewing {
        let repo = GitFixture::new();
        let package = repo.root().join("designs").join("page");
        fs::create_dir_all(package.join("references")).unwrap();
        fs::write(package.join("references/desktop.png"), png()).unwrap();
        fs::write(package.join("DESIGN.md"), "# Page\n").unwrap();
        let agents = TempDir::new("orq-design-person-agents");
        fake_pi(agents.path());
        let viewports: BTreeMap<String, Viewport> = [(
            "desktop".to_string(),
            Viewport {
                width: 4,
                height: 3,
            },
        )]
        .into_iter()
        .collect();
        Reviewing {
            repo,
            logs: TempDir::new("orq-design-person-logs"),
            agents,
            settings: DesignSettings {
                start_command: Some(vec!["true".into()]),
                ready_path: "/".into(),
                ready_timeout: Duration::from_secs(5),
                max_correction_rounds: 1,
                verify_after_rounds: 0,
                mode: DesignMode::Infer,
                viewports,
            },
        }
    }

    fn answer(&self, issues: Vec<Value>) {
        self.answer_with("changes_required", issues);
    }

    fn answer_with(&self, verdict: &str, issues: Vec<Value>) {
        let answer = json!({
            "run_id": "@RUN@", "reference_hash": "@REF@", "capture_revision": "@CAP@",
            "evaluation": "compared the desktop capture", "verdict": verdict,
            "checks": [], "issues": issues, "suggestions": [], "blockers": [],
        });
        fs::write(self.agents.path().join("answer.json"), answer.to_string()).unwrap();
    }

    fn prompt(&self) -> String {
        fs::read_to_string(self.agents.path().join("prompt")).unwrap()
    }

    fn review(&self, person: &[PersonGrant]) -> DesignReviewOutcome {
        let agent = DesignAgent {
            program: self.agents.path().join("pi"),
            harness: "pi".to_string(),
            model: "openai/gpt-6.1-sol".to_string(),
            thinking: None,
        };
        let run = DesignReviewRun {
            run_id: "run-1",
            main_checkout: self.repo.root(),
            feature: "page",
            settings: &self.settings,
            run_logs: self.logs.path(),
            worktree: self.repo.root(),
            round: 3,
            reviewer: &agent,
            verifier: &agent,
            skill: "the design-review skill",
            schema: "the review schema",
            agent_timeout: Duration::from_secs(30),
        };
        review_design_with_person(&run, &fake_capture, person)
    }
}

/// How the reviewer reports one of the person's issues: `status`, and as a minor visual issue.
fn reported(id: &str, status: &str) -> Value {
    json!({
        "id": id, "section": "Footer", "viewport": "desktop", "source": "references/desktop.png",
        "expected": "the logo on a transparent background", "actual": "the logo sits on a white box",
        "evidence": ["round-3/desktop/index.png"], "severity": "minor", "status": status,
        "fix_direction": "drop the logo's background", "basis": "visual", "copy": false,
    })
}

fn grant() -> PersonGrant {
    let issues = orq_orchestrator::person_issues(TWO_ISSUES, &viewports(), 1).unwrap();
    PersonGrant {
        extra: 1,
        batch: 2,
        issues,
    }
}

#[test]
fn a_minor_person_issue_left_open_or_regressed_keeps_the_page_from_passing() {
    let minor = r#"[{"section": "Footer", "viewport": "all", "expected": "a transparent logo",
      "actual": "the logo sits on a white box", "severity": "minor"}]"#;
    let given = PersonGrant {
        extra: 1,
        batch: 2,
        issues: orq_orchestrator::person_issues(minor, &viewports(), 1).unwrap(),
    };
    let r = Reviewing::new();
    for status in ["open", "regressed"] {
        // The reviewer asks for changes: orq does not turn it into a pass.
        r.answer(vec![reported("person-1", status)]);
        let got = r.review(std::slice::from_ref(&given));
        assert_eq!(got.verdict, DesignVerdict::ChangesRequired, "{status}: {got:?}");
        // The reviewer calls it a pass: orq refuses it.
        r.answer_with("pass", vec![reported("person-1", status)]);
        let got = r.review(std::slice::from_ref(&given));
        assert_ne!(got.verdict, DesignVerdict::Pass, "{status}: {got:?}");
        let reason = got.reason.unwrap_or_default();
        assert!(reason.contains("person-1"), "{status}: {reason}");
    }
}

#[test]
fn the_next_review_must_report_each_of_the_persons_issues_and_one_that_drops_one_is_refused() {
    let r = Reviewing::new();
    r.answer(vec![reported("person-1", "resolved")]);
    let got = r.review(&[grant()]);
    assert_eq!(got.verdict, DesignVerdict::Blocked, "{got:?}");
    let reason = got.reason.unwrap_or_default();
    assert!(reason.contains("person-2"), "{reason}");
    let prompt = r.prompt();
    assert!(
        prompt.contains("person-1") && prompt.contains("person-2"),
        "{prompt}"
    );

    r.answer(vec![
        reported("person-1", "resolved"),
        reported("person-2", "open"),
    ]);
    let got = r.review(&[grant()]);
    assert_eq!(got.verdict, DesignVerdict::ChangesRequired, "{got:?}");
    let review = got.review.unwrap();
    let kept = review.issues.iter().find(|i| i.id == "person-2").unwrap();
    // The person's issue keeps its basis and severity, whatever the reviewer called it.
    assert_eq!(kept.basis, Some(IssueBasis::Person));
    assert_eq!(kept.severity, Severity::Critical);
}

#[test]
fn the_persons_issues_and_their_extra_round_show_in_the_design_report_and_on_the_run_page() {
    let r = Reviewing::new();
    r.answer(vec![
        reported("person-1", "resolved"),
        reported("person-2", "open"),
    ]);
    let got = r.review(&[grant()]);
    let report = fs::read_to_string(&got.report).unwrap();
    assert!(report.contains("## Your issues"), "{report}");
    assert!(
        report.contains("extra round 1, from your issues"),
        "{report}"
    );
    let section = &report[report.find("## Your issues").unwrap()..];
    assert!(
        section.contains("person-1") && section.contains("resolved"),
        "{report}"
    );
    assert!(
        section.contains("person-2") && section.contains("open"),
        "{report}"
    );

    // The run page names the extra round by its events.
    let w = world("label");
    ended_needing_review(&w);
    record_design_person_issues(&w.store, "run-1", TWO_ISSUES, &viewports()).unwrap();
    let mut after = Fake::new("fix-1", vec![pass()]);
    run_design_loop(&w.store, "run-1", 1, &mut after).unwrap();
    let labels: Vec<String> = w
        .store
        .events("run-1")
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "design_person_issues" | "design_correction"
            )
        })
        .filter_map(design_round_label)
        .collect();
    assert!(
        labels
            .iter()
            .filter(|l| l.contains("extra round 1, from your issues"))
            .count()
            == 2,
        "{labels:?}"
    );
    let _ = fs::remove_dir_all(&w.root);
}

#[test]
fn the_extra_round_is_not_granted_again_by_a_second_plain_resume() {
    let w = world("no-second-grant");
    ended_needing_review(&w);
    record_design_person_issues(&w.store, "run-1", TWO_ISSUES, &viewports()).unwrap();

    let mut extra = Fake::new("fix-1", vec![changes()]);
    let got = run_design_loop(&w.store, "run-1", 1, &mut extra).unwrap();
    assert!(
        matches!(got, DesignLoopOutcome::NeedsReview { .. }),
        "{got:?}"
    );
    assert_eq!(extra.parts.len(), 1);
    record_run_completed(
        &w.store,
        &w.record,
        "needs_review",
        Some("still requires changes"),
    )
    .unwrap();

    let mut plain = Fake::new("fix-2", Vec::new());
    let got = run_design_loop(&w.store, "run-1", 1, &mut plain).unwrap();
    assert!(
        matches!(got, DesignLoopOutcome::NeedsReview { .. }),
        "{got:?}"
    );
    assert!(plain.parts.is_empty(), "no batch: {:?}", plain.parts);
    assert!(plain.reviewed.is_empty(), "no review: {:?}", plain.reviewed);
    let _ = fs::remove_dir_all(&w.root);
}
