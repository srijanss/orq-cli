//! What in a design package binds, and the severity rules orq fixes itself: every issue names its
//! basis (`rule`, `structure` or `visual`), a `visual` issue is at most minor, and an issue whose
//! basis is not backed (no quote, no token, no reference, a copy difference with no copy rule) is
//! invalid. The reviewer is a fake pi script answering from a file; the capture is a stand-in.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orq_orchestrator::{
    design_fix_task, review_design, run_design_loop, CaptureOutcome, CaptureRecord, CaptureRun,
    CorrectionOutcome, DesignAgent, DesignLoopOutcome, DesignLoopSteps, DesignMode,
    DesignReviewOutcome, DesignReviewRun, DesignSettings, DesignVerdict, PageCapture, ReviewIssue,
    RunRecord, RunStore, StoreError, Viewport,
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

/// A black RGB PNG of `width` x `height`, its pixels in one stored zlib block.
fn png(width: u32, height: u32) -> Vec<u8> {
    let raw = height as usize * (1 + 3 * width as usize);
    assert!(raw <= 65_535);
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

// ---------------------------------------------------------------- fixture

const RUN_ID: &str = "design-review-rules-test";
const FEATURE: &str = "home";
const DESIGN_MD: &str = "# Home\n\nThe header is 64px tall on every viewport.\n";

/// A fake pi: answers with `answer-<n>.json` (or `answer.json`) of its folder, with `@RUN@`, `@REF@`
/// and `@CAP@` replaced by what the prompt names, and keeps its argv.
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
        Self::with_design(DESIGN_MD)
    }

    fn with_design(design_md: &str) -> Self {
        let repo = GitFixture::new();
        let package = repo.root().join("designs").join(FEATURE);
        fs::create_dir_all(package.join("references")).unwrap();
        fs::write(package.join("references/desktop.png"), png(40, 30)).unwrap();
        fs::write(package.join("DESIGN.md"), design_md).unwrap();
        let agents = TempDir::new("orq-design-rules-agents");
        fake_pi(&agents.path().join("reviewer"));
        fake_pi(&agents.path().join("verifier"));
        let viewports: BTreeMap<String, Viewport> = [(
            "desktop".to_string(),
            Viewport {
                width: 40,
                height: 30,
            },
        )]
        .into_iter()
        .collect();
        let settings = DesignSettings {
            start_command: Some(vec!["true".into()]),
            ready_path: "/".into(),
            ready_timeout: Duration::from_secs(5),
            max_correction_rounds: 3,
            verify_after_rounds: 2,
            mode: DesignMode::Infer,
            viewports,
        };
        Fixture {
            repo,
            logs: TempDir::new("orq-design-rules-logs"),
            agents,
            settings,
        }
    }

    fn agent_dir(&self, role: &str) -> PathBuf {
        self.agents.path().join(role)
    }

    fn answer(&self, role: &str, answer: &Value) {
        fs::write(self.agent_dir(role).join("answer.json"), answer.to_string()).unwrap();
    }

    fn answer_at(&self, role: &str, attempt: u32, answer: &Value) {
        fs::write(
            self.agent_dir(role).join(format!("answer-{attempt}.json")),
            answer.to_string(),
        )
        .unwrap();
    }

    fn calls(&self, role: &str) -> u32 {
        fs::read_to_string(self.agent_dir(role).join("count"))
            .map(|n| n.trim().parse().unwrap())
            .unwrap_or(0)
    }

    fn prompt(&self, role: &str, call: u32) -> String {
        fs::read_to_string(self.agent_dir(role).join(format!("argv-{call}"))).unwrap()
    }

    fn design_dir(&self) -> PathBuf {
        self.logs.path().join("design")
    }

    fn written(&self, round: u32) -> Value {
        serde_json::from_slice(
            &fs::read(self.design_dir().join(format!("round-{round}/review.json"))).unwrap(),
        )
        .unwrap()
    }

    fn report(&self) -> String {
        fs::read_to_string(self.design_dir().join("report.md")).unwrap()
    }

    fn review(&self, round: u32) -> DesignVerdict {
        self.review_outcome(round).verdict
    }

    fn review_outcome(&self, round: u32) -> DesignReviewOutcome {
        let agent = |role: &str| DesignAgent {
            program: self.agent_dir(role).join("pi"),
            harness: "pi".to_string(),
            model: "openai/gpt-6.1-terra".to_string(),
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
            agent_timeout: Duration::from_secs(30),
        };
        review_design(&run, &fake_capture)
    }
}

/// The design review step around the fake reviewer: each round reviewed for real, each correction
/// batch's task kept, and the run stopped at the first batch.
struct Loop<'a> {
    f: &'a Fixture,
    tasks: Vec<String>,
}

impl DesignLoopSteps for Loop<'_> {
    fn head(&mut self) -> Result<String, StoreError> {
        Ok("head-1".to_string())
    }

    fn review(&mut self, round: u32) -> Result<DesignReviewOutcome, StoreError> {
        Ok(self.f.review_outcome(round))
    }

    fn correct(
        &mut self,
        batch: u32,
        issues: &[ReviewIssue],
        suggestions: &[String],
    ) -> Result<CorrectionOutcome, StoreError> {
        self.tasks
            .push(design_fix_task(batch, 3, issues, suggestions));
        Ok(CorrectionOutcome::Stopped(
            "stopped after the first batch".to_string(),
        ))
    }

    fn review_code(&mut self, _batch: u32, _from: &str) -> Result<CorrectionOutcome, StoreError> {
        Ok(CorrectionOutcome::Done)
    }
}

/// Runs the design review step of a fresh run over `f`; its outcome and the batches' tasks.
fn run_loop(f: &Fixture) -> (DesignLoopOutcome, Vec<String>) {
    let state = f.logs.path().join("state");
    let store = RunStore::new(&state);
    store
        .create(&RunRecord {
            delivery: Default::default(),
            run_id: RUN_ID.to_string(),
            project_id: "repo-1".to_string(),
            feature_id: FEATURE.to_string(),
            remote: "origin".to_string(),
            provider: "github".to_string(),
            target_branch: "main".to_string(),
            base_sha: "0".repeat(40),
            branch: "fix--home".to_string(),
            worktree: f.repo.root().to_path_buf(),
            repository: String::new(),
            profiles: BTreeMap::new(),
        })
        .unwrap();
    let mut steps = Loop {
        f,
        tasks: Vec::new(),
    };
    let outcome = run_design_loop(&store, RUN_ID, 3, &mut steps).unwrap();
    (outcome, steps.tasks)
}

/// Writes one desktop screenshot and `capture.json` for the round, as the real capture does.
fn fake_capture(run: &CaptureRun) -> CaptureOutcome {
    let round = run
        .run_logs
        .join("design")
        .join(format!("round-{}", run.round));
    fs::create_dir_all(round.join("desktop")).unwrap();
    fs::write(round.join("desktop/index.png"), png(40, 30)).unwrap();
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
            viewport_width: 40,
            viewport_height: 30,
            image: Some("desktop/index.png".to_string()),
            image_width: Some(40),
            image_height: Some(30),
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

/// An issue on `basis`, backed as that basis needs: a `rule` quotes DESIGN.md, a `structure` names
/// the desktop reference, a `visual` names nothing.
fn issue(id: &str, basis: &str, severity: &str) -> Value {
    let mut issue = json!({
        "id": id, "section": "Header", "viewport": "desktop", "source": "references/desktop.png",
        "expected": "a 64px header", "actual": "a 48px header",
        "evidence": ["round-1/desktop/index.png"], "severity": severity, "status": "open",
        "fix_direction": "make the header taller", "basis": basis, "copy": false,
    });
    match basis {
        "rule" => issue["quote"] = json!("The header is 64px tall on every viewport."),
        "structure" => issue["reference"] = json!("references/desktop.png"),
        _ => {}
    }
    issue
}

fn answer(verdict: &str, issues: Vec<Value>) -> Value {
    json!({
        "run_id": "@RUN@", "reference_hash": "@REF@", "capture_revision": "@CAP@",
        "evaluation": "compared the desktop capture", "verdict": verdict,
        "checks": [check("layout", "pass")], "issues": issues, "suggestions": [], "blockers": [],
    })
}

// ---------------------------------------------------------------- tests

#[test]
fn a_major_visual_issue_is_downgraded_to_minor_and_the_round_passes() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![issue("header-shade", "visual", "major")],
        ),
    );

    assert_eq!(f.review(1), DesignVerdict::Pass);

    let written = f.written(1);
    let issue = &written["review"]["issues"][0];
    assert_eq!(issue["severity"], "minor", "{written:#}");
    assert_eq!(issue["downgraded_from"], "major", "{written:#}");
    assert_eq!(f.calls("reviewer"), 1);
}

#[test]
fn a_major_structure_issue_fails_the_round() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![issue("nav-missing", "structure", "major")],
        ),
    );

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);

    let written = f.written(1);
    let issue = &written["review"]["issues"][0];
    assert_eq!(issue["severity"], "major", "{written:#}");
    assert_eq!(issue["basis"], "structure", "{written:#}");
    assert!(issue.get("downgraded_from").is_none(), "{written:#}");
}

/// The reviewer answers `bad` every time: invalid, retried once, then the round is blocked.
fn assert_invalid(case: &str, bad: Value, why: &str) {
    let f = Fixture::new();
    f.answer("reviewer", &answer("changes_required", vec![bad]));

    assert_eq!(f.review(1), DesignVerdict::Blocked, "{case}");
    assert_eq!(f.calls("reviewer"), 2, "{case}: one retry");
    let reason = f.written(1)["reason"].as_str().unwrap().to_string();
    assert!(
        reason.contains("invalid") && reason.contains(why),
        "{case}: {reason}"
    );
}

#[test]
fn a_rule_issue_without_a_quote_or_token_in_the_package_is_invalid() {
    let mut unquoted = issue("header-height", "rule", "major");
    unquoted.as_object_mut().unwrap().remove("quote");
    assert_invalid("no quote", unquoted, "quotes no");

    let mut invented = issue("header-height", "rule", "major");
    invented["quote"] = json!("The header is 80px tall.");
    assert_invalid("a quote the documents don't hold", invented, "quotes no");

    let mut no_token = issue("header-height", "rule", "major");
    no_token.as_object_mut().unwrap().remove("quote");
    no_token["token"] = json!("--header-height");
    assert_invalid("a token the package doesn't define", no_token, "quotes no");

    let mut no_basis = issue("header-height", "rule", "major");
    no_basis.as_object_mut().unwrap().remove("basis");
    assert_invalid("no basis", no_basis, "basis");
}

#[test]
fn a_structure_issue_without_a_reference_of_the_package_is_invalid() {
    let mut unnamed = issue("nav-missing", "structure", "major");
    unnamed.as_object_mut().unwrap().remove("reference");
    assert_invalid("no reference", unnamed, "reference");

    let mut unknown = issue("nav-missing", "structure", "major");
    unknown["reference"] = json!("references/tablet.png");
    assert_invalid("a reference the package doesn't hold", unknown, "reference");

    // Named as the prompt lists it, under package/, it stands.
    let f = Fixture::new();
    let mut listed = issue("nav-missing", "structure", "major");
    listed["reference"] = json!("package/references/desktop.png");
    f.answer("reviewer", &answer("changes_required", vec![listed]));
    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
}

#[test]
fn a_text_or_data_difference_with_no_design_md_copy_rule_is_invalid() {
    let text = |basis: &str| {
        let mut issue = issue("hero-title", basis, "major");
        issue["expected"] = json!("the title \"Plan your week\"");
        issue["actual"] = json!("the title \"Welcome\"");
        issue["copy"] = json!(true);
        issue
    };
    assert_invalid("held against the mockup", text("structure"), "copy");
    assert_invalid("a minor mockup detail", text("visual"), "copy");

    let mut unstated = issue("hero-title", "structure", "major");
    unstated.as_object_mut().unwrap().remove("copy");
    assert_invalid("copy not stated", unstated, "copy");

    // UX.md is binding, but only DESIGN.md can make the copy exact.
    let f = Fixture::with_design("# Home\n\nThe title reads exactly \"Plan your week\".\n");
    fs::write(
        f.repo.root().join("designs").join(FEATURE).join("UX.md"),
        "Say \"Plan your week\".\n",
    )
    .unwrap();
    let mut ux = text("rule");
    ux["quote"] = json!("Say \"Plan your week\".");
    f.answer("reviewer", &answer("changes_required", vec![ux]));
    assert_eq!(f.review(1), DesignVerdict::Blocked);

    let f = Fixture::with_design("# Home\n\nThe title reads exactly \"Plan your week\".\n");
    let mut exact = text("rule");
    exact["quote"] = json!("The title reads exactly \"Plan your week\".");
    f.answer("reviewer", &answer("changes_required", vec![exact]));
    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
}

#[test]
fn a_copy_issue_quoting_a_design_md_line_that_does_not_make_the_copy_exact_is_invalid() {
    let mut unrelated = issue("hero-title", "rule", "major");
    unrelated["expected"] = json!("the title \"Plan your week\"");
    unrelated["actual"] = json!("the title \"Welcome\"");
    unrelated["copy"] = json!(true);
    // A real DESIGN.md line, but about the header's height, not about exact copy.
    unrelated["quote"] = json!("The header is 64px tall on every viewport.");
    assert_invalid("an unrelated DESIGN.md line", unrelated, "copy");
}

/// A fixture whose h1 is measured at weight 600 where a token rule binds it to `--weight-bold`.
fn violated_token_rule() -> Fixture {
    let f = Fixture::with_design(&format!(
        "{DESIGN_MD}\n## Token rules\n\nh1: font-weight --weight-bold\n"
    ));
    let package = f.repo.root().join("designs").join(FEATURE);
    fs::write(
        package.join("tokens.css"),
        ":root { --weight-bold: 700; }\n",
    )
    .unwrap();
    let h1 = json!({"tag": "h1", "index": 0, "box": {"x": 0, "y": 0, "width": 10, "height": 10},
                    "styles": {"font-weight": "600"}});
    let measured = json!({"viewport": "desktop", "width": 40, "height": 30, "routes": {"/": {
        "document": {"width": 40, "height": 30, "root_font_size": 16}, "elements": [h1]}}});
    fs::write(f.logs.path().join("measured.json"), measured.to_string()).unwrap();
    f
}

#[test]
fn a_reviewer_pass_with_an_open_token_violation_is_invalid() {
    let f = violated_token_rule();
    f.answer("reviewer", &answer("pass", vec![]));
    f.answer("verifier", &answer("pass", vec![]));

    assert_eq!(f.review(1), DesignVerdict::Blocked);
    assert_eq!(f.calls("reviewer"), 2, "one retry");
    let reason = f.written(1)["reason"].as_str().unwrap().to_string();
    assert!(
        reason.contains("invalid") && reason.contains("tokens-desktop-h1-font-weight"),
        "{reason}"
    );
    let prompt = f.prompt("reviewer", 1);
    assert!(
        prompt.contains("tokens-desktop-h1-font-weight"),
        "the reviewer is told which token issues are open: {prompt}"
    );

    // Answered changes_required on the retry, the token issue joins the round as a rule issue.
    let f = violated_token_rule();
    f.answer_at("reviewer", 1, &answer("pass", vec![]));
    f.answer_at("reviewer", 2, &answer("changes_required", vec![]));

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    let written = f.written(1);
    let token = &written["review"]["issues"][0];
    assert_eq!(token["id"], "tokens-desktop-h1-font-weight", "{written:#}");
    assert_eq!(token["basis"], "rule", "{written:#}");
    assert_eq!(token["token"], "--weight-bold", "{written:#}");
    assert_eq!(token["severity"], "major", "{written:#}");
}

#[test]
fn the_report_shows_each_issues_basis_and_lists_downgraded_issues_separately() {
    let f = Fixture::new();
    let mut reviewed = answer(
        "changes_required",
        vec![
            issue("nav-missing", "structure", "major"),
            issue("header-shade", "visual", "critical"),
        ],
    );
    reviewed["suggestions"] = json!(["round the search field a little more"]);
    f.answer("reviewer", &reviewed);

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);

    let report = f.report();
    assert!(
        report.contains("- nav-missing [major, open, structure: references/desktop.png]"),
        "{report}"
    );
    assert!(
        report.contains("- header-shade [minor, open, visual]"),
        "{report}"
    );
    let downgraded = report
        .split("## Downgraded")
        .nth(1)
        .unwrap_or_else(|| panic!("{report}"));
    assert!(
        downgraded.contains("- header-shade: critical to minor"),
        "{report}"
    );
    assert!(
        !downgraded
            .split("\n## ")
            .next()
            .unwrap()
            .contains("nav-missing"),
        "{report}"
    );
    let suggestions = report
        .split("## Suggestions")
        .nth(1)
        .unwrap_or_else(|| panic!("{report}"));
    assert!(
        suggestions.contains("- round the search field a little more"),
        "{report}"
    );
}

#[test]
fn the_reviewer_is_told_what_binds_and_how_severity_is_fixed_in_every_round() {
    let f = Fixture::new();
    f.answer(
        "reviewer",
        &answer(
            "changes_required",
            vec![issue("nav-missing", "structure", "major")],
        ),
    );
    // Open two rounds, the issue goes to the verifier in round 2 too.
    f.answer(
        "verifier",
        &answer(
            "changes_required",
            vec![issue("nav-missing", "structure", "major")],
        ),
    );
    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    assert_eq!(f.review(2), DesignVerdict::ChangesRequired);

    for call in [1, 2] {
        let prompt = f.prompt("reviewer", call);
        for told in [
            "DESIGN.md, UX.md, RESPONSIVE.md and the tokens bind exactly",
            "Sample data and text in the mockups never bind unless DESIGN.md says the copy is exact",
            "a visual issue is at most minor",
            "\"basis\"",
        ] {
            assert!(prompt.contains(told), "call {call} is not told {told:?}: {prompt}");
        }
    }
}

#[test]
fn minor_issues_alone_pass_with_no_batch_and_ride_along_in_a_batch_sent_for_a_major_issue() {
    let mut minor = issue("footer-gap", "structure", "minor");
    minor["fix_direction"] = json!("close the gap above the footer");
    let suggestion = "round the search field a little more";

    // Only minor issues and a suggestion: the round passes and no batch is sent.
    let f = Fixture::new();
    let mut reviewed = answer(
        "changes_required",
        vec![minor.clone(), issue("header-shade", "visual", "major")],
    );
    reviewed["suggestions"] = json!([suggestion]);
    f.answer("reviewer", &reviewed);
    let (outcome, tasks) = run_loop(&f);
    assert_eq!(
        outcome,
        DesignLoopOutcome::Passed {
            head: "head-1".to_string()
        }
    );
    assert!(tasks.is_empty(), "{tasks:?}");

    // A major issue sends a batch, and the minor issues and suggestions go in it too.
    let f = Fixture::new();
    let mut reviewed = answer(
        "changes_required",
        vec![issue("nav-missing", "structure", "major"), minor],
    );
    reviewed["suggestions"] = json!([suggestion]);
    f.answer("reviewer", &reviewed);
    let (_, tasks) = run_loop(&f);
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    for text in ["nav-missing", "close the gap above the footer", suggestion] {
        assert!(
            tasks[0].contains(text),
            "the batch lacks {text:?}: {}",
            tasks[0]
        );
    }
}

/// The design-review skill and schema orq ships (and gives the reviewer every round).
fn shipped(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../orq-agents/design_skills/design-review")
        .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_shipped_skill_says_what_binds_and_its_schema_asks_each_issue_for_its_basis() {
    let skill = shipped("SKILL.md");
    for needle in [
        "## What binds",
        "`DESIGN.md`, `UX.md`, `RESPONSIVE.md` and the tokens: exact",
        "never bind unless DESIGN.md says the copy is exact",
        "`rule`",
        "`structure`",
        "`visual`",
        "at most minor",
    ] {
        assert!(
            skill.contains(needle),
            "{needle:?} missing from the skill:\n{skill}"
        );
    }
    let schema: serde_json::Value = serde_json::from_str(&shipped("review.schema.json")).unwrap();
    let issue = &schema["properties"]["issues"]["items"];
    let required: Vec<&str> = issue["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r.as_str())
        .collect();
    assert!(
        required.contains(&"basis") && required.contains(&"copy"),
        "{required:?}"
    );
    assert_eq!(
        issue["properties"]["basis"]["enum"],
        json!(["rule", "structure", "visual"])
    );
    for optional in ["quote", "token", "reference"] {
        assert_eq!(
            issue["properties"][optional]["type"],
            json!("string"),
            "{optional}"
        );
    }
}

#[test]
fn a_rule_issue_quoting_a_package_document_or_naming_a_package_token_stands() {
    let f = Fixture::new();
    let package = f.repo.root().join("designs").join(FEATURE);
    fs::write(
        package.join("tokens.css"),
        ":root { --header-height: 64px; }\n",
    )
    .unwrap();
    let mut quoted = issue("header-height", "rule", "major");
    // Quoted as the line runs, its whitespace aside.
    quoted["quote"] = json!("The header is 64px  tall\non every viewport.");
    let mut token = issue("header-token", "rule", "major");
    token.as_object_mut().unwrap().remove("quote");
    token["token"] = json!("--header-height");
    f.answer("reviewer", &answer("changes_required", vec![quoted, token]));

    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    assert_eq!(f.calls("reviewer"), 1);
}

/// A rule issue naming `token` against a package whose tokens file `file` holds `text`.
fn token_issue_against(file: &str, text: &str, token: &str) -> Fixture {
    let f = Fixture::new();
    let package = f.repo.root().join("designs").join(FEATURE);
    fs::write(package.join(file), text).unwrap();
    let mut issue = issue("header-token", "rule", "major");
    issue.as_object_mut().unwrap().remove("quote");
    issue["token"] = json!(token);
    f.answer("reviewer", &answer("changes_required", vec![issue]));
    f
}

#[test]
fn a_rule_issue_stands_only_on_a_token_the_package_defines_by_that_exact_name() {
    // A token is defined: a prefix of one, a comment or a var() reference is not a definition.
    for (case, text) in [
        ("a prefix of a defined token", ":root { --header-height: 64px; }\n"),
        ("named in a comment", "/* --header: 64px */\n:root { --gap: 8px; }\n"),
        ("only referenced", ":root { --gap: var(--header); }\n"),
    ] {
        let f = token_issue_against("tokens.css", text, "--header");
        assert_eq!(f.review(1), DesignVerdict::Blocked, "{case}");
        assert_eq!(f.calls("reviewer"), 2, "{case}: one retry");
    }

    // A nested tokens.json group defines its joined name.
    let f = token_issue_against("tokens.json", r#"{"font": {"title": "32px"}}"#, "--font-title");
    assert_eq!(f.review(1), DesignVerdict::ChangesRequired);
    assert_eq!(f.calls("reviewer"), 1);
}
