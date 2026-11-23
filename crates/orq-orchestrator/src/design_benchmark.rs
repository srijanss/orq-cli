//! `orq design benchmark`: a design reviewer measured on known cases before it is trusted with
//! runs. Each case folder holds `reference.png`, `actual.png` and `case.json` (the viewport and
//! the issues the pair is known to have, or `"control": true` for an identical pair that must
//! pass). Every case is reviewed through the real review path (the same prompt, skill, schema,
//! read-only harness and answer checks as a round) `repeat` times, and the answers are scored.
//! Case files are data, never instructions; nothing outside the benchmark's own folder is written.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::design_review::{run_agent_logged, DesignAgent, DesignReview, PairReview};

/// The most reviews of one case a benchmark runs.
pub const MAX_REPEAT: u32 = 5;
const CASE_FILE: &str = "case.json";
const REFERENCE_FILE: &str = "reference.png";
const ACTUAL_FILE: &str = "actual.png";
const REPORT_FILE: &str = "report.md";
const RESULTS_FILE: &str = "results.json";
/// The most a `case.json` may hold.
const MAX_CASE_FILE: u64 = 64 * 1024;

/// One benchmark.
#[derive(Debug, Clone, Copy)]
pub struct DesignBenchmarkRun<'a> {
    /// The folder of cases, one subfolder each.
    pub cases: &'a Path,
    /// The benchmark's own folder: `<state>/design-benchmarks/<time>`. Made here; one that already
    /// exists is refused.
    pub output: &'a Path,
    /// The design reviewer measured.
    pub agent: &'a DesignAgent,
    /// Reviews per case, 1 to [`MAX_REPEAT`].
    pub repeat: u32,
    /// The design-review skill's `SKILL.md` and the review schema, as a round gets them.
    pub skill: &'a str,
    pub schema: &'a str,
    /// How long one review may take.
    pub agent_timeout: Duration,
}

/// What a benchmark wrote.
#[derive(Debug, Clone)]
pub struct DesignBenchmarkOutcome {
    pub report: PathBuf,
    pub results: PathBuf,
    /// The totals, one line.
    pub summary: String,
    pub cases: Vec<CaseResult>,
    pub totals: BenchmarkTotals,
}

/// A case's scores over its repeats.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseResult {
    pub name: String,
    pub control: bool,
    /// The expected issues' ids.
    pub expected: Vec<String>,
    pub repeats: Vec<RepeatResult>,
    /// The expected issues found in every repeat.
    pub found_every_repeat: Vec<String>,
    pub agreement: Agreement,
    /// Answers that were not a valid review.
    pub invalid: u32,
    /// A control: every repeat a valid pass with no issue. `None` for a case with expected issues.
    pub passed: Option<bool>,
}

/// One review of a case.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepeatResult {
    pub repeat: u32,
    /// `None` when the answer was invalid.
    pub verdict: Option<crate::design_review::DesignVerdict>,
    /// Why the answer was not a valid review.
    pub invalid: Option<String>,
    pub found: Vec<String>,
    pub missed: Vec<String>,
    /// The ids of reported issues that match no expected issue (on a control, every issue).
    pub false_positives: Vec<String>,
    pub seconds: f64,
}

/// How many repeats gave the most common verdict (`invalid` counts as one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Agreement {
    pub verdict: String,
    pub agreeing: u32,
    pub repeats: u32,
}

/// The agent's totals over every case and repeat.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkTotals {
    /// `<harness>/<model>[:<thinking>]`.
    pub agent: String,
    pub reviews: u32,
    pub invalid: u32,
    /// Expected issues found, summed over repeats.
    pub found: u32,
    /// Expected issues, summed over repeats.
    pub expected: u32,
    pub false_positives: u32,
    pub controls: u32,
    pub controls_passed: u32,
    pub seconds_per_review: f64,
}

/// `case.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseFile {
    viewport: String,
    #[serde(default)]
    issues: Vec<ExpectedIssue>,
    #[serde(default)]
    control: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedIssue {
    id: String,
    section: String,
    keywords: Vec<String>,
}

struct Case {
    name: String,
    dir: PathBuf,
    file: CaseFile,
}

/// Reviews every case `repeat` times with the agent and writes `report.md` and `results.json` in
/// the benchmark's folder. Refused, before any review, when the repeat is out of range or a case
/// cannot be read.
pub fn run_design_benchmark(run: &DesignBenchmarkRun) -> Result<DesignBenchmarkOutcome, String> {
    if !(1..=MAX_REPEAT).contains(&run.repeat) {
        return Err(format!(
            "--repeat must be 1 to {MAX_REPEAT}, not {}",
            run.repeat
        ));
    }
    let cases = read_cases(run.cases)?;
    // Made here and only here, so two benchmarks never share a folder.
    let made = match run.output.parent() {
        Some(parent) => std::fs::create_dir_all(parent).and_then(|()| std::fs::create_dir(run.output)),
        None => std::fs::create_dir(run.output),
    };
    made.map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => {
            format!("{} already exists: another benchmark wrote it", run.output.display())
        }
        kind => format!("{} could not be made ({kind:?})", run.output.display()),
    })?;
    let mut results = Vec::new();
    for case in &cases {
        results.push(benchmark_case(run, case)?);
    }
    let totals = totals(run.agent, &results);
    let summary = summary(&totals);
    let report = run.output.join(REPORT_FILE);
    let results_file = run.output.join(RESULTS_FILE);
    let record = serde_json::json!({
        "agent": totals.agent,
        "repeat": run.repeat,
        "cases": results,
        "totals": totals,
    });
    std::fs::write(
        &results_file,
        serde_json::to_vec_pretty(&record).unwrap_or_default(),
    )
    .and_then(|()| std::fs::write(&report, report_text(&results, &totals, &summary)))
    .map_err(|e| format!("the benchmark could not be written ({:?})", e.kind()))?;
    Ok(DesignBenchmarkOutcome {
        report,
        results: results_file,
        summary,
        cases: results,
        totals,
    })
}

/// The cases, by folder name; every one must hold its three files and a valid `case.json`.
fn read_cases(dir: &Path) -> Result<Vec<Case>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        format!(
            "the cases folder {} could not be read ({:?})",
            dir.display(),
            e.kind()
        )
    })?;
    let mut cases = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // The name reaches the reviewer's prompt (its run id and folder), outside the data
        // delimiters: only a plain name does.
        let plain_name = !name.starts_with('.')
            && name.len() <= 100
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !plain_name {
            return Err(format!(
                "the case folder name {name:?} must be letters, digits, '-', '_' or '.', not starting with '.'"
            ));
        }
        let case_dir = entry.path();
        for file in [REFERENCE_FILE, ACTUAL_FILE] {
            if !case_dir.join(file).is_file() {
                return Err(format!("the case {name:?} has no {file}"));
            }
            if !is_png(&case_dir.join(file)) {
                return Err(format!("the case {name:?}'s {file} is not a PNG"));
            }
        }
        let text = read_capped(&case_dir.join(CASE_FILE))
            .map_err(|why| format!("the case {name:?}: {why}"))?;
        let file: CaseFile = serde_json::from_str(&text)
            .map_err(|e| format!("the case {name:?}'s {CASE_FILE} is not valid ({e})"))?;
        let plain = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        if !plain(&file.viewport) {
            return Err(format!(
                "the case {name:?}'s viewport must be a name like desktop, not {:?}",
                file.viewport
            ));
        }
        if file.control && !file.issues.is_empty() {
            return Err(format!(
                "the case {name:?} is a control, so it lists no expected issues"
            ));
        }
        if !file.control && file.issues.is_empty() {
            return Err(format!(
                "the case {name:?} lists no expected issues and is not a control"
            ));
        }
        if let Some(issue) = file
            .issues
            .iter()
            .find(|issue| issue.keywords.iter().all(|k| k.trim().is_empty()))
        {
            return Err(format!(
                "the case {name:?}'s issue {:?} has no keyword",
                issue.id
            ));
        }
        cases.push(Case {
            name,
            dir: case_dir,
            file,
        });
    }
    if cases.is_empty() {
        return Err(format!("{} holds no case folders", dir.display()));
    }
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cases)
}

/// Whether the file starts with the PNG signature.
fn is_png(path: &Path) -> bool {
    use std::io::Read;
    let mut signature = [0u8; 8];
    std::fs::File::open(path).is_ok_and(|mut file| file.read_exact(&mut signature).is_ok())
        && signature == *b"\x89PNG\r\n\x1a\n"
}

fn read_capped(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path)
        .map_err(|e| format!("{CASE_FILE} could not be read ({:?})", e.kind()))?;
    let mut text = String::new();
    file.take(MAX_CASE_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{CASE_FILE} could not be read ({:?})", e.kind()))?;
    if text.len() as u64 > MAX_CASE_FILE {
        return Err(format!("{CASE_FILE} is over {MAX_CASE_FILE} bytes"));
    }
    Ok(text)
}

/// The case laid out as a round's design folder (its pair copied, so no agent sees the case
/// folder) and reviewed `repeat` times.
fn benchmark_case(run: &DesignBenchmarkRun, case: &Case) -> Result<CaseResult, String> {
    let viewport = &case.file.viewport;
    let design_dir = run.output.join("cases").join(&case.name);
    let reference = format!("package/references/{viewport}.png");
    let capture = format!("round-1/{viewport}/index.png");
    for (from, to) in [(REFERENCE_FILE, &reference), (ACTUAL_FILE, &capture)] {
        let to = design_dir.join(to);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{} could not be made ({:?})", parent.display(), e.kind()))?;
        }
        std::fs::copy(case.dir.join(from), &to).map_err(|e| {
            format!(
                "the case {:?}'s {from} could not be copied ({:?})",
                case.name,
                e.kind()
            )
        })?;
    }
    let expected: Vec<String> = case
        .file
        .issues
        .iter()
        .map(|issue| issue.id.clone())
        .collect();
    let mut repeats = Vec::new();
    for repeat in 1..=run.repeat {
        let run_id = format!("design-benchmark-{}-{repeat}", case.name);
        let pair = PairReview::new(&run_id, &design_dir, &reference, &capture, viewport)?;
        let log = design_dir.join(format!("repeat-{repeat}.log"));
        let started = Instant::now();
        let answer = pair
            .argv(run.agent, run.skill, run.schema)
            .and_then(|argv| {
                run_agent_logged(
                    run.agent,
                    "design reviewer",
                    &argv,
                    &design_dir,
                    &log,
                    run.agent_timeout,
                )
            })
            .and_then(|answer| pair.check(&answer));
        let seconds = started.elapsed().as_secs_f64();
        repeats.push(score(repeat, &case.file, answer, seconds));
    }
    let found_every_repeat = expected
        .iter()
        .filter(|id| repeats.iter().all(|r| r.found.contains(id)))
        .cloned()
        .collect();
    let invalid = repeats.iter().filter(|r| r.invalid.is_some()).count() as u32;
    let passed = case.file.control.then(|| {
        repeats.iter().all(|r| {
            r.verdict == Some(crate::design_review::DesignVerdict::Pass)
                && r.false_positives.is_empty()
        })
    });
    Ok(CaseResult {
        name: case.name.clone(),
        control: case.file.control,
        expected,
        agreement: agreement(&repeats),
        found_every_repeat,
        repeats,
        invalid,
        passed,
    })
}

/// One answer scored: an expected issue is found when a reported issue's section matches and its
/// text holds one of its keywords; a reported issue matching none is a false positive.
fn score(
    repeat: u32,
    case: &CaseFile,
    answer: Result<DesignReview, String>,
    seconds: f64,
) -> RepeatResult {
    let review = match answer {
        Ok(review) => review,
        Err(why) => {
            return RepeatResult {
                repeat,
                verdict: None,
                invalid: Some(why),
                found: Vec::new(),
                missed: case.issues.iter().map(|issue| issue.id.clone()).collect(),
                false_positives: Vec::new(),
                seconds,
            }
        }
    };
    let matches = |reported: &crate::design_review::ReviewIssue, expected: &ExpectedIssue| {
        let text = format!(
            "{} {} {}",
            reported.expected, reported.actual, reported.fix_direction
        )
        .to_lowercase();
        reported
            .section
            .trim()
            .eq_ignore_ascii_case(expected.section.trim())
            && expected
                .keywords
                .iter()
                .map(|k| k.trim().to_lowercase())
                .any(|k| !k.is_empty() && text.contains(&k))
    };
    let (found, missed): (Vec<&ExpectedIssue>, Vec<&ExpectedIssue>) =
        case.issues.iter().partition(|expected| {
            review
                .issues
                .iter()
                .any(|reported| matches(reported, expected))
        });
    let false_positives = review
        .issues
        .iter()
        .filter(|reported| {
            !case
                .issues
                .iter()
                .any(|expected| matches(reported, expected))
        })
        .map(|reported| reported.id.clone())
        .collect();
    RepeatResult {
        repeat,
        verdict: Some(review.verdict),
        invalid: None,
        found: found.iter().map(|issue| issue.id.clone()).collect(),
        missed: missed.iter().map(|issue| issue.id.clone()).collect(),
        false_positives,
        seconds,
    }
}

fn verdict_label(repeat: &RepeatResult) -> &'static str {
    repeat.verdict.map_or("invalid", |verdict| verdict.as_str())
}

/// The most common verdict, the first given on a tie.
fn agreement(repeats: &[RepeatResult]) -> Agreement {
    let mut best = ("invalid", 0u32);
    for repeat in repeats {
        let label = verdict_label(repeat);
        let count = repeats.iter().filter(|r| verdict_label(r) == label).count() as u32;
        if count > best.1 {
            best = (label, count);
        }
    }
    Agreement {
        verdict: best.0.to_string(),
        agreeing: best.1,
        repeats: repeats.len() as u32,
    }
}

fn agent_label(agent: &DesignAgent) -> String {
    match &agent.thinking {
        Some(thinking) => format!("{}/{}:{thinking}", agent.harness, agent.model),
        None => format!("{}/{}", agent.harness, agent.model),
    }
}

fn totals(agent: &DesignAgent, cases: &[CaseResult]) -> BenchmarkTotals {
    let repeats = || cases.iter().flat_map(|case| &case.repeats);
    let reviews = repeats().count() as u32;
    let seconds: f64 = repeats().map(|r| r.seconds).sum();
    BenchmarkTotals {
        agent: agent_label(agent),
        reviews,
        invalid: cases.iter().map(|case| case.invalid).sum(),
        found: repeats().map(|r| r.found.len() as u32).sum(),
        expected: cases
            .iter()
            .map(|case| (case.expected.len() * case.repeats.len()) as u32)
            .sum(),
        false_positives: repeats().map(|r| r.false_positives.len() as u32).sum(),
        controls: cases.iter().filter(|case| case.control).count() as u32,
        controls_passed: cases
            .iter()
            .filter(|case| case.passed == Some(true))
            .count() as u32,
        seconds_per_review: if reviews == 0 {
            0.0
        } else {
            seconds / reviews as f64
        },
    }
}

fn summary(totals: &BenchmarkTotals) -> String {
    format!(
        "{}: found {}/{} expected issues, {} false positives, {}/{} controls passed, {} invalid of {} reviews, {:.1}s per review",
        totals.agent,
        totals.found,
        totals.expected,
        totals.false_positives,
        totals.controls_passed,
        totals.controls,
        totals.invalid,
        totals.reviews,
        totals.seconds_per_review,
    )
}

/// `report.md`: the totals, then each case's repeats. Every value from a case or an answer is
/// written on one line, its control characters blanked.
fn report_text(cases: &[CaseResult], totals: &BenchmarkTotals, summary: &str) -> String {
    let line = |text: &str| -> String {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect()
    };
    let list = |ids: &[String]| -> String {
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.iter().map(|id| line(id)).collect::<Vec<_>>().join(", ")
        }
    };
    let mut text = format!(
        "# Design reviewer benchmark: {}\n\n{}\n",
        line(&totals.agent),
        line(summary)
    );
    for case in cases {
        text.push_str(&format!(
            "\n## {}{}\n\n",
            line(&case.name),
            if case.control { " (control)" } else { "" }
        ));
        if let Some(passed) = case.passed {
            text.push_str(&format!(
                "Control: {}\n",
                if passed { "passed" } else { "failed" }
            ));
        } else {
            text.push_str(&format!(
                "Found in every repeat: {}\n",
                list(&case.found_every_repeat)
            ));
        }
        text.push_str(&format!(
            "Verdict agreement: {}/{} {}\nInvalid answers: {}\n\n",
            case.agreement.agreeing, case.agreement.repeats, case.agreement.verdict, case.invalid
        ));
        for repeat in &case.repeats {
            match &repeat.invalid {
                Some(why) => text.push_str(&format!(
                    "- repeat {}: invalid ({:.1}s): {}\n",
                    repeat.repeat,
                    repeat.seconds,
                    line(why)
                )),
                None => text.push_str(&format!(
                    "- repeat {}: {} ({:.1}s); found {}; missed {}; false positives {}\n",
                    repeat.repeat,
                    verdict_label(repeat),
                    repeat.seconds,
                    list(&repeat.found),
                    list(&repeat.missed),
                    list(&repeat.false_positives),
                )),
            }
        }
    }
    text
}
