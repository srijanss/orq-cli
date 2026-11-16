//! What the review inbox shows, read from the stored reports: one row per
//! run (its latest report) and the sections of one report. Everything here is
//! read-only; nothing is rewritten or refreshed from a provider.

use std::path::Path;

use serde_json::Value;

use crate::review_lifecycle::{Observation, ProviderState, ReviewLifecycle, LOCAL_PROVIDER};
use crate::storage::{check_run_id, StorageRoots};
use crate::store::{check_report_path, ReviewRecord, Store};

/// One line of the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxRow {
    pub run_id: String,
    pub feature: String,
    pub project: String,
    /// Whether the review belongs to the project the user is in, decided by
    /// project id (two projects may share a display name).
    pub current: bool,
    pub status: String,
    /// The highest severity among the report's open findings; `resolved` when it has findings and
    /// every one is fixed; `-` when it has none. A finding that does not say whether it is resolved
    /// counts as open.
    pub risk: String,
    /// When `risk` is `resolved`, the highest severity that was fixed; otherwise `None`.
    pub fixed_risk: Option<String>,
    pub stale: bool,
    /// When the report was generated, as plain UTC text (`2026-11-03T10:20:30Z`); `None` when absent or odd.
    pub generated_at: Option<String>,
}

/// The sections of one review, as plain lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxDetail {
    pub summary: Vec<String>,
    pub risks: Vec<String>,
    pub files: Vec<String>,
    pub tests: Vec<String>,
    pub rounds: Vec<String>,
    /// As on [`InboxRow`].
    pub generated_at: Option<String>,
}

/// Every stored review, newest report per run. `current_project_id` names the
/// project the user is in; its rows show `current_project_name`, others show
/// their id without the trailing hash. A missing database is an empty inbox
/// and is not created.
pub fn inbox(roots: &StorageRoots, current_project_id: &str, current_project_name: &str) -> Result<Vec<InboxRow>, String> {
    rows(roots, current_project_id, current_project_name, false)
}

/// Like [`inbox`], for the reviews of one project only. Other projects' records are dropped before
/// any report or lifecycle is read, so they cost nothing.
pub fn inbox_of_project(roots: &StorageRoots, project_id: &str, project_name: &str) -> Result<Vec<InboxRow>, String> {
    rows(roots, project_id, project_name, true)
}

fn rows(roots: &StorageRoots, current_project_id: &str, current_project_name: &str, only_current: bool) -> Result<Vec<InboxRow>, String> {
    if !roots.database().exists() {
        return Ok(Vec::new());
    }
    let store = Store::open_read_only(&roots.database()).map_err(|err| format!("cannot open the review index: {err}"))?;
    let mut latest = latest_per_run(store.all_reviews().map_err(|err| format!("cannot read the review index: {err}"))?);
    if only_current {
        latest.retain(|record| record.project == current_project_id);
    }
    let mut rows = Vec::new();
    for record in latest {
        let report = read_report(&roots.data, &record);
        // A lifecycle that cannot be read is unknown, never treated as "no lifecycle".
        let lifecycles = store.lifecycles_for_run(&record.run_id);
        let lifecycle = lifecycles.as_ref().ok().and_then(|all| all.iter().rev().find(|l| l.reported_head_sha == record.head_sha));
        let status = row_status(&report, &lifecycles, lifecycle);
        let (risk, fixed_risk) = match &report {
            Ok(report) => open_risk(report),
            Err(_) => ("-".to_string(), None),
        };
        let unknown = lifecycles.is_err();
        rows.push(InboxRow {
            run_id: record.run_id,
            feature: record.feature,
            project: if record.project == current_project_id { current_project_name.to_string() } else { display_project(&record.project) },
            current: record.project == current_project_id,
            status,
            risk,
            fixed_risk,
            stale: unknown || lifecycle.is_some_and(|l| l.stale),
            generated_at: report.as_ref().ok().and_then(generated_at),
        });
    }
    Ok(rows)
}

/// The status a run's inbox row shows: the report's own (`ready`, `draft` or `unreadable`), replaced by
/// what its lifecycle says, or `unknown` when the lifecycles cannot be read.
fn row_status<E>(report: &Result<Value, String>, lifecycles: &Result<Vec<ReviewLifecycle>, E>, lifecycle: Option<&ReviewLifecycle>) -> String {
    let Ok(report) = report else { return "unreadable".to_string() };
    if lifecycles.is_err() {
        return "unknown".to_string();
    }
    match lifecycle.and_then(ReviewLifecycle::status) {
        Some(shown) => shown.to_string(),
        None if report["status"] == "current" => "ready".to_string(),
        None => "draft".to_string(),
    }
}

/// The status the inbox row of `run_id` shows, read from that run alone; `None` when it has no report.
pub fn status_of_run(roots: &StorageRoots, run_id: &str) -> Result<Option<String>, String> {
    if !roots.database().exists() {
        return Ok(None);
    }
    let store = Store::open_read_only(&roots.database()).map_err(|err| format!("cannot open the review index: {err}"))?;
    let records = store.reviews_for_run(run_id).map_err(|err| format!("cannot read the review index: {err}"))?;
    let Some(record) = records.last() else { return Ok(None) };
    let report = read_report(&roots.data, record);
    let lifecycles = store.lifecycles_for_run(run_id);
    let lifecycle = lifecycles.as_ref().ok().and_then(|all| all.iter().rev().find(|l| l.reported_head_sha == record.head_sha));
    Ok(Some(row_status(&report, &lifecycles, lifecycle)))
}

/// The latest of `records` for each run: later records replace earlier ones for the same run, and runs
/// keep the order they were first seen in.
fn latest_per_run(records: Vec<ReviewRecord>) -> Vec<ReviewRecord> {
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
    latest
}

/// How many runs of `project` have a latest report still waiting on a person: no lifecycle for that
/// report's head yet, or one that is neither merged, closed nor resolved. A lifecycle that cannot be
/// read is an error, never a guess.
pub fn waiting_count(store: &Store, project: &str) -> Result<usize, String> {
    let mut waiting = 0;
    let records = store.reviews_for_project(project).map_err(|err| format!("cannot read the review index: {err}"))?;
    for record in latest_per_run(records) {
        let lifecycles = store.lifecycles_for_run(&record.run_id).map_err(|err| format!("cannot read the provider review state: {err}"))?;
        if lifecycles.iter().rev().find(|l| l.reported_head_sha == record.head_sha).is_none_or(ReviewLifecycle::waiting) {
            waiting += 1;
        }
    }
    Ok(waiting)
}

/// Records that `run_id` was merged into its base in this repository at `merged_sha`: every report of
/// the run, the merged head's and any older one it superseded, is resolved as merged locally. A head
/// that already has a provider's lifecycle, or was merged locally by an earlier pass, keeps it, so a
/// repeat saves nothing; one a person resolved or dismissed here becomes merged. Returns how many
/// lifecycles were saved.
pub fn record_local_merge(store: &Store, run_id: &str, merged_sha: &str) -> Result<usize, String> {
    let lifecycles = store.lifecycles_for_run(run_id).map_err(|err| format!("cannot read the provider review state: {err}"))?;
    let mut saved = 0;
    let mut heads: Vec<String> = Vec::new();
    for record in store.reviews_for_run(run_id).map_err(|err| format!("cannot read the review index: {err}"))? {
        if !heads.contains(&record.head_sha) {
            heads.push(record.head_sha);
        }
    }
    for head in heads {
        if lifecycles.iter().any(|l| l.reported_head_sha == head && (l.provider != LOCAL_PROVIDER || l.state == ProviderState::Merged)) {
            continue;
        }
        store.save_lifecycle(run_id, &ReviewLifecycle::merged_locally(&head, merged_sha)).map_err(|err| format!("cannot save the review state: {err}"))?;
        saved += 1;
    }
    Ok(saved)
}

/// Records each `(run, merged commit)` of runs merged locally before merges resolved their reviews.
/// Runs without a report, and reviews already settled, are left alone, so a repeat changes nothing.
/// Returns how many runs changed.
pub fn backfill_local_merges(store: &Store, merges: &[(String, String)]) -> Result<usize, String> {
    let mut changed = 0;
    for (run_id, merged_sha) in merges {
        if record_local_merge(store, run_id, merged_sha)? > 0 {
            changed += 1;
        }
    }
    Ok(changed)
}

/// A PR/MR a run created or adopted, as its run's events record it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedReview {
    pub run_id: String,
    pub provider: String,
    pub review_id: String,
    pub url: String,
}

/// Records the PR/MR a run just created or adopted for its pushed `head` as seen open there now, so it
/// reads as current. The same review already kept for that head is updated, keeping a person's
/// "reviewed" on it; anything else kept for that head is replaced.
pub fn record_created_review(store: &Store, review: &CreatedReview, head: &str) -> Result<(), String> {
    let kept = store.lifecycles_for_run(&review.run_id).map_err(|err| format!("cannot read the provider review state: {err}"))?;
    let base = kept
        .into_iter()
        .find(|l| l.reported_head_sha == head && l.provider == review.provider && l.review_id == review.review_id)
        .unwrap_or_else(|| ReviewLifecycle::associate(&review.provider, &review.review_id, &review.url, head));
    let seen = base.apply(Ok(Observation { state: ProviderState::Open, head_sha: head.to_string() }));
    store.save_lifecycle(&review.run_id, &seen).map_err(|err| format!("cannot save the review state: {err}"))
}

/// Keeps the PR/MR a run adopted for its pushed `head`. An adopted review can already be merged, so it is
/// kept unverified until it is looked at; one already kept on that head is left as it is.
pub fn record_adopted_review(store: &Store, review: &CreatedReview, head: &str) -> Result<(), String> {
    let kept = store.lifecycles_for_run(&review.run_id).map_err(|err| format!("cannot read the provider review state: {err}"))?;
    if kept.iter().any(|l| l.reported_head_sha == head && l.provider == review.provider && l.review_id == review.review_id) {
        return Ok(());
    }
    let lifecycle = ReviewLifecycle::associate(&review.provider, &review.review_id, &review.url, head);
    store.save_lifecycle(&review.run_id, &lifecycle).map_err(|err| format!("cannot save the review state: {err}"))
}

/// Records each run's created or adopted PR/MR, from before creation recorded it, as the provider review
/// of the run's latest report: unverified until it is looked at. A run without a report, one whose review
/// is already kept, and a latest report a person already acted on here are left alone, so a repeat
/// changes nothing. Returns how many runs changed.
pub fn backfill_created_reviews(store: &Store, created: &[CreatedReview]) -> Result<usize, String> {
    let mut changed = 0;
    for review in created {
        let reports = store.reviews_for_run(&review.run_id).map_err(|err| format!("cannot read the review index: {err}"))?;
        let Some(head) = reports.last().map(|r| r.head_sha.clone()) else { continue };
        let lifecycles = store.lifecycles_for_run(&review.run_id).map_err(|err| format!("cannot read the provider review state: {err}"))?;
        if lifecycles.iter().any(|l| l.provider != LOCAL_PROVIDER || l.reported_head_sha == head) {
            continue;
        }
        let lifecycle = ReviewLifecycle::associate(&review.provider, &review.review_id, &review.url, &head);
        store.save_lifecycle(&review.run_id, &lifecycle).map_err(|err| format!("cannot save the review state: {err}"))?;
        changed += 1;
    }
    Ok(changed)
}

/// What a person can do to a review from the inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    /// Mark it reviewed: it stops waiting.
    Resolve,
    /// Set it aside without merging: it is closed and stops waiting.
    Dismiss,
    /// Undo either: it waits again.
    Reopen,
}

impl ReviewAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewAction::Resolve => "resolve",
            ReviewAction::Dismiss => "dismiss",
            ReviewAction::Reopen => "reopen",
        }
    }

    pub fn parse(name: &str) -> Option<ReviewAction> {
        [ReviewAction::Resolve, ReviewAction::Dismiss, ReviewAction::Reopen].into_iter().find(|a| a.as_str() == name)
    }

    /// The kind of the run event that records the action.
    pub fn event_kind(self) -> &'static str {
        match self {
            ReviewAction::Resolve => "review.resolved",
            ReviewAction::Dismiss => "review.dismissed",
            ReviewAction::Reopen => "review.reopened",
        }
    }
}

/// How an action on a review ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    /// The review's lifecycle is now this one.
    Done(ReviewLifecycle),
    /// The run has no stored report.
    NoReview,
    /// The person acted on an older report than the run's latest, written for `latest`.
    StaleHead { latest: String },
    /// The action does not apply to the review as it stands.
    Refused(String),
}

/// Apply `action` to the review of `run_id`'s latest report. With `expected_head`, the action only
/// applies while that is still the latest report's head, so a person never acts on a report they did not
/// see. A report with no lifecycle yet gets one kept in this repository.
pub fn act_on_review(store: &Store, run_id: &str, action: ReviewAction, expected_head: Option<&str>) -> Result<ActionOutcome, String> {
    // The transition is stored only over the lifecycle it was made from, and only while its head is still
    // the latest report's, under one write lock; when a local merge, a refresh or a newer report came in
    // between, the action is decided again from what is stored now.
    loop {
        let records = store.reviews_for_run(run_id).map_err(|err| format!("cannot read the review index: {err}"))?;
        let Some(head) = records.last().map(|r| r.head_sha.clone()) else { return Ok(ActionOutcome::NoReview) };
        if expected_head.is_some_and(|seen| seen != head) {
            return Ok(ActionOutcome::StaleHead { latest: head });
        }
        let lifecycles = store.lifecycles_for_run(run_id).map_err(|err| format!("cannot read the provider review state: {err}"))?;
        let current = lifecycles.into_iter().rev().find(|l| l.reported_head_sha == head);
        let base = current.clone().unwrap_or_else(|| ReviewLifecycle::local(&head));
        let next = match action {
            ReviewAction::Resolve => Ok(base.resolved()),
            ReviewAction::Dismiss => base.dismissed(),
            ReviewAction::Reopen => base.reopened(),
        };
        let next = match next {
            Ok(next) => next,
            Err(reason) => return Ok(ActionOutcome::Refused(reason)),
        };
        if store.replace_lifecycle(run_id, current.as_ref(), &next).map_err(|err| format!("cannot save the review state: {err}"))? {
            return Ok(ActionOutcome::Done(next));
        }
    }
}

/// The sections of the latest stored report of `run_id`.
pub fn detail(roots: &StorageRoots, run_id: &str) -> Result<InboxDetail, String> {
    detail_where(roots, run_id, |_| true)
}

/// The sections of the stored report of `run_id` written for `head_sha`.
pub fn detail_at(roots: &StorageRoots, run_id: &str, head_sha: &str) -> Result<InboxDetail, String> {
    detail_where(roots, run_id, |record| record.head_sha == head_sha)
}

/// The sections of the latest stored report of `run_id` together with the verified permalink of each
/// risk, read from one report so a link always belongs to the risk at its position. A link that is not
/// a plain `https` address is dropped.
pub fn detail_with_links(roots: &StorageRoots, run_id: &str) -> Result<(InboxDetail, Vec<Option<String>>), String> {
    let report = report_where(roots, run_id, |_| true)?;
    let links = report["risks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| r["link"].as_str().filter(|l| l.starts_with("https://") && !l.chars().any(|c| c.is_control() || c.is_whitespace())).map(str::to_string))
        .collect();
    Ok((sections(&report), links))
}

/// The time a report says it was generated, when it is a plain UTC time
/// (`2026-11-03T10:20:30Z`); anything else is `None`.
fn generated_at(report: &Value) -> Option<String> {
    let text = report["generated_at"].as_str()?;
    let b = text.as_bytes();
    let shape = b.len() == 20 && b.iter().enumerate().all(|(i, c)| match i {
        4 | 7 => *c == b'-',
        10 => *c == b'T',
        13 | 16 => *c == b':',
        19 => *c == b'Z',
        _ => c.is_ascii_digit(),
    });
    shape.then(|| text.to_string())
}

fn detail_where(roots: &StorageRoots, run_id: &str, wanted: impl Fn(&ReviewRecord) -> bool) -> Result<InboxDetail, String> {
    Ok(sections(&report_where(roots, run_id, wanted)?))
}

fn report_where(roots: &StorageRoots, run_id: &str, wanted: impl Fn(&ReviewRecord) -> bool) -> Result<Value, String> {
    check_run_id(run_id).map_err(|err| format!("invalid run id: {err}"))?;
    if !roots.database().exists() {
        return Err(format!("no review report for run {run_id}"));
    }
    let store = Store::open_read_only(&roots.database()).map_err(|err| format!("cannot open the review index: {err}"))?;
    let records = store.reviews_for_run(run_id).map_err(|err| format!("cannot read the review index: {err}"))?;
    let record = records.iter().rev().find(|r| wanted(r)).ok_or_else(|| format!("no review report for run {run_id}"))?;
    read_report(&roots.data, record).map_err(|why| format!("the report for run {run_id} is unreadable: {why}"))
}

/// The most a stored report may be before it is refused.
const MAX_REPORT_BYTES: u64 = 8 * 1024 * 1024;

/// Reads a stored report. The path comes from the index, so it is checked on
/// every read: it must sit under its own run's directory, and it is opened one
/// component at a time from the data root without following a symlink at any
/// step, so no directory swapped in underneath can lead outside it.
fn read_report(data_root: &Path, record: &ReviewRecord) -> Result<Value, String> {
    use rustix::fs::{open, openat, Mode, OFlags};
    use std::io::Read;
    use std::path::Component;
    const NOT_VALID: &str = "the stored report path is not valid";
    check_report_path(&record.run_id, &record.report_path).map_err(|_| NOT_VALID.to_string())?;
    let parts: Vec<&std::ffi::OsStr> = Path::new(&record.report_path)
        .components()
        .filter_map(|c| if let Component::Normal(part) = c { Some(part) } else { None })
        .collect();
    let (name, dirs) = parts.split_last().ok_or_else(|| NOT_VALID.to_string())?;
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut dir = open(data_root, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).map_err(|_| NOT_VALID.to_string())?;
    for part in dirs {
        dir = openat(&dir, *part, dir_flags, Mode::empty()).map_err(|_| NOT_VALID.to_string())?;
    }
    let fd = openat(&dir, *name, OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK, Mode::empty())
        .map_err(|_| "the stored report cannot be opened".to_string())?;
    let file = std::fs::File::from(fd);
    if !file.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err("the stored report is not a regular file".to_string());
    }
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

/// The row's risk and, when every finding is fixed, the highest fixed severity (see [`InboxRow`]).
fn open_risk(report: &Value) -> (String, Option<String>) {
    let risks: Vec<&Value> = report["risks"].as_array().into_iter().flatten().collect();
    // Only `unresolved: false` marks a finding as fixed; anything else leaves it open.
    let (fixed, open): (Vec<&Value>, Vec<&Value>) = risks.into_iter().partition(|r| r["unresolved"] == Value::Bool(false));
    if !open.is_empty() {
        return (highest_severity(&open).unwrap_or_else(|| "-".to_string()), None);
    }
    match highest_severity(&fixed) {
        Some(level) => ("resolved".to_string(), Some(level)),
        None => ("-".to_string(), None),
    }
}

fn highest_severity(risks: &[&Value]) -> Option<String> {
    let rank = |level: &str| ["critical", "high", "medium", "low"].iter().position(|l| *l == level).unwrap_or(4);
    risks.iter().filter_map(|r| r["severity"].as_str()).min_by_key(|level| rank(level)).map(str::to_string)
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
    InboxDetail { summary, risks, files, tests, rounds, generated_at: generated_at(report) }
}

