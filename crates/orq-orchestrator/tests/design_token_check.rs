//! The design token check: a deterministic step, no agent, after every capture. The page's measured
//! styles are compared with the package's tokens through the `## Token rules` of DESIGN.md, and the
//! result is written to `design/round-<n>/token-check.json`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    check_design_tokens, review_design, CaptureOutcome, CaptureRecord, CaptureRun, DesignAgent, DesignMode,
    DesignReviewRun, DesignSettings, DesignVerdict, IssueStatus, PageCapture, Severity, TokenCheckStatus, Viewport,
    token_rule_selectors,
};
use orq_test_support::{GitFixture, TempDir};
use serde_json::{json, Value};

/// A run's design folder: the frozen package and one captured round.
struct Fixture {
    _dir: TempDir,
    design: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = TempDir::new("design-token-check");
        let design = dir.path().join("design");
        std::fs::create_dir_all(design.join("package")).unwrap();
        Fixture { _dir: dir, design }
    }

    fn package(&self, name: &str, text: &str) -> &Fixture {
        let path = self.design.join("package").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        self
    }

    /// `measurements.json` of `viewport` in round 1, with one route `/`.
    fn measured(&self, viewport: &str, page: Value) -> &Fixture {
        let folder = self.design.join("round-1").join(viewport);
        std::fs::create_dir_all(&folder).unwrap();
        let file = json!({"viewport": viewport, "width": 1280, "height": 800, "routes": {"/": page}});
        std::fs::write(folder.join("measurements.json"), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        self
    }

    fn written(&self) -> Value {
        let bytes = std::fs::read(self.design.join("round-1").join("token-check.json")).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn dir(&self) -> &Path {
        &self.design
    }
}

/// One landmark as the capture measures it.
fn element(tag: &str, styles: Value) -> Value {
    json!({"tag": tag, "index": 0, "box": {"x": 0, "y": 0, "width": 100, "height": 40}, "styles": styles})
}

fn page(elements: Vec<Value>, selectors: Value) -> Value {
    json!({
        "document": {"width": 1280, "height": 800, "root_font_size": 16},
        "elements": elements,
        "selectors": selectors,
    })
}

const TOKENS: &str = ":root {\n  --font-title: 2rem;\n  --weight-bold: 700;\n  --radius-none: 0;\n  --ink: #111111;\n}\n";

const DESIGN: &str = "# Checkout\n\n## Token rules\n\nh1: font-size --font-title; font-weight --weight-bold; color --ink\n";

#[test]
fn a_heading_at_weight_600_against_weight_bold_700_fails_and_becomes_a_major_tokens_issue() {
    let f = Fixture::new();
    f.package("tokens.css", TOKENS).package("DESIGN.md", DESIGN).measured(
        "desktop",
        page(
            vec![element("h1", json!({"font-size": "32px", "font-weight": "600", "color": "rgb(17, 17, 17)"}))],
            json!({}),
        ),
    );

    let check = check_design_tokens(f.dir(), 1);

    assert_eq!(check.status, TokenCheckStatus::Fail);
    let issues = check.issues();
    assert_eq!(issues.len(), 1, "{issues:?}");
    let issue = &issues[0];
    assert_eq!(issue.source, "tokens");
    assert_eq!(issue.severity, Severity::Major);
    assert_eq!(issue.status, IssueStatus::Open);
    assert_eq!(issue.confidence.as_deref(), Some("high"));
    assert_eq!(issue.viewport, "desktop");
    assert_eq!(issue.selector.as_deref(), Some("h1"));
    assert!(issue.expected.contains("--weight-bold") && issue.expected.contains("700"), "{}", issue.expected);
    assert!(issue.actual.contains("600"), "{}", issue.actual);

    let written = f.written();
    assert_eq!(written["status"], "fail");
    let failed: Vec<&Value> = written["rules"].as_array().unwrap().iter().filter(|r| r["status"] == "fail").collect();
    assert_eq!(failed.len(), 1, "{written:#}");
    assert_eq!(failed[0]["selector"], "h1");
    assert_eq!(failed[0]["viewport"], "desktop");
    assert_eq!(failed[0]["property"], "font-weight");
    assert_eq!(failed[0]["token"], "--weight-bold");
    assert_eq!(failed[0]["expected"], "700");
    assert_eq!(failed[0]["measured"], "600");
}

#[test]
fn the_selectors_the_token_rules_name_are_the_ones_the_capture_measures_each_once() {
    let f = Fixture::new();
    f.package(
        "DESIGN.md",
        "# Checkout\n\n## Token rules\n\n- `h1: font-size --font-title`\n- .feature-state.state-pending: color --pending-fg; background --pending-bg\n- h1: font-weight --weight-bold\n\n## Layout\n\nmain: color --ink\n",
    );

    assert_eq!(token_rule_selectors(f.dir()), vec!["h1".to_string(), ".feature-state.state-pending".to_string()]);
}

// ---------------------------------------------------------------- in the design review round

/// A black RGB PNG of `width` x `height`, so the frozen package and the capture hold a real image.
fn png(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in bytes {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let n = height as usize * (1 + 3 * width as usize);
    let mut zlib = vec![0x78, 0x01];
    let mut left = n;
    loop {
        let block = left.min(65_535);
        left -= block;
        zlib.push(if left == 0 { 1 } else { 0 });
        zlib.extend_from_slice(&(block as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block as u16)).to_le_bytes());
        zlib.extend(std::iter::repeat_n(0, block));
        if left == 0 {
            break;
        }
    }
    zlib.extend_from_slice(&((((n % 65521) as u32) << 16) | 1).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A fake pi that answers `answer` with the run id, reference hash and capture revision the prompt names.
fn fake_pi(dir: &Path, answer: &Value) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("answer.json"), answer.to_string()).unwrap();
    let script = dir.join("pi");
    let d = dir.display();
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             for a; do prompt=$a; done\n\
             run=$(printf '%s\\n' \"$prompt\" | sed -n 's/^run_id: //p' | head -n 1)\n\
             ref=$(printf '%s\\n' \"$prompt\" | sed -n 's/^reference_hash: //p' | head -n 1)\n\
             cap=$(printf '%s\\n' \"$prompt\" | sed -n 's/^capture_revision: //p' | head -n 1)\n\
             sed -e \"s|@RUN@|$run|g\" -e \"s|@REF@|$ref|g\" -e \"s|@CAP@|$cap|g\" '{d}/answer.json'\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Writes the desktop screenshot, `measurements.json` with an `h1` at weight 600, and `capture.json`.
fn capture_with_a_600_heading(run: &CaptureRun) -> CaptureOutcome {
    let round = run.run_logs.join("design").join(format!("round-{}", run.round));
    std::fs::create_dir_all(round.join("desktop")).unwrap();
    std::fs::write(round.join("desktop/index.png"), png(400, 300)).unwrap();
    let measurements = json!({"viewport": "desktop", "width": 400, "height": 300, "routes": {"/": page(
        vec![element("h1", json!({"font-size": "32px", "font-weight": "600", "color": "rgb(17, 17, 17)"}))],
        json!({}),
    )}});
    std::fs::write(round.join("desktop/measurements.json"), measurements.to_string()).unwrap();
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
    std::fs::write(round.join("capture.json"), serde_json::to_vec(&record).unwrap()).unwrap();
    CaptureOutcome::Captured(record)
}

#[test]
fn a_reviewer_pass_with_a_violated_token_rule_becomes_changes_required_with_the_tokens_issue() {
    let repo = GitFixture::new();
    let package = repo.root().join("designs").join("home");
    std::fs::create_dir_all(package.join("references")).unwrap();
    std::fs::write(package.join("references/desktop.png"), png(400, 300)).unwrap();
    std::fs::write(package.join("DESIGN.md"), DESIGN).unwrap();
    std::fs::write(package.join("tokens.css"), TOKENS).unwrap();
    let logs = TempDir::new("design-token-review-logs");
    let agents = TempDir::new("design-token-review-agents");
    let pass = json!({
        "run_id": "@RUN@", "reference_hash": "@REF@", "capture_revision": "@CAP@",
        "evaluation": "the capture matches the reference", "verdict": "pass",
        "checks": [{"id": "layout", "required": true, "status": "pass", "evidence": ["round-1/desktop/index.png"]}],
        "issues": [], "suggestions": [], "blockers": [],
    });
    let agent = |role: &str| DesignAgent {
        program: fake_pi(&agents.path().join(role), &pass),
        harness: "pi".to_string(),
        model: "openai/gpt-6.1-sol".to_string(),
        thinking: Some("medium".to_string()),
    };
    let (reviewer, verifier) = (agent("reviewer"), agent("verifier"));
    let settings = DesignSettings {
        start_command: Some(vec!["true".into()]),
        ready_path: "/".into(),
        ready_timeout: Duration::from_secs(5),
        max_correction_rounds: 3,
        verify_after_rounds: 2,
        mode: DesignMode::Infer,
        viewports: [("desktop".to_string(), Viewport { width: 400, height: 300 })].into_iter().collect(),
    };
    let run = DesignReviewRun {
        run_id: "design-token-review",
        main_checkout: repo.root(),
        feature: "home",
        settings: &settings,
        run_logs: logs.path(),
        worktree: repo.root(),
        round: 1,
        reviewer: &reviewer,
        verifier: &verifier,
        skill: "the design-review skill",
        schema: "the review schema",
        agent_timeout: Duration::from_secs(30),
    };

    let outcome = review_design(&run, &capture_with_a_600_heading);

    assert_eq!(outcome.verdict, DesignVerdict::ChangesRequired, "{:?}", outcome.reason);
    let design = logs.path().join("design");
    let written: Value = serde_json::from_slice(&std::fs::read(design.join("round-1/review.json")).unwrap()).unwrap();
    let issues = written["review"]["issues"].as_array().unwrap();
    let tokens: Vec<&Value> = issues.iter().filter(|issue| issue["source"] == "tokens").collect();
    assert_eq!(tokens.len(), 1, "{written:#}");
    assert_eq!(tokens[0]["severity"], "major");
    assert_eq!(tokens[0]["confidence"], "high");
    assert_eq!(tokens[0]["selector"], "h1");
    let check: Value = serde_json::from_slice(&std::fs::read(design.join("round-1/token-check.json")).unwrap()).unwrap();
    assert_eq!(check["status"], "fail");
}
