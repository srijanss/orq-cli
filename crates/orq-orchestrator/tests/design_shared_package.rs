//! A design package shared by several features, each building some of its components: the feature's
//! ledger description names the package (`Design package: home`) and its components
//! (`Design components: H02, H03`), and its design review is scoped to them. The reviewer is a fake
//! pi script that answers from a file; the capture is a stand-in that writes one screenshot.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    design_scope, design_scope_note, prepare_design, review_design, start_design_package_scoped,
    CaptureOutcome, CaptureRecord, CaptureRun, DesignAgent, DesignMode, DesignPackageOutcome,
    DesignPackageRun, DesignPrepareOutcome, DesignPreparer, DesignReviewRun, DesignScope,
    DesignSettings, DesignVerdict, PackageLimits, PageCapture, PrepareRequest, ReviewIssue, Viewport,
    DESIGN_FIX_TASK_LIMIT, plan_correction_parts_noted,
};
use orq_test_support::{GitFixture, TempDir};
use serde_json::{json, Value};

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

/// A zlib stream of `n` zero bytes in stored (uncompressed) blocks.
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

/// A black RGB PNG of `width` x `height`.
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

// ---------------------------------------------------------------- fixture

const RUN_ID: &str = "design-shared-package-test";
/// The feature building some of the shared `home` package's components.
const FEATURE: &str = "home-process";
const PACKAGE: &str = "home";

const DESIGN_MD: &str = "# Home\n\n\
## Protected\n\nThe footer is existing and protected.\n\n\
### H01 Header\n\nA 64px header with the logo.\n\n\
### H02 Hero\n\nA full-width hero with one call to action.\n\n\
### H03 Process section\n\nThree steps in a row.\n\n\
### H05 Pricing\n\nThree price cards.\n";

fn settings() -> DesignSettings {
    let viewports: BTreeMap<String, Viewport> = [(
        "desktop".to_string(),
        Viewport {
            width: 400,
            height: 300,
        },
    )]
    .into_iter()
    .collect();
    DesignSettings {
        start_command: Some(vec!["true".into()]),
        ready_path: "/".into(),
        ready_timeout: Duration::from_secs(5),
        max_correction_rounds: 3,
        verify_after_rounds: 2,
        mode: DesignMode::Infer,
        viewports,
    }
}

/// A fake pi: records its argv, answers with `answer.json` of its folder, with `@RUN@`, `@REF@` and
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
             printf '%s' \"$prompt\" > '{d}/prompt-'$n\n\
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

struct Fixture {
    repo: GitFixture,
    logs: TempDir,
    agents: TempDir,
    settings: DesignSettings,
}

impl Fixture {
    /// The shared package `designs/home/`, with a desktop reference and DESIGN.md's components.
    fn new() -> Self {
        let repo = GitFixture::new();
        let package = repo.root().join("designs").join(PACKAGE);
        fs::create_dir_all(package.join("references")).unwrap();
        fs::write(package.join("references/desktop.png"), png(400, 300)).unwrap();
        fs::write(package.join("DESIGN.md"), DESIGN_MD).unwrap();
        let agents = TempDir::new("orq-design-agents");
        fake_pi(&agents.path().join("reviewer"));
        fake_pi(&agents.path().join("verifier"));
        Fixture {
            repo,
            logs: TempDir::new("orq-design-shared-logs"),
            agents,
            settings: settings(),
        }
    }

    fn package_run(&self) -> DesignPackageRun<'_> {
        DesignPackageRun {
            main_checkout: self.repo.root(),
            feature: FEATURE,
            settings: &self.settings,
            run_logs: self.logs.path(),
            worktree: self.repo.root(),
            limits: PackageLimits::default(),
        }
    }

    /// The package checked and frozen for the feature whose ledger description is `description`.
    fn start(&self, description: &str) -> DesignPackageOutcome {
        let scope = design_scope(FEATURE, Some(description)).unwrap();
        start_design_package_scoped(&self.package_run(), scope.as_ref())
    }

    fn agent_dir(&self, role: &str) -> PathBuf {
        self.agents.path().join(role)
    }

    fn answer(&self, role: &str, answer: &Value) {
        fs::write(self.agent_dir(role).join("answer.json"), answer.to_string()).unwrap();
    }

    fn prompt(&self, role: &str, call: u32) -> String {
        fs::read_to_string(self.agent_dir(role).join(format!("prompt-{call}"))).unwrap()
    }

    fn design_dir(&self) -> PathBuf {
        self.logs.path().join("design")
    }

    fn review(&self) -> DesignVerdict {
        let agent = |role: &str| DesignAgent {
            program: self.agent_dir(role).join("pi"),
            harness: "pi".to_string(),
            model: "openai/gpt-6.1-sol".to_string(),
            thinking: Some("medium".to_string()),
        };
        let (reviewer, verifier) = (agent("reviewer"), agent("verifier"));
        let run = DesignReviewRun {
            run_id: RUN_ID,
            main_checkout: self.repo.root(),
            feature: FEATURE,
            settings: &self.settings,
            run_logs: self.logs.path(),
            worktree: self.repo.root(),
            round: 1,
            reviewer: &reviewer,
            verifier: &verifier,
            skill: "the design-review skill",
            schema: "the review schema",
            agent_timeout: Duration::from_secs(30),
        };
        review_design(&run, &fake_capture).verdict
    }

    fn review_record(&self) -> Value {
        serde_json::from_slice(&fs::read(self.design_dir().join("round-1/review.json")).unwrap())
            .unwrap()
    }
}

/// Writes one desktop screenshot and `capture.json` for the round, as the real capture does.
fn fake_capture(run: &CaptureRun) -> CaptureOutcome {
    let round = run
        .run_logs
        .join("design")
        .join(format!("round-{}", run.round));
    fs::create_dir_all(round.join("desktop")).unwrap();
    fs::write(round.join("desktop/index.png"), png(400, 300)).unwrap();
    let record = CaptureRecord {
        round: run.round,
        commit: None,
        browser_version: "fake".to_string(),
        port: 1,
        pages: vec![PageCapture {
            route: "/".to_string(),
            viewport: "desktop".to_string(),
            viewport_width: 400,
            viewport_height: 300,
            image: Some("desktop/index.png".to_string()),
            image_width: Some(400),
            image_height: Some(300),
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

fn issue(id: &str, section: &str, status: &str) -> Value {
    json!({
        "id": id, "section": section, "viewport": "desktop", "source": "references/desktop.png",
        "expected": "as the reference", "actual": "not as the reference",
        "evidence": ["round-1/desktop/index.png"], "severity": "major", "status": status,
        "fix_direction": "match the reference",
        "basis": "structure", "reference": "references/desktop.png", "copy": false,
    })
}

fn answer(verdict: &str, issues: Vec<Value>) -> Value {
    json!({
        "run_id": "@RUN@", "reference_hash": "@REF@", "capture_revision": "@CAP@",
        "evaluation": "compared the desktop capture", "verdict": verdict,
        "checks": [], "issues": issues, "suggestions": [], "blockers": [],
    })
}

fn frozen(outcome: DesignPackageOutcome) {
    assert!(
        matches!(outcome, DesignPackageOutcome::Frozen(_)),
        "{outcome:?}"
    );
}

// ---------------------------------------------------------------- tests

#[test]
fn a_design_package_line_reads_the_named_package_and_the_frozen_copy_records_the_scope() {
    let f = Fixture::new();

    frozen(f.start("Build the process section.\nDesign package: home\nDesign components: H03\n"));

    let package = f.design_dir().join("package");
    assert_eq!(
        fs::read_to_string(package.join("DESIGN.md")).unwrap(),
        DESIGN_MD
    );
    let scope: DesignScope =
        serde_json::from_slice(&fs::read(f.design_dir().join("scope.json")).unwrap()).unwrap();
    assert_eq!(scope.feature, FEATURE);
    assert_eq!(scope.package, PACKAGE);
    assert_eq!(scope.components, Some(vec!["H03".to_string()]));
}

#[test]
fn the_review_prompt_quotes_the_components_sections_as_its_scope_and_lists_the_done_ones() {
    let f = Fixture::new();
    frozen(
        f.start("Design package: home\nDesign components: H02, H03\nDesign components done: H01\n"),
    );
    f.answer("reviewer", &answer("pass", vec![]));

    assert_eq!(f.review(), DesignVerdict::Pass);

    let prompt = f.prompt("reviewer", 1);
    let at = prompt
        .find("Scope: components H02, H03")
        .unwrap_or_else(|| panic!("{prompt}"));
    let end = at
        + prompt[at..]
            .find(">>>")
            .unwrap_or_else(|| panic!("{prompt}"));
    let scope = &prompt[at..end];
    assert!(
        scope.contains("### H02 Hero\n\nA full-width hero with one call to action."),
        "{prompt}"
    );
    assert!(
        scope.contains("### H03 Process section\n\nThree steps in a row."),
        "{prompt}"
    );
    assert!(!scope.contains("H05 Pricing"), "{prompt}");
    assert!(!scope.contains("H01 Header"), "{prompt}");
    assert!(prompt.contains("Design components done: H01"), "{prompt}");
}

#[test]
fn a_component_id_design_md_lacks_is_refused_naming_the_ids_it_has() {
    let f = Fixture::new();

    let outcome = f.start("Design package: home\nDesign components: H02, H04\n");

    let DesignPackageOutcome::Refused(problems) = outcome else {
        panic!("{outcome:?}");
    };
    let problem = problems.join("; ");
    assert!(problem.contains("H04"), "{problem}");
    assert!(problem.contains("H01, H02, H03, H05"), "{problem}");
    assert!(!f.design_dir().join("manifest.json").exists());
}

#[test]
fn an_issue_for_another_component_is_dropped_and_a_regression_of_a_done_one_kept() {
    let f = Fixture::new();
    frozen(
        f.start("Design package: home\nDesign components: H02, H03\nDesign components done: H01\n"),
    );
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![
                issue("pricing-missing", "H05 Pricing", "open"),
                issue("header-shorter", "H01 Header", "regressed"),
                issue("hero-button", "H02 Hero", "open"),
                issue("no-component", "Somewhere", "open"),
            ],
        ),
    );

    assert_eq!(f.review(), DesignVerdict::ChangesRequired);

    let record = f.review_record();
    let ids: Vec<&str> = record["review"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["header-shorter", "hero-button"]);
    let dropped = record["issues_dropped"].as_array().unwrap();
    assert_eq!(dropped.len(), 2, "{dropped:?}");
    assert!(dropped[0].as_str().unwrap().contains("H05"), "{dropped:?}");
}

#[test]
fn all_components_review_the_whole_page_as_before() {
    let f = Fixture::new();
    frozen(f.start("Design package: home\nDesign components: all\n"));
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![issue("pricing-missing", "H05 Pricing", "open")],
        ),
    );

    assert_eq!(f.review(), DesignVerdict::ChangesRequired);

    assert!(!f.prompt("reviewer", 1).contains("Scope: components"));
    let record = f.review_record();
    assert_eq!(record["review"]["issues"].as_array().unwrap().len(), 1);
    assert_eq!(record["issues_dropped"], json!([]));
}

#[test]
fn the_fix_task_and_the_code_review_are_told_the_same_scope() {
    let scope = design_scope(
        FEATURE,
        Some("Design package: home\nDesign components: H02, H03\nDesign components done: H01\n"),
    )
    .unwrap()
    .unwrap();

    let note = design_scope_note(&scope).unwrap();

    assert!(note.contains("H02, H03"), "{note}");
    assert!(note.contains("designs/home/"), "{note}");
    assert!(note.contains("done") && note.contains("H01"), "{note}");
    let whole = design_scope(
        FEATURE,
        Some("Design package: home\nDesign components: all\n"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(design_scope_note(&whole), None);
}

#[test]
fn without_the_lines_the_package_is_the_features_own_folder() {
    let f = Fixture::new();
    assert_eq!(
        design_scope(FEATURE, Some("Build the process section.\n")).unwrap(),
        None
    );

    let outcome = f.start("Build the process section.\n");

    let DesignPackageOutcome::FilesNeeded(needed) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        needed
            .files
            .iter()
            .all(|file| file.path.starts_with(&format!("designs/{FEATURE}/"))),
        "{needed:?}"
    );
    assert!(!f.design_dir().join("scope.json").exists());
}

/// An open issue of `section` whose long fields are each about `long` characters.
fn long_issue(id: &str, section: &str, long: usize) -> ReviewIssue {
    let mut value = issue(id, section, "open");
    for field in ["expected", "actual", "fix_direction"] {
        value[field] = json!(format!("{field} {}", "x".repeat(long)));
    }
    serde_json::from_value(value).unwrap()
}

#[test]
fn every_correction_part_with_the_scope_note_still_fits_the_task_limit() {
    let scope = design_scope(
        FEATURE,
        Some("Design package: home\nDesign components: H02, H03\nDesign components done: H01\n"),
    )
    .unwrap()
    .unwrap();
    let note = design_scope_note(&scope).unwrap();
    let report = Path::new("design/round-1/report.md");
    // Rounds whose plain task is just under the limit, and rounds that are split into parts.
    for long in [100, 300, 380, 400, 420, 600, 900] {
        let issues: Vec<ReviewIssue> = (0..3)
            .map(|n| long_issue(&format!("i{n}"), &format!("H0{} Section", n + 2), long))
            .collect();
        let parts = plan_correction_parts_noted(1, 3, &issues, &[], report, &note).unwrap();
        for part in &parts {
            assert!(
                part.task.chars().count() <= DESIGN_FIX_TASK_LIMIT,
                "long {long}: part {} is {} characters",
                part.part,
                part.task.chars().count()
            );
            assert!(part.task.ends_with(&note), "long {long}: {}", part.task);
        }
    }
}

/// A preparer that asks for one asset, as an infer-mode run's preparer does when the package lacks it.
struct AsksForALogo;

impl DesignPreparer for AsksForALogo {
    fn prepare(&mut self, _request: &PrepareRequest) -> Result<String, String> {
        Ok(json!({
            "conflicts": [], "inferred": [],
            "needed_files": [{"path": "assets/logo.png", "what": "the logo", "why": "the header shows it"}],
        })
        .to_string())
    }
}

#[test]
fn the_design_preparer_of_a_shared_package_reads_and_asks_for_files_in_the_shared_folder() {
    let f = Fixture::new();
    let DesignPackageOutcome::Frozen(manifest) =
        f.start("Build the process section.\nDesign package: home\nDesign components: H03\n")
    else {
        panic!("the shared package is not frozen");
    };

    let outcome = prepare_design(&f.package_run(), &manifest, &mut AsksForALogo);

    let DesignPrepareOutcome::FilesNeeded(needed) = outcome else {
        panic!("{outcome:?}");
    };
    let paths: Vec<&str> = needed.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, vec![format!("designs/{PACKAGE}/assets/logo.png")]);
}
