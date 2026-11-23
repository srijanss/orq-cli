//! `orq design review`: the page as it is, reviewed by a read-only design reviewer, with no
//! implementer, commit or push. The reviewer and the verifier are fake pi scripts that answer from
//! a file; the capture is a stand-in that writes one screenshot, so no browser is needed.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    review_design, start_design_package, CaptureOutcome, CaptureRecord, CaptureRun, DesignAgent,
    DesignMode, DesignPackageOutcome, DesignPackageRun, DesignReviewRun, DesignSettings,
    DesignVerdict, PackageLimits, PageCapture, Viewport,
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

const RUN_ID: &str = "design-review-test";
const FEATURE: &str = "home";

fn settings(verify_after_rounds: u8) -> DesignSettings {
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
        verify_after_rounds,
        mode: DesignMode::Infer,
        viewports,
    }
}

/// A fake pi: records its argv, answers with `answer-<n>.json` (or `answer.json`) of its folder,
/// with `@RUN@`, `@REF@` and `@CAP@` replaced by what the prompt names, then runs `after-<n>.sh`
/// (or `after.sh`) in the worktree when there is one.
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
             f='{d}/answer-'$n.json; [ -f \"$f\" ] || f='{d}/answer.json'\n\
             sed -e \"s|@RUN@|$run|g\" -e \"s|@REF@|$ref|g\" -e \"s|@CAP@|$cap|g\" \"$f\"\n\
             s='{d}/after-'$n.sh; [ -f \"$s\" ] || s='{d}/after.sh'\n\
             [ -f \"$s\" ] && sh \"$s\"\n\
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
    fn new() -> Self {
        Self::with_rounds(2)
    }

    fn with_rounds(verify_after_rounds: u8) -> Self {
        let repo = GitFixture::new();
        let package = repo.root().join("designs").join(FEATURE);
        fs::create_dir_all(package.join("references")).unwrap();
        fs::write(package.join("references/desktop.png"), png(400, 300)).unwrap();
        fs::write(package.join("DESIGN.md"), "# Home\n").unwrap();
        let agents = TempDir::new("orq-design-agents");
        fake_pi(&agents.path().join("reviewer"));
        fake_pi(&agents.path().join("verifier"));
        Fixture {
            repo,
            logs: TempDir::new("orq-design-review-logs"),
            agents,
            settings: settings(verify_after_rounds),
        }
    }

    fn agent_dir(&self, role: &str) -> PathBuf {
        self.agents.path().join(role)
    }

    /// What the fake `role` answers, every attempt.
    fn answer(&self, role: &str, answer: &Value) {
        fs::write(self.agent_dir(role).join("answer.json"), answer.to_string()).unwrap();
    }

    fn answer_text(&self, role: &str, attempt: u32, text: &str) {
        fs::write(
            self.agent_dir(role).join(format!("answer-{attempt}.json")),
            text,
        )
        .unwrap();
    }

    fn calls(&self, role: &str) -> u32 {
        fs::read_to_string(self.agent_dir(role).join("count"))
            .map(|n| n.trim().parse().unwrap())
            .unwrap_or(0)
    }

    fn argv(&self, role: &str, call: u32) -> Vec<String> {
        fs::read_to_string(self.agent_dir(role).join(format!("argv-{call}")))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn design_dir(&self) -> PathBuf {
        self.logs.path().join("design")
    }

    fn review(&self, round: u32) -> DesignVerdict {
        self.review_with(round, Duration::from_secs(30))
    }

    fn review_with(&self, round: u32, agent_timeout: Duration) -> DesignVerdict {
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
            round,
            reviewer: &reviewer,
            verifier: &verifier,
            skill: "the design-review skill",
            schema: "the review schema",
            agent_timeout,
        };
        review_design(&run, &fake_capture).verdict
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
    // What a test wants measured: `measured.json` of the run's logs, copied as the desktop's measurements.
    if let Ok(measured) = fs::read(run.run_logs.join("measured.json")) {
        fs::write(round.join("desktop/measurements.json"), measured).unwrap();
    }
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

fn check(id: &str, status: &str) -> Value {
    json!({"id": id, "required": true, "status": status, "evidence": ["round-1/desktop/index.png"]})
}

fn issue(id: &str, severity: &str, status: &str) -> Value {
    json!({
        "id": id, "section": "Header", "viewport": "desktop", "source": "references/desktop.png",
        "expected": "a 64px header", "actual": "a 48px header",
        "evidence": ["round-1/desktop/index.png"], "severity": severity, "status": status,
        "fix_direction": "make the header taller",
    })
}

fn answer(verdict: &str, checks: Vec<Value>, issues: Vec<Value>) -> Value {
    json!({
        "run_id": "@RUN@", "reference_hash": "@REF@", "capture_revision": "@CAP@",
        "evaluation": "compared the desktop capture", "verdict": verdict,
        "checks": checks, "issues": issues, "suggestions": [], "blockers": [],
    })
}

// ---------------------------------------------------------------- tests

#[test]
fn a_pass_answer_is_reported_as_pass() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );

    assert_eq!(f.review(1), DesignVerdict::Pass);

    let written: Value =
        serde_json::from_slice(&fs::read(f.design_dir().join("round-1/review.json")).unwrap())
            .unwrap();
    assert_eq!(written["verdict"], "pass");
    let report = fs::read_to_string(f.design_dir().join("report.md")).unwrap();
    assert!(report.contains("pass"), "{report}");
    assert_eq!(f.calls("verifier"), 0);
}

#[test]
fn a_reviewer_cannot_downgrade_a_violated_token_rule_or_pass_over_it() {
    let f = Fixture::new();
    let package = f.repo.root().join("designs").join(FEATURE);
    fs::write(package.join("tokens.css"), ":root { --weight-bold: 700; }\n").unwrap();
    fs::write(package.join("DESIGN.md"), "# Home\n\n## Token rules\n\nh1: font-weight --weight-bold\n").unwrap();
    let h1 = json!({"tag": "h1", "index": 0, "box": {"x": 0, "y": 0, "width": 100, "height": 40},
                    "styles": {"font-weight": "600"}});
    let measured = json!({"viewport": "desktop", "width": 400, "height": 300, "routes": {"/": {
        "document": {"width": 400, "height": 300, "root_font_size": 16}, "elements": [h1]}}});
    fs::write(f.logs.path().join("measured.json"), measured.to_string()).unwrap();
    let mut downgraded = issue("tokens-desktop-h1-font-weight", "minor", "open");
    downgraded["expected"] = json!("weight 700, near enough");
    f.answer("reviewer", &answer("pass", vec![check("layout", "pass")], vec![downgraded]));

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);

    let written: Value =
        serde_json::from_slice(&fs::read(f.design_dir().join("round-1/review.json")).unwrap()).unwrap();
    let issues = written["review"]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "{written:#}");
    assert_eq!(issues[0]["id"], "tokens-desktop-h1-font-weight");
    assert_eq!(issues[0]["source"], "tokens");
    assert_eq!(issues[0]["severity"], "major");
    assert_eq!(issues[0]["confidence"], "high");
    let check: Value =
        serde_json::from_slice(&fs::read(f.design_dir().join("round-1/token-check.json")).unwrap()).unwrap();
    assert_eq!(check["status"], "fail");
    let prompt = f.argv("reviewer", 1).join("\n");
    assert!(prompt.contains("round-1/token-check.json"), "{prompt}");
}

#[test]
fn a_reviewer_pass_cannot_pass_token_rules_the_check_could_not_assess() {
    let bold_h1 = json!({"tag": "h1", "index": 0, "box": {"x": 0, "y": 0, "width": 100, "height": 40},
                         "styles": {"font-weight": "700"}});
    let page = |extra: Value| {
        let mut page = json!({"document": {"width": 400, "height": 300, "root_font_size": 16}, "elements": [bold_h1]});
        if let (Some(page), Some(extra)) = (page.as_object_mut(), extra.as_object()) {
            page.extend(extra.clone());
        }
        json!({"viewport": "desktop", "width": 400, "height": 300, "routes": {"/": page}})
    };
    let truncated = json!({"selectors": {"h1": [bold_h1]}, "selector_counts": {"h1": 2}});
    let cases = [
        ("an unresolved token", "h1: font-weight --weight-bodl", Some(page(json!({})))),
        ("no measurements", "h1: font-weight --weight-bold", None),
        ("a selector matched beyond the capture", "h1: font-weight --weight-bold", Some(page(truncated))),
    ];
    for (case, rule, measured) in cases {
        let f = Fixture::new();
        let package = f.repo.root().join("designs").join(FEATURE);
        fs::write(package.join("tokens.css"), ":root { --weight-bold: 700; }\n").unwrap();
        fs::write(package.join("DESIGN.md"), format!("# Home\n\n## Token rules\n\n{rule}\n")).unwrap();
        if let Some(measured) = measured {
            fs::write(f.logs.path().join("measured.json"), measured.to_string()).unwrap();
        }
        f.answer("reviewer", &answer("pass", vec![check("layout", "pass")], vec![]));

        assert_eq!(f.review(1), DesignVerdict::ChangesRequired, "{case}");

        let written: Value =
            serde_json::from_slice(&fs::read(f.design_dir().join("round-1/review.json")).unwrap()).unwrap();
        let issues = written["review"]["issues"].as_array().unwrap();
        let incomplete = issues.iter().find(|i| i["id"] == "tokens-not-assessed").unwrap_or_else(|| {
            panic!("{case}: {written:#}")
        });
        assert_eq!(incomplete["source"], "tokens", "{case}");
        assert_eq!(incomplete["severity"], "major", "{case}");
        let check: Value =
            serde_json::from_slice(&fs::read(f.design_dir().join("round-1/token-check.json")).unwrap()).unwrap();
        assert_eq!(check["status"], "not_assessed", "{case}");
        assert!(incomplete["actual"].as_str().unwrap().contains(check["reason"].as_str().unwrap()), "{case}");
    }

    // Tokens without any rule, measured or not: nothing configured is left unchecked.
    let f = Fixture::new();
    let package = f.repo.root().join("designs").join(FEATURE);
    fs::write(package.join("tokens.css"), ":root { --weight-bold: 700; }\n").unwrap();
    f.answer("reviewer", &answer("pass", vec![check("layout", "pass")], vec![]));
    assert_eq!(f.review(1), DesignVerdict::Pass);
}

#[test]
fn a_token_violation_open_for_verify_after_rounds_rounds_goes_to_the_verifier() {
    let f = Fixture::new();
    let package = f.repo.root().join("designs").join(FEATURE);
    fs::write(package.join("tokens.css"), ":root { --weight-bold: 700; }\n").unwrap();
    fs::write(package.join("DESIGN.md"), "# Home\n\n## Token rules\n\nh1: font-weight --weight-bold\n").unwrap();
    let h1 = json!({"tag": "h1", "index": 0, "box": {"x": 0, "y": 0, "width": 100, "height": 40},
                    "styles": {"font-weight": "600"}});
    let measured = json!({"viewport": "desktop", "width": 400, "height": 300, "routes": {"/": {
        "document": {"width": 400, "height": 300, "root_font_size": 16}, "elements": [h1]}}});
    fs::write(f.logs.path().join("measured.json"), measured.to_string()).unwrap();
    f.answer("reviewer", &answer("pass", vec![check("layout", "pass")], vec![]));
    f.answer("verifier", &answer("pass", vec![check("layout", "pass")], vec![]));

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    assert_eq!(f.calls("verifier"), 0);
    assert_eq!(f.review(2), DesignVerdict::ChangesRequired);

    assert_eq!(f.calls("verifier"), 1, "the token issue has been open for two rounds");
    let written: Value =
        serde_json::from_slice(&fs::read(f.design_dir().join("round-2/review.json")).unwrap()).unwrap();
    let issues = written["review"]["issues"].as_array().unwrap();
    assert!(issues.iter().any(|i| i["id"] == "tokens-desktop-h1-font-weight" && i["severity"] == "major"), "{written:#}");
}

#[test]
fn the_reviewer_runs_read_only_with_the_references_and_captures_attached() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    f.review(1);

    let argv = f.argv("reviewer", 1);
    let tools = argv.iter().position(|a| a == "--tools").expect("--tools");
    assert_eq!(argv[tools + 1], "read", "{argv:?}");
    let reference = format!(
        "@{}",
        f.design_dir()
            .join("package/references/desktop.png")
            .display()
    );
    let capture = format!(
        "@{}",
        f.design_dir().join("round-1/desktop/index.png").display()
    );
    assert!(
        argv.contains(&reference) && argv.contains(&capture),
        "{argv:?}"
    );
    assert!(
        argv.iter().any(|a| a.contains("the design-review skill")),
        "{argv:?}"
    );
}

#[test]
fn the_reviewer_loads_no_extensions_mcp_servers_or_project_context() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    f.review(1);

    let argv = f.argv("reviewer", 1);
    for flag in [
        "--no-extensions",
        "--no-mcp",
        "--no-context-files",
        "--no-session",
    ] {
        assert!(argv.iter().any(|a| a == flag), "{flag} missing: {argv:?}");
    }
}

#[test]
fn a_changes_required_answer_is_reported_as_changes_required() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![check("layout", "fail")],
            vec![issue("header-height", "major", "open")],
        ),
    );

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    let report = fs::read_to_string(f.design_dir().join("report.md")).unwrap();
    assert!(
        report.contains("### Header") && report.contains("header-height"),
        "{report}"
    );
}

#[test]
fn a_blocked_answer_is_reported_as_blocked() {
    let f = Fixture::new();
    let mut blocked = answer("blocked", vec![check("layout", "not_assessed")], vec![]);
    blocked["blockers"] = json!(["the capture shows a spinner"]);
    f.answer("reviewer", &blocked);
    f.answer("verifier", &blocked);

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    let report = fs::read_to_string(f.design_dir().join("report.md")).unwrap();
    assert!(
        report.contains("## Not assessed") && report.contains("- layout"),
        "{report}"
    );
}

/// The answer is invalid every time: the round is blocked after one retry, never a pass.
fn assert_invalid(answer: &str) {
    let f = Fixture::new();
    f.answer_text("reviewer", 1, answer);
    f.answer_text("reviewer", 2, answer);

    assert_eq!(f.review(1), DesignVerdict::Blocked, "{answer}");
    assert_eq!(f.calls("reviewer"), 2, "one retry");
    let written: Value =
        serde_json::from_slice(&fs::read(f.design_dir().join("round-1/review.json")).unwrap())
            .unwrap();
    assert_eq!(written["verdict"], "blocked");
    assert!(
        written["reason"].as_str().unwrap().contains("invalid"),
        "{written}"
    );
}

#[test]
fn malformed_json_is_invalid() {
    assert_invalid("{\"verdict\": \"pass\"");
}

#[test]
fn a_pass_with_an_open_major_issue_is_invalid() {
    assert_invalid(
        &answer(
            "pass",
            vec![check("layout", "pass")],
            vec![issue("header-height", "major", "open")],
        )
        .to_string(),
    );
}

#[test]
fn a_pass_with_a_failed_required_check_is_invalid() {
    assert_invalid(&answer("pass", vec![check("layout", "fail")], vec![]).to_string());
}

#[test]
fn an_answer_with_a_blocker_that_is_not_blocked_is_invalid() {
    let mut passing = answer("pass", vec![check("layout", "pass")], vec![]);
    passing["blockers"] = json!(["the capture shows an error overlay"]);
    assert_invalid(&passing.to_string());
}

#[test]
fn a_required_check_not_assessed_without_blocked_is_invalid() {
    assert_invalid(
        &answer(
            "changes_required",
            vec![check("layout", "not_assessed")],
            vec![],
        )
        .to_string(),
    );
}

#[test]
fn evidence_outside_the_design_folder_is_invalid() {
    let mut outside = answer("pass", vec![check("layout", "pass")], vec![]);
    outside["checks"][0]["evidence"] = json!(["../../etc/passwd"]);
    assert_invalid(&outside.to_string());
}

#[test]
fn evidence_naming_no_file_is_invalid() {
    let mut missing = answer("pass", vec![check("layout", "pass")], vec![]);
    missing["checks"][0]["evidence"] = json!(["round-1/desktop/nothing.png"]);
    assert_invalid(&missing.to_string());
}

#[test]
fn a_mismatched_capture_revision_is_invalid() {
    let mut other = answer("pass", vec![check("layout", "pass")], vec![]);
    other["capture_revision"] = json!("0000");
    assert_invalid(&other.to_string());
}

#[test]
fn another_run_or_reference_hash_is_invalid() {
    let mut other = answer("pass", vec![check("layout", "pass")], vec![]);
    other["run_id"] = json!("another-run");
    assert_invalid(&other.to_string());
    let mut other = answer("pass", vec![check("layout", "pass")], vec![]);
    other["reference_hash"] = json!("0000");
    assert_invalid(&other.to_string());
}

fn after(f: &Fixture, role: &str, script: &str) {
    fs::write(f.agent_dir(role).join("after.sh"), script).unwrap();
}

fn reason(f: &Fixture) -> String {
    let written: Value =
        serde_json::from_slice(&fs::read(f.design_dir().join("round-1/review.json")).unwrap())
            .unwrap();
    written["reason"].as_str().unwrap_or_default().to_string()
}

#[test]
fn the_first_review_checks_the_package_frozen_before_the_implementer_instead_of_freezing_it_again() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    let package_run = DesignPackageRun {
        main_checkout: f.repo.root(),
        feature: FEATURE,
        settings: &f.settings,
        run_logs: f.logs.path(),
        worktree: f.repo.root(),
        limits: PackageLimits::default(),
    };
    assert!(matches!(
        start_design_package(&package_run),
        DesignPackageOutcome::Frozen(_)
    ));
    // The reference changes while the implementer works.
    fs::write(
        f.repo.root().join("designs").join(FEATURE).join("references/desktop.png"),
        png(400, 301),
    )
    .unwrap();

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert!(
        reason(&f).contains("the design package changed since this run started"),
        "{}",
        reason(&f)
    );
    assert_eq!(f.calls("reviewer"), 0, "the page is never reviewed against the new references");
}

#[test]
fn a_reviewer_that_edits_a_worktree_file_fails_the_review() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    after(&f, "reviewer", "echo changed >> README.md\n");

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert!(
        reason(&f).contains("changed the worktree"),
        "{}",
        reason(&f)
    );
}

#[test]
fn a_reviewer_that_makes_a_file_executable_fails_the_review() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    after(&f, "reviewer", "chmod +x README.md\n");

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert!(
        reason(&f).contains("changed the worktree"),
        "{}",
        reason(&f)
    );
}

#[test]
fn a_reviewer_that_adds_a_file_fails_the_review() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    after(&f, "reviewer", "echo new > added.txt\n");

    assert_eq!(f.review(1), DesignVerdict::Blocked);
}

#[test]
fn a_reviewer_that_edits_the_frozen_package_fails_the_review() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    after(
        &f,
        "reviewer",
        &format!(
            "echo changed >> '{}'\n",
            f.design_dir().join("package/DESIGN.md").display()
        ),
    );

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert!(
        reason(&f).contains("changed the frozen design package"),
        "{}",
        reason(&f)
    );
}

#[test]
fn a_reviewer_that_commits_fails_the_review() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );
    after(
        &f,
        "reviewer",
        "git -c user.name=x -c user.email=x@example.invalid commit -q --allow-empty -m sneaky\n",
    );

    assert_eq!(f.review(1), DesignVerdict::Blocked);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_review_makes_no_commit_branch_or_push() {
    let f = Fixture::new();
    let remote = f.repo.add_bare_remote();
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![check("layout", "fail")],
            vec![issue("header-height", "major", "open")],
        ),
    );
    let refs = git(f.repo.root(), &["for-each-ref"]);
    let head = git(f.repo.root(), &["rev-parse", "HEAD"]);
    let status = git(
        f.repo.root(),
        &["status", "--porcelain", "--untracked-files=no"],
    );

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);

    assert_eq!(git(f.repo.root(), &["for-each-ref"]), refs);
    assert_eq!(git(f.repo.root(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(
            f.repo.root(),
            &["status", "--porcelain", "--untracked-files=no"]
        ),
        status
    );
    assert_eq!(git(&remote, &["for-each-ref"]), "", "nothing was pushed");
}

fn decided_by(f: &Fixture, round: u32) -> String {
    let written: Value = serde_json::from_slice(
        &fs::read(f.design_dir().join(format!("round-{round}/review.json"))).unwrap(),
    )
    .unwrap();
    written["decided_by"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_blocked_answer_goes_to_the_verifier_whose_verdict_is_used() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer("blocked", vec![check("layout", "not_assessed")], vec![]),
    );
    f.answer(
        "verifier",
        &answer(
            "changes_required",
            vec![check("layout", "fail")],
            vec![issue("header-height", "major", "open")],
        ),
    );

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    assert_eq!(f.calls("verifier"), 1);
    assert_eq!(decided_by(&f, 1), "design verifier");
    let report = fs::read_to_string(f.design_dir().join("report.md")).unwrap();
    assert!(report.contains("design verifier"), "{report}");
}

#[test]
fn a_low_confidence_review_goes_to_the_verifier() {
    let f = Fixture::new();
    let mut unsure = answer("pass", vec![check("layout", "pass")], vec![]);
    unsure["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "confidence", "required": false, "status": "fail", "evidence": []}));
    f.answer("reviewer", &unsure);
    f.answer(
        "verifier",
        &answer(
            "changes_required",
            vec![check("layout", "fail")],
            vec![issue("header-height", "major", "open")],
        ),
    );

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    assert_eq!(f.calls("verifier"), 1);
    let argv = f.argv("verifier", 1);
    assert!(
        argv.iter().any(|a| a.contains("design verifier")),
        "{argv:?}"
    );
}

#[test]
fn an_issue_open_for_verify_after_rounds_rounds_goes_to_the_verifier() {
    let f = Fixture::with_rounds(2);
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![check("layout", "fail")],
            vec![issue("header-height", "major", "open")],
        ),
    );
    f.answer(
        "verifier",
        &answer(
            "pass",
            vec![check("layout", "pass")],
            vec![issue("header-height", "major", "resolved")],
        ),
    );

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    assert_eq!(f.calls("verifier"), 0, "open for one round only");

    assert_eq!(f.review(2), DesignVerdict::Pass);
    assert_eq!(f.calls("verifier"), 1);
    assert_eq!(decided_by(&f, 2), "design verifier");
    let prompt = f.argv("reviewer", 2).join("\n");
    assert!(
        prompt.contains("header-height"),
        "the previous round's issues are given: {prompt}"
    );
}

#[test]
fn an_invalid_answer_is_retried_once() {
    let f = Fixture::new();
    f.answer_text("reviewer", 1, "not json");
    f.answer(
        "reviewer",
        &answer("pass", vec![check("layout", "pass")], vec![]),
    );

    assert_eq!(f.review(1), DesignVerdict::Pass);
    assert_eq!(f.calls("reviewer"), 2);
    let retry = f.argv("reviewer", 2);
    assert!(
        retry.iter().any(|line| line.contains("not accepted")),
        "{retry:?}"
    );
}

// ---------------------------------------------------------------- agents that fail

/// Replaces the fake `role` with a script of `body`.
fn agent_script(f: &Fixture, role: &str, body: &str) {
    let script = f.agent_dir(role).join("pi");
    fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Whether `pid` is a running process: a zombie, killed but not yet reaped by whoever adopted
/// it, is not running. Its state is the field after the `(comm)` of `/proc/<pid>/stat`.
fn process_is_alive(pid: &str) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next());
    !matches!(state, None | Some("Z") | Some("X"))
}

#[test]
fn a_hanging_reviewer_is_stopped_with_what_it_started_and_blocks_the_review() {
    let f = Fixture::new();
    let pid_file = f.agent_dir("reviewer").join("child.pid");
    agent_script(
        &f,
        "reviewer",
        &format!("sleep 300 &\necho $! > '{}'\nwait\n", pid_file.display()),
    );

    let started = std::time::Instant::now();
    assert_eq!(
        f.review_with(1, Duration::from_secs(1)),
        DesignVerdict::Blocked
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(reason(&f).contains("did not answer"), "{}", reason(&f));

    let pid = fs::read_to_string(&pid_file).unwrap().trim().to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while process_is_alive(&pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !process_is_alive(&pid),
        "the reviewer's child {pid} is still running"
    );
}

#[test]
fn a_reviewer_that_exits_nonzero_blocks_the_review() {
    let f = Fixture::new();
    agent_script(&f, "reviewer", "echo '{}'\nexit 3\n");

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert!(
        reason(&f).contains("the design reviewer failed"),
        "{}",
        reason(&f)
    );
}

#[test]
fn a_reviewer_that_cannot_start_blocks_the_review() {
    let f = Fixture::new();
    fs::remove_file(f.agent_dir("reviewer").join("pi")).unwrap();

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert!(reason(&f).contains("could not start"), "{}", reason(&f));
}

#[test]
fn an_answer_over_the_size_limit_blocks_the_review() {
    let f = Fixture::new();
    agent_script(&f, "reviewer", "head -c 5000000 /dev/zero | tr '\\0' a\n");

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert!(reason(&f).contains("bytes"), "{}", reason(&f));
}
