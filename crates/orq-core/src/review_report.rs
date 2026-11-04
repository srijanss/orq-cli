//! The deterministic review report (schema v1): what changed, the commit it
//! was pinned to, what the tests did and what the review skill reported.
//! Evidence that is missing is stated as missing; the builder never invents a
//! finding, a round or a test result.

use std::io::ErrorKind;
use std::path::Path;

use serde_json::{json, Value};

use crate::review_input::{optional_text, InputError, ReviewInput};

pub const REPORT_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Passed,
    Failed,
    NotRun,
    Unknown,
}

impl TestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TestStatus::Passed => "passed",
            TestStatus::Failed => "failed",
            TestStatus::NotRun => "not_run",
            TestStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCommand {
    pub command: String,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestEvidence {
    pub status: TestStatus,
    pub commands: Vec<TestCommand>,
    /// The commit the commands ran against; `None` when the file names none.
    pub head_sha: Option<String>,
}

/// Reads the run's exported `tests.json`. The status comes from the recorded
/// exit codes alone; prose is never read as a result.
pub fn load_test_evidence(path: &Path) -> Result<TestEvidence, InputError> {
    let text = std::fs::read_to_string(path).map_err(|err| match err.kind() {
        ErrorKind::NotFound => InputError::Missing,
        _ => InputError::Inaccessible(err.to_string()),
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|err| InputError::Malformed(err.to_string()))?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(InputError::Malformed("unsupported or missing schema_version".to_string()));
    }
    let list = value.get("commands").and_then(Value::as_array).ok_or_else(|| InputError::Malformed("commands is not a list".to_string()))?;
    let mut commands = Vec::new();
    for entry in list {
        let command = entry.get("command").and_then(Value::as_str).ok_or_else(|| InputError::Malformed("command is not text".to_string()))?;
        commands.push(TestCommand { command: command.to_string(), exit_code: entry.get("exit_code").and_then(Value::as_i64) });
    }
    let status = if commands.is_empty() {
        TestStatus::NotRun
    } else if commands.iter().any(|c| c.exit_code.is_some_and(|code| code != 0)) {
        TestStatus::Failed
    } else if commands.iter().all(|c| c.exit_code == Some(0)) {
        TestStatus::Passed
    } else {
        TestStatus::Unknown
    };
    Ok(TestEvidence { status, commands, head_sha: optional_text(&value, "head_sha")? })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFile {
    pub path: String,
    pub old_path: Option<String>,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

pub struct ReportInputs {
    pub run_id: String,
    pub project_id: String,
    pub feature_id: String,
    pub base_sha: String,
    pub head_sha: String,
    pub files: Vec<ReportFile>,
    pub tests: Result<TestEvidence, InputError>,
    pub review: Result<ReviewInput, InputError>,
    pub generated_at: String,
}

fn why(error: &InputError) -> String {
    match error {
        InputError::Missing => "not provided".to_string(),
        InputError::Inaccessible(detail) => format!("could not be read: {detail}"),
        InputError::Malformed(detail) => format!("malformed: {detail}"),
        InputError::Stale(detail) => detail.clone(),
    }
}

/// Builds the report. It is `current` only when both test and review
/// evidence were available; otherwise it is `incomplete`, with each gap named.
pub fn build_report(inputs: &ReportInputs) -> Value {
    let mut gaps = Vec::new();
    // Evidence is only current for the commit it names: another commit's is
    // stale (not shown), and evidence naming no commit cannot be verified.
    let stale = |head: &Option<String>, what: &str| match head {
        Some(named) if named != &inputs.head_sha => Some(InputError::Stale(format!("{what} belongs to commit {named}, not {}", inputs.head_sha))),
        _ => None,
    };
    let tests_in = match &inputs.tests {
        Ok(evidence) => stale(&evidence.head_sha, "test evidence").map_or(Ok(evidence), Err),
        Err(error) => Err(error.clone()),
    };
    let (tests, tests_known) = match &tests_in {
        Ok(evidence) => {
            if evidence.head_sha.is_none() {
                gaps.push("test evidence is not bound to a commit".to_string());
            }
            (
                json!({
                    "status": evidence.status.as_str(),
                    "commands": evidence.commands.iter().map(|c| json!({"command": c.command, "exit_code": c.exit_code})).collect::<Vec<_>>(),
                }),
                matches!(evidence.status, TestStatus::Passed | TestStatus::Failed),
            )
        }
        Err(error) => {
            gaps.push(match error {
                InputError::Stale(_) => why(error),
                _ => format!("test evidence {}", why(error)),
            });
            (json!({"status": "unknown", "commands": [], "reason": why(error)}), false)
        }
    };
    if tests_in.is_ok() && !tests_known {
        gaps.push("test evidence does not show a result".to_string());
    }
    let review_in = match &inputs.review {
        Ok(input) => stale(&input.head_sha, "review details").map_or(Ok(input), Err),
        Err(error) => Err(error.clone()),
    };
    let review = match &review_in {
        Ok(input) => {
            if input.head_sha.is_none() {
                gaps.push("review details are not bound to a commit".to_string());
            }
            json!({"status": "available", "rounds_observed": input.rounds_observed, "outcome": input.outcome})
        }
        Err(error) => {
            gaps.push(match error {
                InputError::Stale(_) => why(error),
                _ => format!("review details {}", why(error)),
            });
            json!({"status": "unavailable", "reason": why(error), "rounds_observed": null, "outcome": null})
        }
    };
    let sum = |pick: fn(&ReportFile) -> Option<u64>| inputs.files.iter().filter_map(pick).sum::<u64>();
    let files: Vec<Value> = inputs
        .files
        .iter()
        .map(|f| json!({"path": f.path, "old_path": f.old_path, "added": f.added, "deleted": f.deleted, "binary": f.added.is_none() && f.deleted.is_none()}))
        .collect();
    json!({
        "schema_version": REPORT_SCHEMA_VERSION,
        "report_id": format!("{}-{}", inputs.run_id, inputs.head_sha.get(..12).unwrap_or(&inputs.head_sha)),
        "run_id": inputs.run_id,
        "project_id": inputs.project_id,
        "feature_id": inputs.feature_id,
        "base_sha": inputs.base_sha,
        "head_sha": inputs.head_sha,
        "status": if gaps.is_empty() { "current" } else { "incomplete" },
        "generator": {"name": "orq", "version": env!("CARGO_PKG_VERSION"), "method": "deterministic"},
        "generated_at": inputs.generated_at,
        "summary": {
            "files_changed": inputs.files.len(),
            "added": sum(|f| f.added),
            "deleted": sum(|f| f.deleted),
            "binary_files": inputs.files.iter().filter(|f| f.added.is_none() && f.deleted.is_none()).count(),
        },
        "files": files,
        "tests": tests,
        "review": review,
        "risks": [],
        "gaps": gaps,
    })
}

/// A readable rendering of [`build_report`]'s output. Pure: the same report
/// always renders the same text.
pub fn render_markdown(report: &Value) -> String {
    // Paths, commands and ids come from repositories and test output: control
    // characters are shown escaped so they can neither drive the terminal nor
    // start new report lines. The structured report keeps the originals.
    let text = |value: &Value| {
        value.as_str().unwrap_or("-").chars().flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] }).collect::<String>()
    };
    let mut md = String::new();
    md.push_str(&format!("# Review report {}\n\n", text(&report["report_id"])));
    md.push_str(&format!("- State: {}\n", text(&report["status"])));
    md.push_str(&format!("- Run: {} ({} / {})\n", text(&report["run_id"]), text(&report["project_id"]), text(&report["feature_id"])));
    md.push_str(&format!("- Reviewed commit: {}\n- Base: {}\n", text(&report["head_sha"]), text(&report["base_sha"])));
    md.push_str(&format!("- Generated: {}\n\n", text(&report["generated_at"])));
    let summary = &report["summary"];
    md.push_str(&format!(
        "## Files\n\n{} changed, +{} -{}, {} binary\n\n",
        summary["files_changed"], summary["added"], summary["deleted"], summary["binary_files"]
    ));
    for file in report["files"].as_array().into_iter().flatten() {
        let counts = if file["binary"] == true { "binary".to_string() } else { format!("+{} -{}", file["added"], file["deleted"]) };
        match file["old_path"].as_str() {
            Some(_) => md.push_str(&format!("- {} (renamed from {}) {counts}\n", text(&file["path"]), text(&file["old_path"]))),
            None => md.push_str(&format!("- {} {counts}\n", text(&file["path"]))),
        }
    }
    md.push_str(&format!("\n## Tests\n\nStatus: {}\n", text(&report["tests"]["status"])));
    for command in report["tests"]["commands"].as_array().into_iter().flatten() {
        md.push_str(&format!("- `{}` exit {}\n", text(&command["command"]), command["exit_code"]));
    }
    md.push_str(&format!("\n## Review\n\nStatus: {}\n", text(&report["review"]["status"])));
    match report["review"]["status"].as_str() {
        Some("available") => {
            let rounds = report["review"]["rounds_observed"].as_u64().map_or("unknown".to_string(), |n| n.to_string());
            md.push_str(&format!("- Rounds observed: {rounds}\n- Outcome: {}\n", text(&report["review"]["outcome"])));
        }
        _ => md.push_str(&format!("- Reason: {}\n", text(&report["review"]["reason"]))),
    }
    md.push_str("\n## Risks\n\n");
    if let Some(files) = report["file_risks"]["files"].as_array() {
        md.push_str("Files needing attention:\n");
        for file in files.iter().filter(|f| f["level"] != "low") {
            md.push_str(&format!("- {} - {}, {} risk ({})\n", text(&file["path"]), text(&file["category"]), text(&file["level"]), text(&file["evidence"])));
        }
        md.push('\n');
    }
    match report["risks"].as_array().filter(|r| !r.is_empty()) {
        None => md.push_str("No findings are recorded in this report.\n"),
        Some(risks) => {
            for risk in risks {
                let place = match (risk["file"].as_str(), risk["line"].as_u64()) {
                    (Some(_), Some(line)) => format!(" {}:{line}", text(&risk["file"])),
                    (Some(_), None) => format!(" {}", text(&risk["file"])),
                    _ => String::new(),
                };
                let state = if risk["unresolved"] == true { "unresolved" } else { "resolved" };
                let outdated = if risk["reference"] == "outdated" { ", outdated reference" } else { "" };
                md.push_str(&format!(
                    "- [{}] {} {}{place}: {} ({}; {state}{outdated})\n",
                    text(&risk["standing"]), text(&risk["severity"]), text(&risk["category"]), text(&risk["claim"]), text(&risk["evidence"])
                ));
                if let Some(link) = risk["link"].as_str() {
                    md.push_str(&format!("  link: {}\n", text(&Value::String(link.to_string()))));
                }
            }
        }
    }
    if let Some(rounds) = report["rounds"].as_array().filter(|r| !r.is_empty()) {
        md.push_str("\nReview rounds:\n");
        for round in rounds {
            let line = match round["text"].as_str() {
                Some(_) => text(&round["text"]),
                None => format!(
                    "Round {}: {} findings, {} confirmed, {} potential, {} dismissed, {} unresolved",
                    round["round"], round["findings"], round["confirmed"], round["potential"], round["dismissed"], round["unresolved"]
                ),
            };
            md.push_str(&format!(
                "- {line}{}\n",
                round["summary"].as_str().map_or(String::new(), |_| format!(" - {}", text(&round["summary"])))
            ));
        }
    }
    if report["generated_summary"]["generated"] == true {
        md.push_str(&format!(
            "\n## Summary (generated by {})\n\n{}\n",
            text(&report["generated_summary"]["model"]),
            text(&report["generated_summary"]["text"])
        ));
    }
    let gaps = report["gaps"].as_array().cloned().unwrap_or_default();
    if !gaps.is_empty() {
        md.push_str("\n## Gaps\n\n");
        for gap in gaps {
            md.push_str(&format!("- {}\n", text(&gap)));
        }
    }
    md
}
