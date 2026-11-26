//! A design review with no reference for a viewport also captures the run's target branch at those
//! viewports (a temporary worktree of the target at its head, served the way the run's page is) and
//! gives the reviewer those captures as `baseline/<viewport>.png`: what "the existing page" is. The
//! reviewer is a fake pi script answering from a file; the capture is a stand-in, as in
//! `design_capture.rs`'s tests.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orq_orchestrator::{
    review_design_with_baseline, CaptureOutcome, CaptureRecord, CaptureRun, DesignAgent,
    DesignBaseline, DesignMode, DesignReviewRun, DesignSettings, PageCapture, Viewport,
};
use orq_test_support::{GitFixture, TempDir};
use serde_json::{json, Value};

const FEATURE: &str = "home";
const RUN_ID: &str = "run-review-baseline";
const TARGET: &str = "target";

// ---------------------------------------------------------------- a valid PNG

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

fn zlib_zeros(n: usize) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut left = n;
    loop {
        let block = left.min(65_535);
        left -= block;
        out.push(if left == 0 { 1 } else { 0 });
        out.extend_from_slice(&(block as u16).to_le_bytes());
        out.extend_from_slice(&(!(block as u16)).to_le_bytes());
        out.extend(std::iter::repeat_n(0, block));
        if left == 0 {
            break;
        }
    }
    let adler = (((n % 65521) as u32) << 16) | 1;
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(
        &mut out,
        b"IDAT",
        &zlib_zeros(height as usize * (1 + 3 * width as usize)),
    );
    png_chunk(&mut out, b"IEND", &[]);
    out
}

// ---------------------------------------------------------------- fixtures

const SIZES: [(&str, u32, u32); 2] = [("desktop", 400, 300), ("mobile", 200, 400)];

fn settings(mode: DesignMode) -> DesignSettings {
    let viewports: BTreeMap<String, Viewport> = SIZES
        .into_iter()
        .map(|(name, width, height)| (name.to_string(), Viewport { width, height }))
        .collect();
    DesignSettings {
        start_command: Some(vec!["true".into()]),
        ready_path: "/".into(),
        ready_timeout: Duration::from_secs(5),
        max_correction_rounds: 3,
        verify_after_rounds: 0,
        mode,
        viewports,
    }
}

/// A fake pi: records its argv and answers with `answer.json` of its folder, `@RUN@`, `@REF@` and
/// `@CAP@` replaced by what the prompt names.
fn fake_pi(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let script = dir.join("pi");
    let d = dir.display();
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             for a; do prompt=$a; done\n\
             n=$(cat '{d}/count' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{d}/count'\n\
             printf '%s\\n' \"$@\" > '{d}/argv-'$n\n\
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

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=Orq Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// One capture the review asked for: where, at which viewports, into which logs.
#[derive(Debug, Clone)]
struct Captured {
    worktree: PathBuf,
    /// The worktree's HEAD when it was captured.
    head: String,
    viewports: Vec<String>,
    /// Whether the worktree had been prepared (the fixture's setup step writes `prepared.txt`).
    prepared: bool,
}

/// A main checkout with a target branch and a package whose only reference is
/// `references/desktop.png` (infer mode: mobile is reviewed with no reference); the run's logs, the
/// run's shared baseline folder and a fake reviewer and verifier.
struct Fixture {
    repo: GitFixture,
    worktree: PathBuf,
    logs: TempDir,
    cache: TempDir,
    agents: TempDir,
    settings: DesignSettings,
    /// The run's setup steps, which prepare the target's worktree too.
    setup: Vec<Vec<String>>,
}

impl Fixture {
    fn new(mode: DesignMode) -> Self {
        let repo = GitFixture::new();
        git(repo.root(), &["branch", TARGET]);
        let worktree = repo.add_linked_worktree("review-baseline");
        let agents = TempDir::new("orq-design-baseline-agents");
        fake_pi(&agents.path().join("reviewer"));
        fake_pi(&agents.path().join("verifier"));
        let f = Fixture {
            repo,
            worktree,
            logs: TempDir::new("orq-design-baseline-logs"),
            cache: TempDir::new("orq-design-baseline-cache"),
            agents,
            settings: settings(mode),
            setup: Vec::new(),
        };
        f.put("DESIGN.md", b"# Home\n");
        f.put("UX.md", b"# UX\n\nKeep the existing contact section.\n");
        f.put("RESPONSIVE.md", b"# Responsive\n");
        f.put("tokens.css", b":root { --brand: #123456; }\n");
        f.put("references/desktop.png", &png(400, 300));
        f.answer(&answer("pass", vec![]));
        f
    }

    fn put(&self, path: &str, bytes: &[u8]) {
        let path = self.repo.root().join("designs").join(FEATURE).join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn answer(&self, answer: &Value) {
        for role in ["reviewer", "verifier"] {
            fs::write(
                self.agents.path().join(role).join("answer.json"),
                answer.to_string(),
            )
            .unwrap();
        }
    }

    fn calls(&self) -> u32 {
        fs::read_to_string(self.agents.path().join("reviewer/count"))
            .map(|n| n.trim().parse().unwrap())
            .unwrap_or(0)
    }

    fn argv(&self, call: u32) -> String {
        fs::read_to_string(
            self.agents
                .path()
                .join("reviewer")
                .join(format!("argv-{call}")),
        )
        .unwrap()
    }

    fn baseline(&self) -> DesignBaseline<'_> {
        DesignBaseline {
            repo: self.repo.root(),
            branch: TARGET,
            cache: self.cache.path(),
            setup: &self.setup,
        }
    }

    /// Reviews `round`; the capture records each call and answers with `capture`.
    fn review(
        &self,
        round: u32,
        calls: &RefCell<Vec<Captured>>,
        capture: &dyn Fn(&CaptureRun) -> CaptureOutcome,
    ) {
        self.review_in(self.logs.path(), round, calls, capture)
    }

    /// [`Fixture::review`] with the round's logs in `logs`: another member of the run's group.
    fn review_in(
        &self,
        logs: &Path,
        round: u32,
        calls: &RefCell<Vec<Captured>>,
        capture: &dyn Fn(&CaptureRun) -> CaptureOutcome,
    ) {
        let agent = |role: &str| DesignAgent {
            program: self.agents.path().join(role).join("pi"),
            harness: "pi".to_string(),
            model: "openai/gpt-6.1-sol".to_string(),
            thinking: None,
        };
        let (reviewer, verifier) = (agent("reviewer"), agent("verifier"));
        let run = DesignReviewRun {
            run_id: RUN_ID,
            main_checkout: self.repo.root(),
            feature: FEATURE,
            settings: &self.settings,
            run_logs: logs,
            worktree: &self.worktree,
            round,
            reviewer: &reviewer,
            verifier: &verifier,
            skill: "the design-review skill",
            schema: "the review schema",
            agent_timeout: Duration::from_secs(30),
        };
        let recording = |run: &CaptureRun| {
            calls.borrow_mut().push(Captured {
                worktree: run.worktree.to_path_buf(),
                head: git(run.worktree, &["rev-parse", "HEAD"]),
                viewports: run.viewports.iter().map(|v| v.name.clone()).collect(),
                prepared: run.worktree.join("prepared.txt").is_file(),
            });
            capture(run)
        };
        review_design_with_baseline(&run, &recording, &[], Some(&self.baseline()));
    }

    fn report(&self) -> String {
        fs::read_to_string(self.logs.path().join("design/report.md")).unwrap()
    }

    /// The captures of the target's page among `calls`.
    fn baseline_calls(&self, calls: &RefCell<Vec<Captured>>) -> Vec<Captured> {
        calls
            .borrow()
            .iter()
            .filter(|call| call.worktree != self.worktree)
            .cloned()
            .collect()
    }
}

/// One screenshot per viewport asked for, its measurements and `capture.json`, as the real capture
/// writes them.
fn fake_capture(run: &CaptureRun) -> CaptureOutcome {
    let round = run
        .run_logs
        .join("design")
        .join(format!("round-{}", run.round));
    let mut pages = Vec::new();
    for viewport in run.viewports {
        fs::create_dir_all(round.join(&viewport.name)).unwrap();
        fs::write(
            round.join(&viewport.name).join("index.png"),
            png(viewport.width, viewport.height),
        )
        .unwrap();
        fs::write(
            round.join(&viewport.name).join("measurements.json"),
            json!({"routes": {"/": {"elements": []}}}).to_string(),
        )
        .unwrap();
        pages.push(PageCapture {
            route: "/".to_string(),
            viewport: viewport.name.clone(),
            viewport_width: viewport.width,
            viewport_height: viewport.height,
            image: Some(format!("{}/index.png", viewport.name)),
            image_width: Some(viewport.width),
            image_height: Some(viewport.height),
            failure: None,
        });
    }
    let record = CaptureRecord {
        round: run.round,
        commit: None,
        browser_version: "fake".to_string(),
        port: 1,
        pages,
        failures: vec![],
    };
    fs::write(
        round.join("capture.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    CaptureOutcome::Captured(record)
}

fn answer(verdict: &str, issues: Vec<Value>) -> Value {
    json!({
        "run_id": "@RUN@", "reference_hash": "@REF@", "capture_revision": "@CAP@",
        "evaluation": "compared the captures", "verdict": verdict,
        "checks": [], "issues": issues, "suggestions": [], "blockers": [],
    })
}

// ---------------------------------------------------------------- tests

#[test]
fn a_review_without_a_reference_captures_the_target_once_and_gives_its_baseline_images() {
    let f = Fixture::new(DesignMode::Infer);
    let target_head = git(f.repo.root(), &["rev-parse", TARGET]);
    // The run's branch moves on from the target: the baseline is the target's page, not the run's.
    fs::write(f.worktree.join("page.html"), "the run's page\n").unwrap();
    git(&f.worktree, &["add", "page.html"]);
    git(
        &f.worktree,
        &["commit", "--quiet", "-m", "the run's change"],
    );

    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &fake_capture);

    let baseline = f.baseline_calls(&calls);
    assert_eq!(baseline.len(), 1, "the target is captured once: {calls:?}");
    assert_eq!(baseline[0].head, target_head, "at the target's head");
    assert_eq!(
        baseline[0].viewports,
        vec!["mobile"],
        "at the viewports with no reference"
    );
    assert!(
        !baseline[0].worktree.exists(),
        "the target's temporary worktree is removed: {:?}",
        baseline[0].worktree
    );
    let worktrees = git(f.repo.root(), &["worktree", "list", "--porcelain"]);
    assert!(
        !worktrees.contains(baseline[0].worktree.to_str().unwrap()),
        "{worktrees}"
    );

    let design = f.logs.path().join("design");
    assert!(design.join("baseline/mobile.png").is_file());
    assert!(design.join("baseline/mobile-measurements.json").is_file());
    let argv = f.argv(f.calls());
    assert!(
        argv.contains(&format!(
            "@{}",
            design.join("baseline/mobile.png").display()
        )),
        "the baseline image is given to the reviewer: {argv}"
    );
    assert!(
        argv.contains("baseline/mobile.png (route /, viewport mobile"),
        "the prompt names the baseline: {argv}"
    );
    assert!(
        argv.contains("baseline/mobile-measurements.json"),
        "the prompt names the baseline's measurements: {argv}"
    );
    assert!(
        f.report()
            .contains("Reviewed by the rules and the target's page (no reference)"),
        "{}",
        f.report()
    );
}

#[test]
fn later_rounds_and_members_at_the_same_target_head_reuse_the_baseline() {
    let f = Fixture::new(DesignMode::Infer);
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &fake_capture);
    f.review(2, &calls, &fake_capture);
    let member = TempDir::new("orq-design-baseline-member-logs");
    f.review_in(member.path(), 1, &calls, &fake_capture);
    assert_eq!(
        f.baseline_calls(&calls).len(),
        1,
        "the target is captured once while its head is the same: {calls:?}"
    );
    for logs in [f.logs.path(), member.path()] {
        let image = logs.join("design/baseline/mobile.png");
        assert!(image.is_file(), "{image:?}");
        let report = fs::read_to_string(logs.join("design/report.md")).unwrap();
        assert!(
            report.contains("Reviewed by the rules and the target's page (no reference)"),
            "{report}"
        );
    }
    let argv = f.argv(f.calls());
    assert!(
        argv.contains(&format!(
            "@{}",
            member.path().join("design/baseline/mobile.png").display()
        )),
        "the reused baseline is given to the reviewer: {argv}"
    );

    // The target moves on: its page is captured again, at the new head.
    git(
        f.repo.root(),
        &["commit", "--quiet", "--allow-empty", "-m", "on"],
    );
    git(f.repo.root(), &["branch", "--force", TARGET, "HEAD"]);
    let moved = git(f.repo.root(), &["rev-parse", TARGET]);
    f.review(3, &calls, &fake_capture);
    let baseline = f.baseline_calls(&calls);
    assert_eq!(baseline.len(), 2, "{calls:?}");
    assert_eq!(baseline[1].head, moved);
}

#[test]
fn a_baseline_that_cannot_be_captured_leaves_the_review_by_the_rules_only_with_the_reason() {
    let f = Fixture::new(DesignMode::Infer);
    let run_worktree = f.worktree.clone();
    let server_down = |run: &CaptureRun| {
        if run.worktree == run_worktree {
            fake_capture(run)
        } else {
            CaptureOutcome::Blocked("the start command exited before the page was ready".into())
        }
    };
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &server_down);

    assert_eq!(f.calls(), 1, "the review goes on");
    let record: Value = serde_json::from_slice(
        &fs::read(f.logs.path().join("design/round-1/review.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["verdict"], "pass", "{record}");
    let argv = f.argv(1);
    assert!(!argv.contains("baseline/"), "no baseline is given: {argv}");
    let report = f.report();
    assert!(report.contains("Reviewed by the rules only"), "{report}");
    assert!(
        report.contains("the start command exited before the page was ready"),
        "the report says why: {report}"
    );
    let baseline = f.baseline_calls(&calls);
    assert!(!baseline[0].worktree.exists(), "the worktree is removed");

    // At the same head the failure stands: the target is not started again each round.
    f.review(2, &calls, &server_down);
    assert_eq!(f.baseline_calls(&calls).len(), 1, "{calls:?}");
    assert!(
        f.report()
            .contains("the start command exited before the page was ready"),
        "{}",
        f.report()
    );
}

#[test]
fn a_review_with_a_reference_for_every_viewport_captures_no_baseline() {
    let f = Fixture::new(DesignMode::Infer);
    f.put("references/mobile.png", &png(200, 400));
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &fake_capture);
    assert!(f.baseline_calls(&calls).is_empty(), "{calls:?}");
    assert_eq!(calls.borrow().len(), 1, "only the run's page is captured");
    let argv = f.argv(1);
    assert!(!argv.contains("baseline/"), "{argv}");
    assert!(!f.logs.path().join("design/baseline").exists());
    assert!(!f.report().contains("target's page"), "{}", f.report());
}

fn step(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

#[test]
fn the_targets_worktree_is_prepared_with_the_runs_setup_before_it_is_captured() {
    let mut f = Fixture::new(DesignMode::Infer);
    f.setup = vec![step(&["sh", "-c", "echo yes > prepared.txt"])];
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &fake_capture);
    let baseline = f.baseline_calls(&calls);
    assert_eq!(baseline.len(), 1, "{calls:?}");
    assert!(
        baseline[0].prepared,
        "the setup ran in the target's worktree first: {calls:?}"
    );
    assert!(
        f.report()
            .contains("Reviewed by the rules and the target's page (no reference)"),
        "{}",
        f.report()
    );
}

#[test]
fn a_target_worktree_whose_setup_fails_is_not_captured_and_the_report_says_why() {
    let mut f = Fixture::new(DesignMode::Infer);
    f.setup = vec![step(&["sh", "-c", "exit 3"])];
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &fake_capture);
    assert!(f.baseline_calls(&calls).is_empty(), "{calls:?}");
    assert_eq!(f.calls(), 1, "the review goes on");
    let report = f.report();
    assert!(
        report.contains("Reviewed by the rules only")
            && report.contains("its worktree could not be prepared"),
        "{report}"
    );
    let worktrees = git(f.repo.root(), &["worktree", "list", "--porcelain"]);
    assert!(
        !worktrees.contains(f.cache.path().to_str().unwrap()),
        "the target's worktree is removed: {worktrees}"
    );
}

/// A capture that ran but whose pages each failed: no screenshot, a failure line per page.
fn failed_pages(run: &CaptureRun) -> CaptureOutcome {
    let pages: Vec<PageCapture> = run
        .viewports
        .iter()
        .map(|viewport| PageCapture {
            route: "/".to_string(),
            viewport: viewport.name.clone(),
            viewport_width: viewport.width,
            viewport_height: viewport.height,
            image: None,
            image_width: None,
            image_height: None,
            failure: Some("the route answered 404".to_string()),
        })
        .collect();
    let failures = pages
        .iter()
        .map(|page| format!("/ at {}: the route answered 404", page.viewport))
        .collect();
    CaptureOutcome::Captured(CaptureRecord {
        round: run.round,
        commit: None,
        browser_version: "fake".to_string(),
        port: 1,
        pages,
        failures,
    })
}

#[test]
fn a_target_capture_whose_pages_failed_is_no_baseline_and_the_report_says_why() {
    let f = Fixture::new(DesignMode::Infer);
    let run_worktree = f.worktree.clone();
    let not_served = |run: &CaptureRun| {
        if run.worktree == run_worktree {
            fake_capture(run)
        } else {
            failed_pages(run)
        }
    };
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &not_served);
    assert_eq!(f.baseline_calls(&calls).len(), 1, "{calls:?}");
    assert_eq!(f.calls(), 1, "the review goes on");
    let argv = f.argv(1);
    assert!(!argv.contains("baseline/"), "no baseline is given: {argv}");
    let report = f.report();
    assert!(
        report.contains("Reviewed by the rules only")
            && !report.contains("Reviewed by the rules and the target's page")
            && report.contains("/ at mobile: the route answered 404"),
        "{report}"
    );
}

#[test]
fn members_asking_for_different_viewports_each_keep_their_baseline_for_later_rounds() {
    let f = Fixture::new(DesignMode::Infer);
    let other = TempDir::new("orq-design-baseline-other-member-logs");
    let calls = RefCell::new(Vec::new());
    // This member has no mobile reference; the other member has no desktop reference.
    f.review(1, &calls, &fake_capture);
    let package = f.repo.root().join("designs").join(FEATURE).join("references");
    fs::remove_file(package.join("desktop.png")).unwrap();
    f.put("references/mobile.png", &png(200, 400));
    f.review_in(other.path(), 1, &calls, &fake_capture);
    // Each member's next round: neither asks for the target's page again.
    fs::remove_file(package.join("mobile.png")).unwrap();
    f.put("references/desktop.png", &png(400, 300));
    f.review(2, &calls, &fake_capture);
    fs::remove_file(package.join("desktop.png")).unwrap();
    f.put("references/mobile.png", &png(200, 400));
    f.review_in(other.path(), 2, &calls, &fake_capture);

    let baseline = f.baseline_calls(&calls);
    let asked: Vec<Vec<String>> = baseline.iter().map(|call| call.viewports.clone()).collect();
    assert_eq!(
        asked,
        vec![vec!["mobile".to_string()], vec!["desktop".to_string()]],
        "each member's target page is captured once: {calls:?}"
    );
    assert!(f.logs.path().join("design/baseline/mobile.png").is_file());
    assert!(other.path().join("design/baseline/desktop.png").is_file());
}

/// Every file under `dir` named `name`.
fn files_named(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_named(&path, name));
        } else if path.file_name().is_some_and(|n| n == name) {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_kept_capture_whose_screenshot_is_gone_is_captured_again() {
    let f = Fixture::new(DesignMode::Infer);
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &fake_capture);
    let kept = files_named(f.cache.path(), "index.png");
    assert!(!kept.is_empty(), "the target's capture is kept");
    for image in kept {
        fs::remove_file(image).unwrap();
    }
    f.review(2, &calls, &fake_capture);
    assert_eq!(f.baseline_calls(&calls).len(), 2, "{calls:?}");
    assert!(f.logs.path().join("design/baseline/mobile.png").is_file());
    assert!(
        f.report()
            .contains("Reviewed by the rules and the target's page (no reference)"),
        "{}",
        f.report()
    );
}

#[test]
fn a_target_capture_with_no_measurements_gives_its_images_and_names_no_measurements() {
    let f = Fixture::new(DesignMode::Infer);
    let run_worktree = f.worktree.clone();
    let unmeasured = |run: &CaptureRun| {
        let outcome = fake_capture(run);
        if run.worktree != run_worktree {
            for measured in files_named(run.run_logs, "measurements.json") {
                fs::remove_file(measured).unwrap();
            }
        }
        outcome
    };
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &unmeasured);
    let design = f.logs.path().join("design");
    assert!(design.join("baseline/mobile.png").is_file());
    assert!(!design.join("baseline/mobile-measurements.json").exists());
    let argv = f.argv(f.calls());
    assert!(
        argv.contains("baseline/mobile.png (route /, viewport mobile"),
        "{argv}"
    );
    assert!(
        !argv.contains("Baseline measured CSS and DOM"),
        "no measurements are named that are not there: {argv}"
    );
}

#[test]
fn an_unreadable_target_branch_leaves_the_review_by_the_rules_only() {
    let f = Fixture::new(DesignMode::Infer);
    git(f.repo.root(), &["branch", "--delete", "--force", TARGET]);
    let calls = RefCell::new(Vec::new());
    f.review(1, &calls, &fake_capture);
    assert!(f.baseline_calls(&calls).is_empty(), "{calls:?}");
    assert_eq!(f.calls(), 1, "the review goes on");
    let report = f.report();
    assert!(
        report.contains("Reviewed by the rules only")
            && report.contains("the target branch \"target\" could not be read"),
        "{report}"
    );
}
