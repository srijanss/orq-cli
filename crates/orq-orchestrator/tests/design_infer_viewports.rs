//! `mode = "infer"`: a package may have a reference for one viewport only and describe the others in
//! words (RESPONSIVE.md). Such a viewport is not asked for: the package freezes with the references it
//! has, the page is still captured at it each round, and the design review judges it by the rules
//! alone. The reviewer is a fake pi script answering from a file; the capture is a stand-in.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    review_design, start_design_package, CaptureOutcome, CaptureRecord, CaptureRun, DesignAgent,
    DesignMode, DesignPackageOutcome, DesignPackageRun, DesignReviewRun, DesignSettings,
    PackageLimits, PageCapture, Viewport,
};
use orq_test_support::{GitFixture, TempDir};
use serde_json::{json, Value};

const FEATURE: &str = "home";
const RUN_ID: &str = "run-infer-viewports";
const RESPONSIVE_RULE: &str = "On tablet the navigation collapses into a menu button.";

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

const SIZES: [(&str, u32, u32); 3] = [
    ("desktop", 400, 300),
    ("tablet", 300, 400),
    ("mobile", 200, 400),
];

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

/// A main checkout with a package holding every document, the tokens and `references/desktop.png`
/// only; the run's logs and a fake reviewer and verifier.
struct Fixture {
    repo: GitFixture,
    worktree: PathBuf,
    logs: TempDir,
    agents: TempDir,
    settings: DesignSettings,
}

impl Fixture {
    fn new(mode: DesignMode) -> Self {
        let repo = GitFixture::new();
        let worktree = repo.add_linked_worktree("infer-viewports");
        let agents = TempDir::new("orq-design-infer-viewports-agents");
        fake_pi(&agents.path().join("reviewer"));
        fake_pi(&agents.path().join("verifier"));
        let f = Fixture {
            repo,
            worktree,
            logs: TempDir::new("orq-design-infer-viewports-logs"),
            agents,
            settings: settings(mode),
        };
        f.put("DESIGN.md", b"# Home\n");
        f.put("UX.md", b"# UX\n");
        f.put(
            "RESPONSIVE.md",
            format!("# Responsive\n\n{RESPONSIVE_RULE}\n").as_bytes(),
        );
        f.put("tokens.css", b":root { --brand: #123456; }\n");
        f.put("references/desktop.png", &png(400, 300));
        f
    }

    fn put(&self, path: &str, bytes: &[u8]) {
        let path = self.repo.root().join("designs").join(FEATURE).join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn package_run(&self) -> DesignPackageRun<'_> {
        DesignPackageRun {
            main_checkout: self.repo.root(),
            feature: FEATURE,
            settings: &self.settings,
            run_logs: self.logs.path(),
            worktree: &self.worktree,
            limits: PackageLimits::default(),
        }
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

    fn prompt(&self, call: u32) -> String {
        fs::read_to_string(
            self.agents
                .path()
                .join("reviewer")
                .join(format!("argv-{call}")),
        )
        .unwrap()
    }

    /// Reviews `round`, the capture recording the viewports it was asked for.
    fn review(&self, round: u32, captured: &RefCell<Vec<Vec<String>>>) {
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
            run_logs: self.logs.path(),
            worktree: self.repo.root(),
            round,
            reviewer: &reviewer,
            verifier: &verifier,
            skill: "the design-review skill",
            schema: "the review schema",
            agent_timeout: Duration::from_secs(30),
        };
        let capture = |run: &CaptureRun| {
            captured
                .borrow_mut()
                .push(run.viewports.iter().map(|v| v.name.clone()).collect());
            fake_capture(run)
        };
        review_design(&run, &capture);
    }

    fn record(&self, round: u32) -> Value {
        let path = self
            .logs
            .path()
            .join(format!("design/round-{round}/review.json"));
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
}

/// One screenshot per viewport asked for, and `capture.json`, as the real capture writes them.
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

fn tablet_issue(id: &str, basis: Value) -> Value {
    let mut issue = json!({
        "id": id, "section": "Navigation", "viewport": "tablet", "source": "RESPONSIVE.md",
        "expected": "a menu button", "actual": "the full navigation",
        "evidence": ["round-1/tablet/index.png"], "severity": "major", "status": "open",
        "fix_direction": "collapse the navigation", "copy": false,
    });
    issue
        .as_object_mut()
        .unwrap()
        .extend(basis.as_object().unwrap().clone());
    issue
}

fn answer(verdict: &str, issues: Vec<Value>) -> Value {
    json!({
        "run_id": "@RUN@", "reference_hash": "@REF@", "capture_revision": "@CAP@",
        "evaluation": "compared the captures", "verdict": verdict,
        "checks": [], "issues": issues, "suggestions": [], "blockers": [],
    })
}

fn needed_paths(outcome: &DesignPackageOutcome) -> Vec<String> {
    match outcome {
        DesignPackageOutcome::FilesNeeded(request) => {
            request.files.iter().map(|file| file.path.clone()).collect()
        }
        other => panic!("expected design files needed, got {other:?}"),
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn an_infer_package_with_the_desktop_reference_only_freezes_and_prepared_mode_asks_for_the_others()
{
    let infer = Fixture::new(DesignMode::Infer);
    match start_design_package(&infer.package_run()) {
        DesignPackageOutcome::Frozen(manifest) => {
            assert_eq!(manifest.missing.viewports, vec!["mobile", "tablet"]);
        }
        other => panic!("an infer package with one reference freezes, got {other:?}"),
    }

    let prepared = Fixture::new(DesignMode::Prepared);
    assert_eq!(
        needed_paths(&start_design_package(&prepared.package_run())),
        vec![
            "designs/home/references/mobile.png",
            "designs/home/references/tablet.png"
        ]
    );
}

#[test]
fn an_infer_package_with_no_reference_still_asks_for_one() {
    let f = Fixture::new(DesignMode::Infer);
    fs::remove_file(f.repo.root().join("designs/home/references/desktop.png")).unwrap();
    let paths = needed_paths(&start_design_package(&f.package_run()));
    assert!(
        paths.contains(&"designs/home/references/desktop.png".to_string()),
        "{paths:?}"
    );
    assert!(
        paths
            .iter()
            .all(|path| path.starts_with("designs/home/references/")),
        "only references are asked for: {paths:?}"
    );
}

#[test]
fn a_rules_only_viewport_is_reviewed_by_the_rules_and_its_reference_issues_are_dropped() {
    let f = Fixture::new(DesignMode::Infer);
    f.answer(&answer(
        "changes_required",
        vec![
            tablet_issue(
                "tablet-layout",
                json!({"basis": "structure", "reference": "references/desktop.png"}),
            ),
            tablet_issue("tablet-shade", json!({"basis": "visual"})),
            tablet_issue(
                "tablet-menu",
                json!({"basis": "rule", "quote": RESPONSIVE_RULE}),
            ),
        ],
    ));
    f.review(1, &RefCell::new(Vec::new()));

    let prompt = f.prompt(1);
    assert!(
        prompt.contains("round-1/tablet/index.png (route /, viewport tablet, rules only)"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Rules only: mobile, tablet."),
        "the prompt names the rules-only viewports: {prompt}"
    );
    assert!(
        prompt.contains("no reference image"),
        "the prompt says there is no image: {prompt}"
    );

    let record = f.record(1);
    let ids: Vec<&str> = record["review"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["tablet-menu"], "{record}");
    assert_eq!(record["verdict"], "changes_required", "{record}");
    assert_eq!(
        record["rules_only"],
        json!(["mobile", "tablet"]),
        "{record}"
    );
    let dropped = record["issues_dropped"].to_string();
    assert!(
        dropped.contains("tablet-layout") && dropped.contains("tablet-shade"),
        "{record}"
    );
    let report = fs::read_to_string(f.logs.path().join("design/report.md")).unwrap();
    assert!(report.contains("rules only"), "{report}");
}

#[test]
fn rules_only_viewports_are_captured_each_round() {
    let f = Fixture::new(DesignMode::Infer);
    f.answer(&answer("pass", vec![]));
    let captured = RefCell::new(Vec::new());
    f.review(1, &captured);
    f.review(2, &captured);
    assert_eq!(
        captured.into_inner(),
        vec![vec!["desktop", "mobile", "tablet"]; 2]
    );
}
