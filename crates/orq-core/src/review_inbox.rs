//! What the review inbox shows, read from the stored reports: one row per
//! run (its latest report) and the sections of one report. Everything here is
//! read-only; nothing is rewritten or refreshed from a provider.

use std::path::Path;

use serde_json::Value;

use crate::storage::{check_run_id, StorageRoots};
use crate::store::{check_report_path, ReviewRecord, Store};

/// One line of the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxRow {
    pub run_id: String,
    pub feature: String,
    pub project: String,
    pub status: String,
    pub risk: String,
    pub stale: bool,
}

/// The sections of one review, as plain lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxDetail {
    pub summary: Vec<String>,
    pub risks: Vec<String>,
    pub files: Vec<String>,
    pub tests: Vec<String>,
    pub rounds: Vec<String>,
}

/// Every stored review, newest report per run. `current_project_id` names the
/// project the user is in; its rows show `current_project_name`, others show
/// their id without the trailing hash. A missing database is an empty inbox
/// and is not created.
pub fn inbox(roots: &StorageRoots, current_project_id: &str, current_project_name: &str) -> Result<Vec<InboxRow>, String> {
    if !roots.database().exists() {
        return Ok(Vec::new());
    }
    let store = Store::open_read_only(&roots.database()).map_err(|err| format!("cannot open the review index: {err}"))?;
    let records = store.all_reviews().map_err(|err| format!("cannot read the review index: {err}"))?;
    // Later records replace earlier ones for the same run; runs keep the
    // order they were first seen in.
    let mut latest: Vec<ReviewRecord> = Vec::new();
    let mut position: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for record in records {
        match position.get(&record.run_id) {
            Some(&at) => latest[at] = record,
            None => {
                position.insert(record.run_id.clone(), latest.len());
                latest.push(record);
            }
        }
    }
    let mut rows = Vec::new();
    for record in latest {
        let report = read_report(&roots.data, &record);
        // A lifecycle that cannot be read is unknown, never treated as "no lifecycle".
        let lifecycles = store.lifecycles_for_run(&record.run_id);
        let lifecycle = lifecycles.as_ref().ok().and_then(|all| all.iter().rev().find(|l| l.reported_head_sha == record.head_sha));
        let (mut status, risk) = match &report {
            Ok(report) => (
                if report["status"] == "current" { "ready" } else { "draft" }.to_string(),
                highest_risk(report),
            ),
            Err(_) => ("unreadable".to_string(), "-".to_string()),
        };
        if report.is_ok() {
            if let Some(l) = lifecycle {
                status = l.state.as_str().to_string();
            }
        }
        let unknown = lifecycles.is_err();
        if unknown && report.is_ok() {
            status = "unknown".to_string();
        }
        rows.push(InboxRow {
            run_id: record.run_id,
            feature: record.feature,
            project: if record.project == current_project_id { current_project_name.to_string() } else { display_project(&record.project) },
            status,
            risk,
            stale: unknown || lifecycle.is_some_and(|l| l.stale),
        });
    }
    Ok(rows)
}

/// The sections of the latest stored report of `run_id`.
pub fn detail(roots: &StorageRoots, run_id: &str) -> Result<InboxDetail, String> {
    check_run_id(run_id).map_err(|err| format!("invalid run id: {err}"))?;
    if !roots.database().exists() {
        return Err(format!("no review report for run {run_id}"));
    }
    let store = Store::open_read_only(&roots.database()).map_err(|err| format!("cannot open the review index: {err}"))?;
    let records = store.reviews_for_run(run_id).map_err(|err| format!("cannot read the review index: {err}"))?;
    let record = records.last().ok_or_else(|| format!("no review report for run {run_id}"))?;
    let report = read_report(&roots.data, record).map_err(|why| format!("the report for run {run_id} is unreadable: {why}"))?;
    Ok(sections(&report))
}

/// The most a stored report may be before it is refused.
const MAX_REPORT_BYTES: u64 = 8 * 1024 * 1024;

/// Reads a stored report. The path comes from the index, so it is checked on
/// every read: it must sit under its own run's directory, resolve inside the
/// data root, and not itself be a symlink.
fn read_report(data_root: &Path, record: &ReviewRecord) -> Result<Value, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    check_report_path(&record.run_id, &record.report_path).map_err(|_| "the stored report path is not valid".to_string())?;
    let full = data_root.join(&record.report_path);
    let root = data_root.canonicalize().map_err(|err| err.to_string())?;
    let resolved = full.canonicalize().map_err(|err| err.to_string())?;
    if !resolved.starts_with(&root) {
        return Err("the stored report path is not valid".to_string());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&full)
        .map_err(|_| "the stored report cannot be opened".to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes).map_err(|err| err.to_string())?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return Err("the report is too large".to_string());
    }
    serde_json::from_slice(&bytes).map_err(|err| err.to_string())
}

/// A project id is `<name>-<8 hex digits>`; the hash means nothing to a reader.
fn display_project(id: &str) -> String {
    match id.rsplit_once('-') {
        Some((name, hash)) if !name.is_empty() && hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => name.to_string(),
        _ => id.to_string(),
    }
}

fn highest_risk(report: &Value) -> String {
    let rank = |level: &str| ["critical", "high", "medium", "low"].iter().position(|l| *l == level).unwrap_or(4);
    report["risks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["severity"].as_str())
        .min_by_key(|level| rank(level))
        .unwrap_or("-")
        .to_string()
}

fn text(value: &Value) -> String {
    value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())
}

fn sections(report: &Value) -> InboxDetail {
    let count = |key: &str| report["summary"][key].as_u64().unwrap_or(0);
    let mut summary = vec![
        format!("{} files changed, +{} -{}", count("files_changed"), count("added"), count("deleted")),
        format!("Report status: {}", text(&report["status"])),
    ];
    summary.extend(report["gaps"].as_array().into_iter().flatten().map(|g| format!("Gap: {}", text(g))));
    let risks = report["risks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let place = match (r["file"].as_str(), r["line"].as_u64()) {
                (Some(file), Some(line)) => format!(" {file}:{line}"),
                (Some(file), None) => format!(" {file}"),
                _ => String::new(),
            };
            format!("{}{place} {}", text(&r["severity"]), text(&r["claim"]))
        })
        .collect();
    let files = report["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| match (f["added"].as_u64(), f["deleted"].as_u64()) {
            (Some(a), Some(d)) => format!("{}  +{a} -{d}", text(&f["path"])),
            _ => format!("{}  binary", text(&f["path"])),
        })
        .collect();
    let mut tests = vec![format!("Tests: {}", text(&report["tests"]["status"]))];
    if let Some(reason) = report["tests"]["reason"].as_str() {
        tests.push(format!("Why: {reason}"));
    }
    tests.extend(report["tests"]["commands"].as_array().into_iter().flatten().map(|c| format!("{} (exit {})", text(&c["command"]), text(&c["exit_code"]))));
    let rounds = report["rounds"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let summary = r["summary"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or("no summary recorded");
            format!("round {}: {summary} ({} findings, {} unresolved)", text(&r["round"]), text(&r["findings"]), text(&r["unresolved"]))
        })
        .collect();
    InboxDetail { summary, risks, files, tests, rounds }
}
