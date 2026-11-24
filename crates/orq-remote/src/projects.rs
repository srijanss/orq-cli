//! Projects the server can show: the git repositories directly inside the scan root
//! (`~/code` by default). Symlinks are never followed, so nothing outside the root is read.

use std::path::{Path, PathBuf};

use orq_core::retention::PrunePlan;

mod cleanup;
mod plan;
mod push_target;
mod queue;
pub use queue::release::{HarnessAgent, ReleaseAgent};
pub use queue::{action_label, change_queue, project_queue_json, queue_json, start_queue_rounds, QueueRound, QueueRounds, QUEUE_ROUND_EVERY};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

/// The scan root: an explicit override (for tests), else `code` inside the home directory.
pub fn default_root(home: Option<&Path>, override_root: Option<&Path>) -> PathBuf {
    match (override_root, home) {
        (Some(root), _) => root.to_path_buf(),
        (None, Some(home)) => home.join("code"),
        (None, None) => PathBuf::from("code"),
    }
}

/// Scans the root now. A symlinked root is allowed on purpose and is resolved once; every project
/// is then checked to still resolve to a direct child of that real root, so a folder swapped for a
/// link to somewhere else is dropped. A missing or unreadable root, or entry, gives no project.
pub fn discover(root: &Path) -> Vec<Project> {
    let Ok(real_root) = std::fs::canonicalize(root) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    collect(&real_root, "", GROUP_DEPTH, &mut found);
    found.sort();
    found.into_iter().map(|(name, path)| Project { id: project_id(&name), name, path }).collect()
}

/// What a run's event log said when it was last read, and the size and time it had then.
struct LogSummary {
    len: u64,
    modified: Option<std::time::SystemTime>,
    state: &'static str,
    ending: Option<&'static str>,
    /// Why a blocked or failed run ended, as its `run.completed` said (redacted).
    ending_reason: Option<String>,
    resumed_as: Option<String>,
    resumed_from: Option<String>,
    dirty_files: Option<Vec<String>>,
    /// Whether the queue started the run unattended.
    unattended: bool,
    /// Whether the run is a planning run (Draft with Claude) rather than a feature run.
    plan: bool,
    stage: Option<String>,
    started: Option<String>,
    updated: Option<String>,
}

type LogCache = std::collections::HashMap<String, LogSummary>;

/// One listed run: its sort time, id, JSON row (with the state its log gives) and record. `None` when the run belongs to another project
/// or was removed after its record was read; an unreadable event log is an error. A log whose size
/// and time are what they were is not read again (the log only ever grows), so polling the list
/// costs a stat per run, not a replay of its history.
fn run_row(store: &orq_orchestrator::RunStore, state: &Path, cache: &mut LogCache, record: orq_orchestrator::RunRecord, key: &str) -> Result<Option<ListedRun>, String> {
    use orq_orchestrator::StoreError;
    if record.project_id != key {
        return Ok(None);
    }
    let stat = std::fs::metadata(state.join("runs").join(&record.run_id).join("events.jsonl")).ok().map(|meta| (meta.len(), meta.modified().ok()));
    let fresh = stat.is_some_and(|(len, modified)| cache.get(&record.run_id).is_some_and(|known| known.len == len && known.modified == modified && modified.is_some()));
    if !fresh {
        let events = match store.events(&record.run_id) {
            Ok(events) => events,
            Err(StoreError::NotFound(_)) => {
                cache.remove(&record.run_id);
                return Ok(None);
            }
            Err(error) => return Err(format!("run {} is unreadable: {error:?}", record.run_id)),
        };
        let summary = orq_orchestrator::summarize_run(&events);
        let learned = LogSummary {
            len: stat.map_or(0, |(len, _)| len),
            modified: stat.and_then(|(_, modified)| modified),
            state: listed_state(&summary),
            ending: delivered_ending(&events, summary.phase),
            ending_reason: ending_reason(&events, &summary),
            resumed_as: handed_over_to(&summary),
            resumed_from: resumed_from(&events),
            dirty_files: dirty_files(&events, summary.phase),
            unattended: orq_orchestrator::was_unattended(&events),
            plan: plan::is_plan(&events),
            stage: summary.stage,
            started: events.first().map(|event| event.occurred_at.clone()),
            updated: events.last().map(|event| event.occurred_at.clone()),
        };
        if stat.is_some() {
            cache.insert(record.run_id.clone(), learned);
        } else {
            cache.remove(&record.run_id);
            return Ok(Some(row_of(record, &learned)));
        }
    }
    let log = &cache[&record.run_id];
    Ok(Some(row_of(record, log)))
}

/// How a completed run that delivered its work without a review ended: `pushed` or `merged`
/// (into its base, locally). `None` for any other run, a run that ended in a review among them.
fn delivered_ending(events: &[orq_orchestrator::Event], phase: orq_orchestrator::RunPhase) -> Option<&'static str> {
    if phase != orq_orchestrator::RunPhase::Completed {
        return None;
    }
    let completed = events.iter().rev().find(|event| event.kind == "run.completed")?;
    match completed.payload.get("ending").and_then(|ending| ending.as_str()) {
        Some("pushed") => Some("pushed"),
        Some("merged") => Some("merged"),
        _ => None,
    }
}

/// Why a blocked or failed run ended: the `reason` of its last `run.completed`, redacted, when that ended it
/// `blocked` or `failed` and the run still is. `None` for any other run, a run a resume took over among them.
fn ending_reason(events: &[orq_orchestrator::Event], summary: &orq_orchestrator::RunSummary) -> Option<String> {
    use orq_orchestrator::RunPhase;
    if handed_over_to(summary).is_some() || !matches!(summary.phase, RunPhase::Blocked | RunPhase::Failed) {
        return None;
    }
    let completed = events.iter().rev().find(|event| event.kind == "run.completed")?;
    if !matches!(completed.payload.get("ending").and_then(|ending| ending.as_str()), Some("blocked" | "failed")) {
        return None;
    }
    let reason = completed.payload.get("reason")?.as_str()?.trim();
    (!reason.is_empty()).then(|| orq_core::redact::redact(reason))
}

/// What comes before the findings an escalated review left open and an unattended run would not leave for later.
const NOT_LEFT_FOR_LATER: &str = "finding for later: ";

/// The findings a review escalation's reason names, each `{id, severity, category}` as `id (severity, category)`
/// gives them, with `file` and `line` when the worktree's review findings give them. Empty for any other reason.
fn ending_findings(reason: &str, worktree: &Path) -> Vec<serde_json::Value> {
    let Some((_, mut rest)) = reason.split_once(NOT_LEFT_FOR_LATER) else { return Vec::new() };
    let mut named = Vec::new();
    while let Some((id, after)) = rest.split_once(" (") {
        let Some((inside, tail)) = after.split_once(')') else { break };
        let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            break;
        }
        let (severity, category) = match inside.split_once(", ") {
            Some((severity, category)) => (severity, Some(category)),
            None => (inside, None),
        };
        let mut finding = serde_json::json!({"id": id, "severity": severity});
        if let Some(category) = category {
            finding["category"] = category.into();
        }
        named.push(finding);
        match tail.strip_prefix(", ") {
            Some(next) => rest = next,
            None => break,
        }
    }
    if named.is_empty() {
        return named;
    }
    let ledger = read_review_findings(worktree);
    let places = finding_places(ledger.as_ref());
    for finding in &mut named {
        let id = finding["id"].as_str().unwrap_or_default();
        if let Some(Some((file, line))) = places.get(id) {
            finding["file"] = orq_core::redact::redact(file).into();
            if let Some(line) = line {
                finding["line"] = (*line).into();
            }
        }
    }
    named
}

/// Where each finding of a review findings ledger is, by id, read in one pass over every scope: its file and line,
/// or `None` when its scopes disagree on them, as then none of them can be trusted to be the escalated one.
fn finding_places(ledger: Option<&serde_json::Value>) -> std::collections::HashMap<&str, Option<(&str, Option<u64>)>> {
    let mut places = std::collections::HashMap::new();
    let entries = ledger.and_then(|l| l.as_object()).into_iter().flat_map(|scopes| scopes.values()).filter_map(|v| v.as_array()).flatten();
    for entry in entries {
        let (Some(id), Some(file)) = (entry.get("id").and_then(|v| v.as_str()), entry.get("file").and_then(|v| v.as_str()).filter(|file| !file.is_empty())) else { continue };
        let place = (file, entry.get("line").and_then(|v| v.as_u64()));
        places.entry(id).and_modify(|known: &mut Option<_>| if known.is_some_and(|seen| seen != place) { *known = None }).or_insert(Some(place));
    }
    places
}

/// The largest review findings file read; a ledger is a few kilobytes.
const MAX_FINDINGS_BYTES: u64 = 4 * 1024 * 1024;

/// The review findings the session left in `worktree`, or `None` when there are none or they cannot be read. The
/// file is opened without following a link and without waiting on a pipe, and judged by the handle.
fn read_review_findings(worktree: &Path) -> Option<serde_json::Value> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(worktree.join(".tdd-review-findings.json")).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FINDINGS_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_FINDINGS_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// The state a run is listed in: a run a resume took over ended `resumed`, never completed.
fn listed_state(summary: &orq_orchestrator::RunSummary) -> &'static str {
    if handed_over_to(summary).is_some() {
        "resumed"
    } else {
        summary.phase.as_str()
    }
}

/// The run that continued this one, when a resume took it over and so ended it.
fn handed_over_to(summary: &orq_orchestrator::RunSummary) -> Option<String> {
    summary.resumed_as.clone().filter(|_| summary.phase == orq_orchestrator::RunPhase::Completed)
}

/// The run this one was resumed from, as the sessions or the finished agent session it carried over
/// from that run name it; `None` for a run that resumed nothing, or whose carry was never written.
fn resumed_from(events: &[orq_orchestrator::Event]) -> Option<String> {
    events.iter().find_map(|event| {
        let key = match event.kind.as_str() {
            "run.sessions_carried" => "from",
            "stage.completed" => "resumed_from",
            _ => return None,
        };
        event.payload.get(key)?.as_str().map(str::to_string)
    })
}

/// The answer the person who resumed this run gave its session, redacted: kept in the run's logs, and shown only
/// when its `run.launched` says it was resumed with one. `None` for a run given none, or whose answer cannot be read.
fn resume_answer(events: &[orq_orchestrator::Event], logs_dir: &std::path::Path) -> Option<String> {
    use std::io::Read;
    // An answer is at most 8000 characters, each at most four bytes.
    const MOST: u64 = 4 * 8000;
    let launched = events.iter().find(|event| event.kind == "run.launched")?;
    if launched.payload.get("answer").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let (_, file) = open_run_log_with(&logs_dir.join(orq_orchestrator::RESUME_ANSWER_LOG), logs_dir, || {})?;
    let mut bytes = Vec::new();
    file.take(MOST).read_to_end(&mut bytes).ok()?;
    let answer = String::from_utf8_lossy(&bytes);
    (!answer.trim().is_empty()).then(|| orq_core::redact::redact(&answer))
}

/// Adds `resumed_as` and `resumed_from` to a run's JSON, each only when the run has one.
fn add_resume_links(row: &mut serde_json::Value, resumed_as: Option<&String>, resumed_from: Option<&String>) {
    if let Some(id) = resumed_as {
        row["resumed_as"] = id.as_str().into();
    }
    if let Some(id) = resumed_from {
        row["resumed_from"] = id.as_str().into();
    }
}

/// The files a blocked run's last merge attempt found with uncommitted changes in the checkout, when that is
/// what it waits on; `None` for any other run.
fn dirty_files(events: &[orq_orchestrator::Event], phase: orq_orchestrator::RunPhase) -> Option<Vec<String>> {
    if phase != orq_orchestrator::RunPhase::Blocked || !orq_orchestrator::waiting_on_dirty_checkout(events) {
        return None;
    }
    let refused = events.iter().rev().find(|event| event.kind == "merge.refused")?;
    Some(texts(&refused.payload["files"]))
}

/// The strings of a JSON list, redacted; anything else in it is left out.
fn texts(list: &serde_json::Value) -> Vec<String> {
    list.as_array().map(|items| items.iter().filter_map(|item| item.as_str()).map(orq_core::redact::redact).collect()).unwrap_or_default()
}

/// Adds `dirty_files` to a run's JSON when the run waits on uncommitted changes to them.
fn add_dirty_files(row: &mut serde_json::Value, files: Option<&[String]>) {
    if let Some(files) = files {
        row["dirty_files"] = serde_json::json!(files);
    }
}

/// Adds what the run's accepts say to a run's JSON: `accepting_since` while its tests run, `accept_failure`
/// (reason, output tail, when, the name of its own log in `logs_dir` and the tests it failed) after the latest one
/// failed, and `accepted` once a passing accept ended it.
fn add_accept(row: &mut serde_json::Value, events: &[orq_orchestrator::Event], summary: &orq_orchestrator::RunSummary, logs_dir: &std::path::Path) {
    use orq_core::redact::redact;
    if summary.phase == orq_orchestrator::RunPhase::Accepting {
        row["accepting_since"] = serde_json::json!(summary.phase_at);
    }
    if let Some(failure) = &summary.accept_failure {
        let mut shown = serde_json::json!({"reason": redact(&failure.reason), "output": redact(&failure.output), "at": failure.at});
        if let Some(failed) = events.iter().rev().find(|event| event.kind == "run.accept_failed") {
            let log = failed.payload.get("log").and_then(|v| v.as_str());
            if let Some(name) = log.and_then(|log| run_log_name(std::path::Path::new(log), logs_dir)) {
                shown["log"] = serde_json::json!(name);
            }
            add_failed_tests(&mut shown, &failed.payload);
        }
        row["accept_failure"] = shown;
    }
    let accepted = summary.phase == orq_orchestrator::RunPhase::Completed
        && events.iter().rev().find(|event| event.kind == "run.completed").and_then(|event| event.payload.get("note")).and_then(|note| note.as_str()) == Some("accepted: tests passed");
    if accepted {
        row["accepted"] = serde_json::json!(true);
    }
}

/// The most failed tests shown of one failure, as the orchestrator keeps them.
const FAILED_TESTS_SHOWN: usize = 50;
/// The most of one failed test's output shown, in characters.
const FAILED_TEST_OUTPUT_SHOWN: usize = 4000;

/// Adds a failure's `failed_tests` (each `{name, output}`, redacted) and `failed_tests_more` to `shown`; an
/// empty list when its event named none.
fn add_failed_tests(shown: &mut serde_json::Value, payload: &serde_json::Value) {
    use orq_core::redact::redact;
    let text = |test: &serde_json::Value, key: &str| test.get(key).and_then(|v| v.as_str()).map(redact);
    let listed = payload.get("failed_tests").and_then(|v| v.as_array()).map(Vec::as_slice).unwrap_or_default();
    let tests: Vec<_> = listed
        .iter()
        .filter_map(|test| {
            let name = text(test, "name")?;
            let output: String = text(test, "output").unwrap_or_default().chars().take(FAILED_TEST_OUTPUT_SHOWN).collect();
            Some(serde_json::json!({"name": name, "output": output}))
        })
        .take(FAILED_TESTS_SHOWN)
        .collect();
    let more = payload.get("failed_tests_more").and_then(|v| v.as_u64()).unwrap_or(0) + listed.len().saturating_sub(FAILED_TESTS_SHOWN) as u64;
    shown["failed_tests"] = serde_json::json!(tests);
    shown["failed_tests_more"] = serde_json::json!(more);
}

/// Adds `missing_report` (why it could not be built) to a run's JSON when the run ended without its report.
fn add_missing_report(row: &mut serde_json::Value, roots: &orq_core::storage::StorageRoots, run: &str) {
    let database = roots.database();
    let Some(store) = database.exists().then(|| orq_core::store::Store::open_read_only(&database).ok()).flatten() else { return };
    if let Some(missing) = missing_report(roots, &store, run) {
        row["missing_report"] = serde_json::json!({"head_sha": missing.head_sha, "reason": orq_core::redact::redact(&missing.reason)});
    }
}

/// The most a design record (a manifest, a capture or a review answer) may hold before it is left unread.
const DESIGN_RECORD_CAP: u64 = 4 * 1024 * 1024;

/// A design record of the run as JSON: a plain file (never a link) under the cap; `None` otherwise.
fn design_record(path: &Path) -> Option<serde_json::Value> {
    use std::io::Read;
    let meta = path.symlink_metadata().ok().filter(|meta| meta.is_file())?;
    if meta.len() > DESIGN_RECORD_CAP {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|file| file.take(DESIGN_RECORD_CAP).read_to_end(&mut bytes)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A design path as recorded, when it stays inside its folder: relative, with plain segments only.
fn plain_design_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.contains('\\') && !path.chars().any(char::is_control) && path.split('/').all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// The reference images of the run's frozen package, as (viewport, width, height, path from the design folder).
fn design_references(manifest: Option<&serde_json::Value>) -> Vec<(String, u64, u64, String)> {
    let files = manifest.and_then(|manifest| manifest["files"].as_array()).cloned().unwrap_or_default();
    files
        .iter()
        .filter(|file| file["kind"] == "reference")
        .filter_map(|file| {
            let viewport = &file["viewport"];
            let (name, path) = (viewport["name"].as_str()?, file["path"].as_str().filter(|path| plain_design_path(path))?);
            Some((name.to_string(), viewport["width"].as_u64().unwrap_or(0), viewport["height"].as_u64().unwrap_or(0), format!("package/{path}")))
        })
        .collect()
}

/// How many design reviews the run allows at most, as its latest design review event says.
fn design_reviews_of(events: &[orq_orchestrator::Event]) -> Option<u64> {
    events.iter().filter(|event| event.stage.as_deref() == Some("design_review")).filter_map(|event| event.payload.get("reviews_of").or_else(|| event.payload.get("of")).and_then(serde_json::Value::as_u64)).last()
}

/// The highest design review round the run's events name.
fn design_last_round(events: &[orq_orchestrator::Event]) -> u64 {
    events.iter().filter(|event| event.stage.as_deref() == Some("design_review")).filter_map(|event| event.payload.get("round").and_then(serde_json::Value::as_u64)).max().unwrap_or(0)
}

fn shown_text(value: &serde_json::Value) -> serde_json::Value {
    value.as_str().map_or(serde_json::Value::Null, |text| orq_core::redact::redact(text).into())
}

fn shown_texts(value: &serde_json::Value) -> Vec<serde_json::Value> {
    value.as_array().map(|all| all.iter().map(shown_text).filter(|text| !text.is_null()).collect()).unwrap_or_default()
}

/// One design review round as the run page shows it, `None` when it left neither captures nor a review.
fn design_round_shown(dir: &Path, round: u64, references: &[(String, u64, u64, String)]) -> Option<serde_json::Value> {
    use serde_json::json;
    let round_dir = dir.join(format!("round-{round}"));
    let (capture, record) = (design_record(&round_dir.join("capture.json")), design_record(&round_dir.join("review.json")));
    if capture.is_none() && record.is_none() {
        return None;
    }
    let pages = capture.as_ref().and_then(|capture| capture["pages"].as_array()).cloned().unwrap_or_default();
    let mut viewports: Vec<(String, u64, u64, Option<String>)> = references.iter().map(|(name, width, height, path)| (name.clone(), *width, *height, Some(path.clone()))).collect();
    for page in &pages {
        let Some(name) = page["viewport"].as_str() else { continue };
        if !viewports.iter().any(|known| known.0 == name) {
            viewports.push((name.to_string(), page["viewport_width"].as_u64().unwrap_or(0), page["viewport_height"].as_u64().unwrap_or(0), None));
        }
    }
    let viewports: Vec<serde_json::Value> = viewports
        .into_iter()
        .map(|(name, width, height, reference)| {
            let captures: Vec<serde_json::Value> = pages
                .iter()
                .filter(|page| page["viewport"] == name.as_str())
                .map(|page| {
                    let image = page["image"].as_str().filter(|image| plain_design_path(image)).map(|image| format!("round-{round}/{image}"));
                    json!({"route": shown_text(&page["route"]), "image": image, "failure": shown_text(&page["failure"])})
                })
                .collect();
            json!({"name": name, "width": width, "height": height, "reference": reference, "captures": captures})
        })
        .collect();
    let record = record.unwrap_or(serde_json::Value::Null);
    let review = &record["review"];
    let issues: Vec<serde_json::Value> = review["issues"]
        .as_array()
        .map(|issues| {
            issues
                .iter()
                .map(|issue| {
                    let field = |name: &str| shown_text(&issue[name]);
                    let mut shown = json!({"id": field("id"), "section": field("section"), "viewport": field("viewport"), "severity": field("severity"), "status": field("status"), "expected": field("expected"), "actual": field("actual"), "fix_direction": field("fix_direction")});
                    // An issue that needs a file the package does not have names that file instead of a fix.
                    if issue["needs"]["path"].is_string() {
                        shown["needs_file"] = shown_text(&issue["needs"]["path"]);
                    }
                    shown
                })
                .collect()
        })
        .unwrap_or_default();
    let not_assessed: Vec<serde_json::Value> = review["checks"].as_array().map(|checks| checks.iter().filter(|check| check["status"] == "not_assessed").map(|check| json!({"id": shown_text(&check["id"]), "evidence": shown_texts(&check["evidence"])})).collect()).unwrap_or_default();
    Some(json!({
        "round": round,
        "verdict": shown_text(&record["verdict"]),
        "decided_by": shown_text(&record["decided_by"]),
        "reason": shown_text(&record["reason"]),
        "viewports": viewports,
        "issues": issues,
        "suggestions": shown_texts(&review["suggestions"]),
        "not_assessed": not_assessed,
        "blockers": shown_texts(&review["blockers"]),
    }))
}

/// A frontend run's design review as the run page shows it: the most reviews it allows (`of`) and each
/// round. Image paths are relative to the run's design folder, the only ones the design image route serves.
/// `None` for a run without a design review round.
fn design_shown(events: &[orq_orchestrator::Event], logs_dir: &Path) -> Option<serde_json::Value> {
    let last = design_last_round(events);
    if last == 0 {
        return None;
    }
    let dir = logs_dir.join("design");
    let references = design_references(design_record(&dir.join("manifest.json")).as_ref());
    let rounds: Vec<serde_json::Value> = (1..=last).filter_map(|round| design_round_shown(&dir, round, &references)).collect();
    if rounds.is_empty() {
        return None;
    }
    Some(serde_json::json!({"of": design_reviews_of(events), "rounds": rounds}))
}

/// The most bytes a design image may have to be served.
const DESIGN_IMAGE_CAP: u64 = 16 * 1024 * 1024;

/// Whether the run's own design records list the image at `path`, relative to its design folder: a package
/// reference or approved asset its manifest names, or a capture of a round its events name, as that round's
/// capture record names it. Only the one record the path points into is read.
fn design_image_listed(events: &[orq_orchestrator::Event], dir: &Path, path: &str) -> bool {
    if let Some(file) = path.strip_prefix("package/") {
        let manifest = design_record(&dir.join("manifest.json"));
        let files = manifest.as_ref().and_then(|manifest| manifest["files"].as_array());
        return files.is_some_and(|files| files.iter().any(|listed| (listed["kind"] == "reference" || listed["kind"] == "asset") && listed["path"] == file));
    }
    let Some((folder, image)) = path.split_once('/') else { return false };
    let Some(round) = folder.strip_prefix("round-").filter(|n| !n.starts_with('0')).and_then(|n| n.parse::<u64>().ok()) else { return false };
    if round == 0 || round > design_last_round(events) {
        return false;
    }
    let capture = design_record(&dir.join(folder).join("capture.json"));
    let pages = capture.as_ref().and_then(|capture| capture["pages"].as_array());
    pages.is_some_and(|pages| pages.iter().any(|page| page["image"] == image))
}

/// The content type of an image the page can show, by its extension.
fn design_image_type(path: &str) -> Option<&'static str> {
    match path.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for at in 0..4 {
            out.push(if at <= chunk.len() { ALPHABET[(n >> (18 - 6 * at) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The image at `path` in run `run`'s design folder as `{content_type, data}`: every folder on the way and
/// the file itself must be real (no link), the file an image under the cap. `None` otherwise. Each step is
/// opened from the folder already open, never following a link, and the file is checked once open, so a
/// link swapped in between a check and the read is refused all the same.
fn design_image(logs_root: &Path, run: &str, path: &str) -> Result<Option<serde_json::Value>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let Some(content_type) = design_image_type(path) else { return Ok(None) };
    let at = |dir: &std::fs::File, entry: &str, flags: libc::c_int| -> Option<std::fs::File> {
        let entry = std::ffi::CString::new(entry).ok()?;
        let fd = unsafe { libc::openat(dir.as_raw_fd(), entry.as_ptr(), flags | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        (fd >= 0).then(|| unsafe { std::fs::File::from_raw_fd(fd) })
    };
    let Ok(mut dir) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(logs_root) else { return Ok(None) };
    let mut steps: Vec<&str> = vec![run, "design"];
    steps.extend(path.split('/'));
    let Some((name, folders)) = steps.split_last() else { return Ok(None) };
    for folder in folders {
        let Some(next) = at(&dir, folder, libc::O_DIRECTORY) else { return Ok(None) };
        dir = next;
    }
    let Some(file) = at(&dir, name, libc::O_NONBLOCK) else { return Ok(None) };
    if !file.metadata().is_ok_and(|meta| meta.is_file() && meta.len() <= DESIGN_IMAGE_CAP) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(DESIGN_IMAGE_CAP).read_to_end(&mut bytes).map_err(|error| format!("design image {path} is unreadable: {error}"))?;
    Ok(Some(serde_json::json!({"content_type": content_type, "data": base64(&bytes)})))
}

/// The viewport a needed reference is for, as its request names it: `the mobile reference, 390x844 (...)`
/// at `designs/<feature>/references/mobile.png`.
fn needed_viewport(what: &str, path: &str) -> Option<serde_json::Value> {
    let file = path.rsplit_once("/references/")?.1;
    let name = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let size = what.split_once(", ")?.1.split_whitespace().next()?;
    let (width, height) = size.split_once('x')?;
    Some(serde_json::json!({"name": name, "width": width.parse::<u64>().ok()?, "height": height.parse::<u64>().ok()?}))
}

/// Why a needed design file is needed, by what it is.
fn needed_why(what: &str, path: &str) -> &'static str {
    if path.contains("/references/") {
        "The design review compares the page at this size with it."
    } else if what.starts_with("the asset ") {
        "DESIGN.md names it: the page uses it as an approved asset, unchanged."
    } else {
        "The implementer builds the page from it."
    }
}

/// What a run waiting for design files still needs, from its last request: each file's description, path in
/// `designs/<feature>/`, why it is needed and, for a reference, its viewport.
fn design_files_needed(events: &[orq_orchestrator::Event]) -> Option<serde_json::Value> {
    let payload = &events.iter().rev().find(|event| event.kind == "design.files_needed")?.payload;
    let files: Vec<serde_json::Value> = payload["files"]
        .as_array()?
        .iter()
        .filter_map(|file| {
            let (what, path) = (file["what"].as_str()?, file["path"].as_str()?);
            Some(serde_json::json!({"what": orq_core::redact::redact(what), "path": orq_core::redact::redact(path), "why": needed_why(what, path), "viewport": needed_viewport(what, path)}))
        })
        .collect();
    Some(serde_json::json!({"feature": shown_text(&payload["feature"]), "files": files}))
}

/// Records as failed every accept a server that stopped left without a result: nothing runs its tests anymore.
fn fail_interrupted_accepts(store: &orq_orchestrator::RunStore) {
    let Ok(runs) = store.run_ids() else { return };
    for run in runs {
        match orq_orchestrator::fail_interrupted_accept(store, &run) {
            Ok(true) => eprintln!("orq serve: {run} was being accepted when the server stopped; recorded as failed"),
            Ok(false) => {}
            Err(why) => eprintln!("orq serve: cannot check whether {run} was being accepted: {}", orq_core::redact::redact(&why)),
        }
    }
}

/// Whether run `run` in `store` is a merge-local run accepted before an accept merged it, still to be merged.
fn merge_after_accept_pending(store: &orq_orchestrator::RunStore, run: &str) -> bool {
    let (Ok(record), Ok(events)) = (store.load(run), store.events(run)) else { return false };
    orq_orchestrator::merge_after_accept_pending(&record, &events)
}

/// Sets `features` completed in the ledger of the project at `repo`, recorded at `at`, as a run that
/// delivered completes them; the names it set.
fn complete_in_ledger(repo: &Path, features: &[String], at: &str) -> Result<Vec<String>, String> {
    let project = orq_git::resolve_project(repo).map_err(|error| format!("{error:?}"))?;
    orq_features::complete_run_features(&orq_features::ledger_path(&project), features, at)
}

/// Takes a run out of the accepting set when dropped.
struct Released(std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>, String);

impl Drop for Released {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&self.1);
    }
}

type ListedRun = (Option<u64>, String, serde_json::Value, orq_orchestrator::RunRecord);

fn row_of(record: orq_orchestrator::RunRecord, log: &LogSummary) -> ListedRun {
    let sort_key = log.updated.as_deref().and_then(orq_orchestrator::parse_rfc3339);
    let mut row = serde_json::json!({"run_id": record.run_id, "feature": record.feature_id, "target_branch": record.target_branch, "state": log.state, "stage": log.stage, "started_at": log.started, "updated_at": log.updated, "delivery": record.delivery.as_str(), "ending": log.ending});
    add_resume_links(&mut row, log.resumed_as.as_ref(), log.resumed_from.as_ref());
    add_dirty_files(&mut row, log.dirty_files.as_deref());
    if let Some(reason) = &log.ending_reason {
        row["ending_reason"] = reason.as_str().into();
    }
    // Said only of a run the queue started unattended; any other run is attended.
    if log.unattended {
        row["unattended"] = true.into();
    }
    // Said only of a planning run (Draft with Claude); any other run is a feature run.
    if log.plan {
        row["kind"] = "plan".into();
    }
    (sort_key, record.run_id.clone(), row, record)
}

/// Whether a failed or review-waiting run's work is on its target, remembered for the commits its branch
/// and target were at when git was asked, so polling the list asks git again only after one of them moved.
#[derive(Default)]
struct MergedCache(std::collections::HashMap<String, (String, String, bool)>);

impl MergedCache {
    fn merged(&mut self, record: &orq_orchestrator::RunRecord, tips: &std::collections::HashMap<String, String>, check: impl FnOnce() -> bool) -> bool {
        let (Some(branch), Some(target)) = (tips.get(&record.branch), tips.get(&record.target_branch)) else {
            self.0.remove(&record.run_id);
            return false;
        };
        if let Some((known_branch, known_target, answer)) = self.0.get(&record.run_id) {
            if known_branch == branch && known_target == target {
                return *answer;
            }
        }
        let answer = check();
        self.0.insert(record.run_id.clone(), (branch.clone(), target.clone(), answer));
        answer
    }
}

/// The commit each local branch of `repo` is at, by branch name, read with one git call; empty when git cannot say.
fn branch_tips(repo: &Path) -> std::collections::HashMap<String, String> {
    let Ok(out) = std::process::Command::new("git").arg("-C").arg(repo).args(["for-each-ref", "--format=%(objectname) %(refname)", "refs/heads"]).output() else { return Default::default() };
    if !out.status.success() {
        return Default::default();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(sha, name)| Some((name.strip_prefix("refs/heads/")?.to_string(), sha.to_string())))
        .collect()
}

/// The state the popup shows: a failed or review-waiting run whose work is on its target branch in `repo`
/// is merged. It is asked of git each time, as a merge leaves the run's log as it was.
fn shown_state(repo: &Path, record: &orq_orchestrator::RunRecord, state: &'static str) -> &'static str {
    if matches!(state, "failed" | "needs_review") && orq_orchestrator::work_is_on_target(repo, record) {
        "merged"
    } else {
        state
    }
}

/// How many events one timeline answer holds; `next_cursor` continues after the last of them.
const TIMELINE_PAGE: usize = 200;
/// Lines in one page of a log, and the most of a log file that is read (and so shown).
const LOG_PAGE: usize = 500;
const LOG_READ_CAP: u64 = 8 * 1024 * 1024;

/// How many plain folders may be nested above a repository: `~/code/pitstop/eGiftit/egiftit-backend`
/// has two, and nothing deeper is looked at.
const GROUP_DEPTH: usize = 2;

/// Adds the repositories inside `dir`; a plain folder is looked into while `groups` allows it.
fn collect(dir: &Path, prefix: &str, groups: usize, found: &mut Vec<(String, PathBuf)>) {
    for name in child_folders(dir) {
        let path = dir.join(&name);
        let shown = format!("{prefix}{name}");
        if is_repository(&path) {
            found.push((shown, path));
        } else if groups > 0 {
            collect(&path, &format!("{shown}/"), groups - 1, found);
        }
    }
}

/// The names of the real directories directly inside `dir`: a link, a file or an unreadable
/// entry is left out, and so is a folder that no longer resolves to itself.
fn child_folders(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let path = dir.join(&name);
            (std::fs::canonicalize(&path).ok()? == path).then_some(name)
        })
        .collect()
}

/// A `.git` directory, or a `.git` file as linked worktrees have, that is not itself a symlink.
fn is_repository(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).map(|meta| !meta.file_type().is_symlink()).unwrap_or(false)
}

fn slug(name: &str) -> String {
    let cleaned: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let parts: Vec<&str> = cleaned.split('-').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() { "project".to_string() } else { parts.join("-") }
}

/// The id comes from the folder name alone, so it never changes when other folders come or go.
/// A name that is already a plain lowercase slug is its own id. Any other name gets its readable
/// slug plus `_` and the hex of the whole name; a plain slug never contains `_`, so the two
/// kinds cannot meet and two different names can never share an id.
fn project_id(name: &str) -> String {
    let readable = slug(name);
    if readable == name && name.len() <= MAX_ID_LEN {
        return readable;
    }
    let hex: String = name.bytes().map(|byte| format!("{byte:02x}")).collect();
    if readable.len() + 1 + hex.len() <= MAX_ID_LEN {
        return format!("{readable}_{hex}");
    }
    // A long name would overflow what the page accepts: keep a short readable start and a fixed
    // 128-bit digest instead. The `__` never occurs in the other two kinds, so they cannot meet.
    let start: String = readable.chars().take(40).collect();
    format!("{}__{:016x}{:016x}", start.trim_end_matches('-'), fnv1a(name, 0xcbf2_9ce4_8422_2325), fnv1a(name, 0x8422_2325_cbf2_9ce4))
}

/// The longest id the page accepts is 128 characters; stay well under it.
const MAX_ID_LEN: usize = 100;

/// FNV-1a: a stable hash that never changes between releases, unlike the standard hasher.
fn fnv1a(text: &str, seed: u64) -> u64 {
    text.bytes().fold(seed, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// Before serve opens orq.db: an upgrade that would leave active runs of an older Orq unable to use the database
/// waits for them unless `upgrade_now`, which applies it and records `store.upgraded_under_runs` on each of them.
/// The error lists those runs. A run launched before runs recorded their schema was started by an Orq from before
/// the compat marker, and blocks any upgrade.
pub fn check_upgrade(roots: &orq_core::storage::StorageRoots, upgrade_now: bool) -> Result<(), String> {
    use orq_core::store::Store;
    let database = roots.database();
    let Some((from, needed)) = Store::pending_upgrade(&database).map_err(|error| format!("cannot read orq.db: {error}"))? else { return Ok(()) };
    let runs = orq_orchestrator::RunStore::new(&roots.state);
    let active = runs.active_machine_runs().map_err(|error| format!("cannot read the runs: {error:?}"))?;
    let mut older = Vec::new();
    for (_, run_id) in active {
        let events = runs.events(&run_id).map_err(|error| format!("cannot read {run_id}: {error:?}"))?;
        let launched = events.iter().rev().find(|event| event.kind == "run.launched");
        // A run whose launch names no schema was started by an Orq from before the compat marker, which refuses
        // any newer schema: every upgrade leaves it unable to record its reviews.
        let schema = launched.and_then(|event| event.payload.get("schema")).and_then(serde_json::Value::as_u64);
        if schema.is_none_or(|schema| schema < u64::from(needed)) {
            older.push(run_id);
        }
    }
    if older.is_empty() {
        return Ok(());
    }
    let to = Store::LATEST_VERSION;
    if !upgrade_now {
        return Err(format!(
            "upgrading orq.db from schema {from} to {to} would leave these runs, started by an older Orq, unable to record their reviews: {}. \
             Wait for them to finish, or start with --upgrade-now",
            older.join(", ")
        ));
    }
    drop(Store::open(&database).map_err(|error| format!("cannot upgrade orq.db: {error}"))?);
    orq_orchestrator::record_upgraded_under_runs(&runs, &older, from, to).map_err(|error| format!("cannot record the upgrade on the runs: {error:?}"))
}

/// The server's repair rounds, stopped and joined when dropped.
pub struct RepairRounds {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Repairs missing reviews at once, then every `every`, as the review refresh looks at reviews again.
pub fn start_repair_rounds(projects: std::sync::Arc<StoreProjects>, every: std::time::Duration) -> RepairRounds {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || loop {
        projects.repair_missing_reviews();
        if !matches!(stopped.recv_timeout(every), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
            break;
        }
    });
    RepairRounds { stop: Some(stop), thread: Some(thread) }
}

impl Drop for RepairRounds {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The server's push rounds, stopped and joined when dropped.
pub struct PushRounds {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Tells the devices the server came back when `restarted`, then pushes what runs record every `every`, from
/// what they had recorded when the rounds began. Every push goes out on a thread of its own.
pub fn start_push_rounds(projects: std::sync::Arc<StoreProjects>, every: std::time::Duration, restarted: bool) -> PushRounds {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let mut watch = crate::web_push::RunWatch::new(&projects.roots);
    if restarted {
        let push = crate::web_push::Push { kind: "restarted", project_id: "server".into(), run_id: None, event_id: Some("restarted".into()), once: None };
        drop(crate::web_push::notify_later(&projects.roots, push));
    }
    let thread = std::thread::spawn(move || loop {
        drop(projects.push_round(&mut watch));
        if !matches!(stopped.recv_timeout(every), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
            break;
        }
    });
    PushRounds { stop: Some(stop), thread: Some(thread) }
}

impl Drop for PushRounds {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl StoreProjects {
    /// The id the web shows for the project a run's record names by its git project id; `None` when serve does not see it.
    pub(crate) fn web_project_of(&self, git_project_id: &str) -> Option<String> {
        discover(&self.scan_root).into_iter().find(|project| orq_git::resolve_project(&project.path).is_ok_and(|git| orq_git::project_id(&git) == git_project_id)).map(|project| project.id)
    }

    /// One look at what runs recorded since the last: each event worth a notification is pushed, under its
    /// project's id as the web shows it, off this thread. The handles give how many devices each reached.
    pub fn push_round(&self, watch: &mut crate::web_push::RunWatch) -> Vec<crate::web_push::Sending> {
        let mut web_ids: Option<std::collections::HashMap<String, String>> = None;
        let mut sends = Vec::new();
        for event in watch.new_events(&self.roots) {
            let Some(kind) = crate::web_push::kind_of(&event) else { continue };
            // The projects are scanned once a look, and only when there is something to send.
            let web_ids = web_ids.get_or_insert_with(|| {
                discover(&self.scan_root).into_iter().filter_map(|project| Some((orq_git::project_id(&orq_git::resolve_project(&project.path).ok()?), project.id))).collect()
            });
            let Some(project_id) = web_ids.get(&event.project_id) else { continue };
            let push = crate::web_push::Push { kind, project_id: project_id.clone(), run_id: Some(event.run_id.clone()), event_id: Some(event.event_id.clone()), once: None };
            sends.extend(crate::web_push::notify_later(&self.roots, push));
        }
        sends
    }

    /// The words of a push about `event`: one of `run`'s events (or the news its review gave), one of project
    /// `id`'s queue, or the server's restart. `None` for what this server does not show.
    fn push_text_of(&self, id: &str, run: Option<&str>, event: &str) -> Result<Option<crate::web_push::PushText>, String> {
        use crate::web_push::{push_text, PushPage};
        let Some(run) = run else {
            if id == "server" && event == "restarted" {
                return Ok(Some(push_text("restarted", false, "", None, PushPage::Dashboard)));
            }
            let kind = match event {
                "queue_finished" => "queue_finished",
                paused if paused == "queue_paused" || paused.starts_with("queue_paused:") => "queue_paused",
                _ => return Ok(None),
            };
            let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
            return Ok(Some(push_text(kind, false, &project.name, None, PushPage::Queue { project: &project.id })));
        };
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(None) };
        let feature = store.load(run).map_err(|error| format!("run {run} is unreadable: {error:?}"))?.feature_id;
        let review = PushPage::Review { project: &project.id, run };
        if event == "merged" {
            return Ok(Some(push_text("merged", false, &feature, None, review)));
        }
        if event.starts_with("stale:") {
            return Ok(Some(push_text("review", true, &feature, None, review)));
        }
        let events = store.events(run).map_err(|error| format!("run {run}'s events are unreadable: {error:?}"))?;
        let Some(found) = events.iter().find(|recorded| recorded.event_id == event) else { return Ok(None) };
        let Some(kind) = crate::web_push::kind_of(found) else { return Ok(None) };
        let detail = ["question", "reason"].iter().find_map(|key| found.payload.get(*key).and_then(serde_json::Value::as_str));
        let page = if kind == "review" { review } else { PushPage::Run { project: &project.id, run } };
        Ok(Some(push_text(kind, false, &feature, detail, page)))
    }
}

/// The projects provider the server hands to the handler: it rescans the scan root on each
/// request and reads counts from the same storage the CLI writes. A project it cannot read is
/// listed as unavailable, never dropped.
pub struct StoreProjects {
    roots: orq_core::storage::StorageRoots,
    scan_root: std::path::PathBuf,
    launcher: Option<std::sync::Arc<dyn RunLauncher + Send + Sync>>,
    /// Picks the level of a release; the project's implementer harness itself when unset.
    release_agent: Option<std::sync::Arc<dyn ReleaseAgent>>,
    agent_options: Option<AgentOptions>,
    storage: Option<std::sync::Arc<dyn StorageCleanup + Send + Sync>>,
    run_logs: std::sync::Mutex<LogCache>,
    merged: std::sync::Mutex<MergedCache>,
    log_snapshot: std::sync::Mutex<Option<LogSnapshot>>,
    accept_tests: AcceptTests,
    accepting: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    before_queue_slot_count: Option<BeforeQueueSlotCount>,
    backfilled: std::sync::Once,
    report_builder: Option<ReportBuilder>,
    server: Option<std::sync::Arc<dyn orq_core::remote_inbox::ServerControl>>,
    /// The program planning runs start as their agent; `claude` when unset.
    plan_agent: Option<PathBuf>,
    /// How long a planning session may run; [`orq_orchestrator::PLAN_TIMEOUT`] when unset.
    plan_timeout: Option<std::time::Duration>,
    /// The program the target push's GitHub branch lookup runs; `gh` when unset.
    gh: Option<PathBuf>,
    /// The program asked which models take images when a design role is saved; `pi` on PATH when unset.
    pi: Option<PathBuf>,
}

/// A report serve builds for a run whose own build of it failed: the run, its head, and the evidence its events hold.
#[derive(Debug, Clone)]
pub struct ReportRepair {
    pub run_id: String,
    /// The run's project among those serve sees; `None` when it is not one of them.
    pub repo: Option<std::path::PathBuf>,
    pub head_sha: String,
    pub evidence: orq_orchestrator::RecordedEvidence,
}

/// Builds and records the report a [`ReportRepair`] asks for; the path of the report, or why it could not be built.
pub type ReportBuilder = std::sync::Arc<dyn Fn(&ReportRepair) -> Result<std::path::PathBuf, String> + Send + Sync>;

/// The report a run ended without: the head it is missing for and why the run could not build it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingReport {
    pub head_sha: String,
    pub reason: String,
}

/// What the repairs say in a run's review-report log.
const REPAIRED: &str = "repaired by orq serve";

/// Adds `note` to `run_id`'s review-report log; a log that cannot be written is let go.
fn note_report_log(roots: &orq_core::storage::StorageRoots, run_id: &str, note: &str) {
    use std::io::Write;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(report_log(roots, run_id));
    let _ = log.and_then(|mut file| writeln!(file, "{note}"));
}

fn report_log(roots: &orq_core::storage::StorageRoots, run_id: &str) -> std::path::PathBuf {
    roots.state.join("logs").join(run_id).join("review-report.log")
}

/// The report `run_id` ended without: it ended review_ready or merged, the last line its review-report log has about a
/// report says it could not be built, and orq.db holds none for that head. A run id that could name a path elsewhere has none.
pub fn missing_report(roots: &orq_core::storage::StorageRoots, store: &orq_core::store::Store, run_id: &str) -> Option<MissingReport> {
    orq_core::storage::check_run_id(run_id).ok()?;
    let events = orq_orchestrator::RunStore::new(&roots.state).events(run_id).ok()?;
    let ending = events.iter().rev().find(|e| e.kind == "run.completed")?.payload.get("ending")?.as_str()?;
    if !matches!(ending, "review_ready" | "merged") || orq_orchestrator::summarize_run(&events).phase != orq_orchestrator::RunPhase::Completed {
        return None;
    }
    let log = std::fs::read_to_string(report_log(roots, run_id)).ok()?;
    let last = log.lines().rev().find(|line| line.starts_with("no report for ") || line.starts_with("report for "))?;
    let (head_sha, reason) = last.strip_prefix("no report for ")?.split_once(": ")?;
    let reported = store.reviews_for_run(run_id).ok()?;
    if reported.iter().any(|report| report.head_sha == head_sha) {
        return None;
    }
    Some(MissingReport { head_sha: head_sha.to_string(), reason: reason.to_string() })
}

/// Runs the tests of a run's worktree when it is accepted; the error is what failed.
pub type AcceptTests = std::sync::Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// Called by a queue round just before it counts the slots for each entry it may start; for tests that
/// end an accept partway through a round.
pub type BeforeQueueSlotCount = std::sync::Arc<dyn Fn(&StoreProjects) + Send + Sync>;

/// The redacted text of the log file last paged through, so the next page of an unchanged file is
/// cut from it instead of reading and redacting the file again.
struct LogSnapshot {
    path: std::path::PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    truncated: bool,
    text: std::sync::Arc<str>,
}

impl StoreProjects {
    /// A new server's provider. Accepts a stopped server left running are recorded as failed first, since this
    /// server runs none of their tests.
    pub fn new(roots: orq_core::storage::StorageRoots, scan_root: std::path::PathBuf) -> Self {
        fail_interrupted_accepts(&orq_orchestrator::RunStore::new(&roots.state));
        StoreProjects {
            roots,
            scan_root,
            launcher: None,
            release_agent: None,
            agent_options: None,
            storage: None,
            run_logs: Default::default(),
            merged: Default::default(),
            log_snapshot: Default::default(),
            accept_tests: std::sync::Arc::new(|worktree: &Path| orq_orchestrator::run_worktree_tests(worktree, None)),
            accepting: Default::default(),
            before_queue_slot_count: None,
            backfilled: std::sync::Once::new(),
            report_builder: None,
            server: None,
            plan_agent: None,
            plan_timeout: None,
            gh: None,
            pi: None,
        }
    }

    /// Has the target push ask GitHub about its branch through `gh` instead of the `gh` on PATH.
    pub fn with_gh(mut self, gh: PathBuf) -> Self {
        self.gh = Some(gh);
        self
    }

    /// Has a design role's save ask `pi` for its models instead of the `pi` on PATH.
    pub fn with_pi(mut self, pi: PathBuf) -> Self {
        self.pi = Some(pi);
        self
    }

    /// Gives the web the server's own controls: its builds and a restart when idle.
    pub fn with_server(mut self, server: std::sync::Arc<dyn orq_core::remote_inbox::ServerControl>) -> Self {
        self.server = Some(server);
        self
    }

    /// Lets the repairs build a missing report through `builder`; without one only PR/MRs are recorded.
    pub fn with_report_builder(mut self, builder: ReportBuilder) -> Self {
        self.report_builder = Some(builder);
        self
    }

    /// Builds `missing`, the report `run_id` ended without, from the evidence its events hold, and notes it in its log;
    /// a build that fails is noted there with why, and stays missing for the next round to try again.
    fn build_missing_report(&self, runs: &orq_orchestrator::RunStore, run_id: &str, missing: &MissingReport, repo: Option<std::path::PathBuf>) -> Result<(), String> {
        let builder = self.report_builder.as_ref().ok_or("this server cannot build review reports")?;
        let evidence = orq_orchestrator::recorded_evidence(runs, run_id, &missing.head_sha).map_err(|error| format!("cannot read the evidence of {run_id}: {error:?}"))?;
        let path = builder(&ReportRepair { run_id: run_id.to_string(), repo, head_sha: missing.head_sha.clone(), evidence }).inspect_err(|error| {
            // Why the repair failed becomes the reason the run's page shows; the same failure again is not said again.
            let reason = format!("{error} (orq serve could not repair it)");
            if missing.reason != reason {
                note_report_log(&self.roots, run_id, &format!("no report for {}: {reason}", missing.head_sha));
            }
        })?;
        note_report_log(&self.roots, run_id, &format!("report for {}: {} ({REPAIRED})", missing.head_sha, path.display()));
        Ok(())
    }

    /// The web's Build it now: builds the report `run` of `project` ended without, then records its PR/MR.
    fn build_report_now(&self, project: &Project, run: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        if self.report_builder.is_none() {
            return Ok(RunControlOutcome::Refused("this server cannot build review reports".to_string()));
        }
        let store = orq_core::store::Store::open(&self.roots.database()).map_err(|error| format!("cannot open the review index: {error}"))?;
        let Some(missing) = missing_report(&self.roots, &store, run) else {
            return Ok(RunControlOutcome::Refused("this run's review report is not missing".to_string()));
        };
        let runs = orq_orchestrator::RunStore::new(&self.roots.state);
        self.build_missing_report(&runs, run, &missing, Some(project.path.clone()))?;
        self.record_reviews_of(&runs, &store, Some(run))?;
        Ok(RunControlOutcome::Done(run.to_string()))
    }

    /// Repairs what runs could not record, as `orq review backfill` would and with the report each run would have
    /// built: a missing report is built from the evidence in the run's events, then each run merged here is resolved
    /// as merged and each created or adopted PR/MR without a review state is recorded. Each repair is noted in the run's review-report log; a repair that fails
    /// is only logged here, and the next round tries again.
    pub fn repair_missing_reviews(&self) {
        let database = self.roots.database();
        if !database.exists() {
            return;
        }
        let runs = orq_orchestrator::RunStore::new(&self.roots.state);
        let store = match orq_core::store::Store::open(&database) {
            Ok(store) => store,
            Err(error) => return eprintln!("orq: cannot repair missing reviews: cannot open the review index: {error}"),
        };
        if self.report_builder.is_some() {
            let mut repos: Option<std::collections::HashMap<String, std::path::PathBuf>> = None;
            for run_id in runs.run_ids().unwrap_or_default() {
                let Some(missing) = missing_report(&self.roots, &store, &run_id) else { continue };
                // Looked up once, and only when a report is missing.
                let repos = repos.get_or_insert_with(|| {
                    discover(&self.scan_root)
                        .into_iter()
                        .filter_map(|project| Some((orq_git::project_id(&orq_git::resolve_project(&project.path).ok()?), project.path)))
                        .collect()
                });
                let repo = runs.load(&run_id).ok().and_then(|record| repos.get(&record.project_id).cloned());
                if let Err(error) = self.build_missing_report(&runs, &run_id, &missing, repo) {
                    eprintln!("orq: cannot build the missing report of {run_id}: {error}");
                }
            }
        }
        if let Err(error) = self.record_reviews_of(&runs, &store, None) {
            eprintln!("orq: cannot repair missing reviews: {error}");
        }
    }

    /// Records the review state of each run (only `only` when given) whose report is now there, as `orq review
    /// backfill` does: a run merged here first, so it reads as merged and its PR/MR is left alone; then each other
    /// run's created or adopted PR/MR, noted in the run's log. A run whose record fails is said and the others go on.
    fn record_reviews_of(&self, runs: &orq_orchestrator::RunStore, store: &orq_core::store::Store, only: Option<&str>) -> Result<(), String> {
        let wanted = |run_id: &str| only.is_none_or(|only| only == run_id);
        let merges = orq_orchestrator::local_merges(runs).map_err(|error| format!("cannot read the runs' local merges: {error:?}"))?;
        for merge in merges.iter().filter(|(run_id, _)| wanted(run_id)) {
            if let Err(error) = orq_core::review_inbox::backfill_local_merges(store, std::slice::from_ref(merge)) {
                eprintln!("orq: cannot resolve the review of {}, merged here: {error}", merge.0);
            }
        }
        let created = orq_orchestrator::created_reviews(runs).map_err(|error| format!("cannot read the runs' review requests: {error:?}"))?;
        // A run merged here is its local merge's, even when recording that failed this round: its PR/MR kept now would
        // stand in the way of the next round's try.
        let merged_here = |run_id: &str| merges.iter().any(|(merged, _)| merged == run_id);
        for review in created.iter().filter(|review| wanted(&review.run_id) && !merged_here(&review.run_id)) {
            match orq_core::review_inbox::backfill_created_reviews(store, std::slice::from_ref(review)) {
                Ok(0) => {}
                Ok(_) => note_report_log(&self.roots, &review.run_id, &format!("review {} recorded ({REPAIRED})", review.url)),
                Err(error) => eprintln!("orq: cannot record the review request of {}: {error}", review.run_id),
            }
        }
        Ok(())
    }

    /// Resolves the reviews of runs merged locally before local merges resolved them. It runs once
    /// per server; a failure is only logged, since the next server start tries again.
    fn backfill_local_merges(&self) {
        self.backfilled.call_once(|| {
            if !self.roots.database().exists() {
                return;
            }
            let result = orq_orchestrator::local_merges(&orq_orchestrator::RunStore::new(&self.roots.state))
                .map_err(|error| format!("{error:?}"))
                .and_then(|merges| {
                    let store = orq_core::store::Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
                    orq_core::review_inbox::backfill_local_merges(&store, &merges)
                });
            if let Err(error) = result {
                eprintln!("orq: cannot backfill the reviews of locally merged runs: {error}");
            }
        });
    }

    /// Accepting a run runs `tests` in its worktree instead of what its stacks call for.
    pub fn with_accept_tests(mut self, tests: AcceptTests) -> Self {
        self.accept_tests = tests;
        self
    }

    /// Calls `hook` in each queue round just before it counts the slots for an entry it may start.
    pub fn with_before_queue_slot_count(mut self, hook: BeforeQueueSlotCount) -> Self {
        self.before_queue_slot_count = Some(hook);
        self
    }

    /// Whether this server is running the tests of an accept of `run`.
    pub fn is_being_accepted(&self, run: &str) -> bool {
        self.being_accepted(run).is_some()
    }

    /// Lets this provider start runs through `launcher`; without one it says it cannot.
    pub fn with_launcher(mut self, launcher: std::sync::Arc<dyn RunLauncher + Send + Sync>) -> Self {
        self.launcher = Some(launcher);
        self
    }

    /// Has the level of each release picked by `agent` instead of the implementer harness.
    pub fn with_release_agent(mut self, agent: std::sync::Arc<dyn ReleaseAgent>) -> Self {
        self.release_agent = Some(agent);
        self
    }

    /// Project `id`'s git project and the main checkout its own settings are kept under, so a linked
    /// worktree shares them; `None` when there is no such project.
    fn settings_key(&self, id: &str) -> Result<Option<(orq_git::Project, std::path::PathBuf)>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let root = orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone());
        Ok(Some((git, root)))
    }

    /// Project `id`'s git and its own settings in this machine's orq.db, keyed by its main checkout.
    /// orq.db is only read; without it the project has no record (revision 0). None for an unknown project.
    fn stored_settings_of(&self, id: &str) -> Result<Option<(orq_git::Project, orq_core::project_settings::StoredProjectSettings)>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        Ok(Some((git, self.stored_settings_at(&root)?)))
    }

    /// The settings saved in orq.db for the project whose main checkout is `root`; without orq.db, none.
    fn stored_settings_at(&self, root: &Path) -> Result<orq_core::project_settings::StoredProjectSettings, String> {
        let database = self.roots.database();
        if !database.exists() {
            return Ok(orq_core::project_settings::StoredProjectSettings { settings: Default::default(), revision: 0 });
        }
        orq_core::store::Store::read_project_settings(&database, root).map_err(|error| error.to_string())
    }

    /// Why project `git`'s settings refuse a run start now, redacted for the page; `None` when it may start.
    fn start_blocked_of(&self, git: &orq_git::Project) -> Result<Option<String>, String> {
        let settings = self.settings_of_git(git)?;
        let active = if settings.max_active_runs.is_some() {
            orq_orchestrator::RunStore::new(&self.roots.state)
                .active_project_runs(&orq_git::project_id(git))
                .map_err(|error| format!("{error:?}"))?
        } else {
            Vec::new()
        };
        let machine = self.machine_runs_now()?;
        Ok(start_blocked(git, &settings, &active, &machine)?.map(|why| orq_core::redact::redact(&why)))
    }

    /// This machine's `[runs]` limits and, under a run limit, its active runs; a malformed setting
    /// is kept to refuse every start with.
    pub(super) fn machine_runs_now(&self) -> Result<MachineRuns, String> {
        let limits = match orq_features::Settings::load(&self.roots.config_file()).map_err(|error| error.to_string()).and_then(|settings| settings.machine_run_limits()) {
            Ok(limits) => limits,
            Err(why) => return Ok(MachineRuns { malformed: Some(why), ..MachineRuns::default() }),
        };
        let active = match limits.max_active {
            Some(_) => orq_orchestrator::RunStore::new(&self.roots.state).active_machine_runs().map_err(|error| format!("{error:?}"))?,
            None => Vec::new(),
        };
        Ok(MachineRuns { limits, active, malformed: None })
    }

    /// The web's Resume or Review again of `run` of `project`, now: refused over the run limit, else
    /// through the launcher with what `request` adds.
    fn resume_now(&self, project: &Project, store: &orq_orchestrator::RunStore, run: &str, request: &ResumeRequest) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some(launcher) = &self.launcher else { return Ok(RunControlOutcome::Refused("this server cannot resume runs".to_string())) };
        if let Some(why) = self.resume_over_limit(project, store, run)? {
            return Ok(RunControlOutcome::Refused(why));
        }
        carried_on(launcher.resume_with(&project.path, run, request), &project.path)
    }

    /// Why resuming `run` of `project` would take the project over its run limit, redacted for the page: a
    /// blocked run holds no slot, so its resume is a start. `None` when it may resume.
    fn resume_over_limit(&self, project: &Project, store: &orq_orchestrator::RunStore, run: &str) -> Result<Option<String>, String> {
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let settings = self.settings_of_git(&git)?;
        let machine = self.machine_runs_now()?;
        if let Some(why) = &machine.malformed {
            return Ok(Some(orq_core::redact::redact(why)));
        }
        if machine.limits.min_free_disk_gb.is_some() {
            // A resume continues in the worktree its run recorded, which may be on another filesystem.
            let free = match store.load(run).ok().map(|record| record.worktree) {
                Some(worktree) => orq_orchestrator::free_disk(worktree.ancestors().find(|path| path.exists()).unwrap_or(&worktree)),
                None => orq_orchestrator::worktrees_free_disk(&git.root),
            }
            .map_err(|error| format!("cannot measure the free disk of {}: {error}", git.name))?;
            if let Some(why) = orq_orchestrator::machine_disk_refusal(machine.limits.min_free_disk_gb, &free) {
                return Ok(Some(orq_core::redact::redact(&why)));
            }
        }
        let takes_a_slot = (settings.max_active_runs.is_some() || machine.limits.max_active.is_some()) && orq_orchestrator::resume_takes_a_slot(store, run)?;
        if !takes_a_slot {
            return Ok(None);
        }
        if let Some(why) = orq_orchestrator::machine_limit_refusal(machine.limits.max_active, &machine.active) {
            return Ok(Some(orq_core::redact::redact(&why)));
        }
        let active = store.active_project_runs(&orq_git::project_id(&git)).map_err(|error| format!("{error:?}"))?;
        Ok(orq_orchestrator::limit_refusal(&git.name, settings.max_active_runs, &active).map(|why| orq_core::redact::redact(&why)))
    }

    /// The settings of project `git`, read from its main checkout's record.
    fn settings_of_git(&self, git: &orq_git::Project) -> Result<orq_core::project_settings::ProjectSettings, String> {
        let root = orq_git::main_checkout_root(git).unwrap_or_else(|| git.root.clone());
        Ok(self.stored_settings_at(&root)?.settings)
    }

    /// Project `git`'s `stored` settings as the web reads them: each role's own choice and what a
    /// run would use if it named none, with where that comes from, all redacted. It cannot fail: a
    /// fallback that cannot be worked out is null.
    fn settings_answer(&self, git: &orq_git::Project, stored: &orq_core::project_settings::StoredProjectSettings) -> serde_json::Value {
        use orq_features::AgentRole;
        // A fallback that cannot be worked out (a broken config a run may never read) is unknown, so
        // the project's own settings can still be seen and changed. Each role needs only its own
        // sources: the implementer never reads .tdd-config.json, and the reviewer's reviewers.default
        // comes before every profile.
        let profiles = self.model_profiles(&git.root).ok();
        let reviewers = orq_features::load_reviewer_config(&git.root.join(".tdd-config.json")).ok();
        let fallback_of = |role| {
            let (profiles, reviewers) = match role {
                AgentRole::Implementer => (profiles.clone()?, orq_features::TddReviewerConfig::default()),
                AgentRole::Reviewer => {
                    let reviewers = reviewers.clone()?;
                    match profiles.clone() {
                        Some(profiles) => (profiles, reviewers),
                        None if reviewers.default_reviewer.is_some() => (orq_features::ModelProfiles::default(), reviewers),
                        None => return None,
                    }
                }
            };
            orq_features::role_fallback(&profiles, &reviewers, role).ok()
        };
        let name = |text: &str| orq_core::redact::redact(text);
        let effort = |effort: Option<&String>| effort.map(|effort| name(effort));
        // A design role has no machine default, so it is the project's choice alone.
        let chosen_agent = |chosen: &Option<orq_core::project_settings::ProjectAgent>| chosen.as_ref().map(|agent| serde_json::json!({ "harness": name(&agent.harness), "model": name(&agent.model), "effort": effort(agent.effort.as_ref()) }));
        let role = |chosen: &Option<orq_core::project_settings::ProjectAgent>, role| {
            let fallback = fallback_of(role);
            let fallback = fallback.map(|(model, source)| serde_json::json!({ "harness": name(&model.harness), "model": name(&model.model), "effort": effort(model.thinking.as_ref()), "source": source.as_str() }));
            serde_json::json!({ "project": chosen_agent(chosen), "fallback": fallback })
        };
        serde_json::json!({
            "implementer": role(&stored.settings.implementer, AgentRole::Implementer),
            "reviewer": role(&stored.settings.reviewer, AgentRole::Reviewer),
            "design_preparer": chosen_agent(&stored.settings.design_preparer),
            "design_reviewer": chosen_agent(&stored.settings.design_reviewer),
            "design_verifier": chosen_agent(&stored.settings.design_verifier),
            "delivery": stored.settings.delivery.as_str(),
            "max_active_runs": stored.settings.max_active_runs,
            "min_free_disk_gb": stored.settings.min_free_disk_gb,
            "queue_hours": stored.settings.queue_hours,
            "queue_stop_on_failure": stored.settings.queue_stop_on_failure,
            "queue_wait_for_review": stored.settings.queue_wait_for_review,
            "pull_target_before_runs": stored.settings.pull_target_before_runs,
            "after_merge": stored.settings.after_merge.as_str(),
            "target_branch": queue::project_target(&git.root),
            "revision": stored.revision,
        })
    }

    /// Lets the web choose the implementer's and reviewer's harness and model from what `source`
    /// finds: each harness with the models it allows. It is asked once, on first use.
    pub fn with_agent_options(mut self, source: std::sync::Arc<AgentOptionsSource>) -> Self {
        self.agent_options = Some(AgentOptions { source, found: Default::default() });
        self
    }

    /// Lets the web clean up storage through `storage`, as `orq storage prune` does; without one it cannot.
    pub fn with_storage(mut self, storage: std::sync::Arc<dyn StorageCleanup + Send + Sync>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Why `run` cannot be acted on while this server is accepting it: its tests are running in its worktree,
    /// and the run must not be resumed, stopped, paused, switched or discarded under them.
    fn being_accepted(&self, run: &str) -> Option<String> {
        let accepting = self.accepting.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        accepting.contains(run).then(|| format!("{run}: an accept is running; try again when its tests have finished"))
    }

    /// The popup's Accept: the run is checked now, as the popup checks it, and its tests then run in the
    /// background, since they can take as long as the project's whole suite. A run already being accepted is
    /// refused; one that passes ends review ready, and one that fails stays as it was, its reason logged here.
    /// A merge-local run that passes is merged into its target, and its features are completed in the ledger
    /// of the project at `repo`; a merge-local run accepted before an accept merged it is merged now.
    fn accept(&self, repo: &Path, store: orq_orchestrator::RunStore, run: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let mut accepting = self.accepting.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if accepting.contains(run) {
            return Ok(RunControlOutcome::Refused(format!("{run} is already being accepted")));
        }
        let merge_only = merge_after_accept_pending(&store, run);
        let checked = if merge_only { orq_orchestrator::merge_accepted_check(&store, run) } else { orq_orchestrator::accept_check(&store, run) };
        // A failed git command is for this machine's operator; any other answer is why the run cannot be accepted.
        match checked {
            Ok(()) => {}
            Err(why) if why.starts_with("git ") => return Err(why),
            Err(why) => return Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
        }
        accepting.insert(run.to_string());
        drop(accepting);
        let (tests, in_flight, run_id, repo) = (self.accept_tests.clone(), self.accepting.clone(), run.to_string(), repo.to_path_buf());
        std::thread::spawn(move || {
            // Let go of the run however this thread ends, an unwind included, so it can be accepted again.
            let _release = Released(in_flight, run_id.clone());
            let complete = |_: &str, features: &[String], at: &str| complete_in_ledger(&repo, features, at);
            let accepted = if merge_only {
                orq_orchestrator::merge_accepted_run(&store, &run_id, "web", &*tests, &complete)
            } else {
                orq_orchestrator::accept_run_delivering(&store, &run_id, "web", &*tests, &complete)
            };
            // A failure is recorded on the run (run.accept_failed), so every device sees it; the log keeps a line too.
            if let Err(why) = accepted {
                eprintln!("orq serve: accepting {run_id} failed: {}", orq_core::redact::redact(&why));
            }
        });
        Ok(RunControlOutcome::Done(run.to_string()))
    }

    /// The model profiles of this machine's config and the `.orq.toml` of the project at `root`.
    /// Starts a run of one feature, or one group run of several, in project `id`, with the chosen profiles or
    /// the launch defaults. The CLI's eligibility rule comes first, and a group is put in its dependsOn order
    /// as the CLI's group rule says; a chosen profile must be one the launch picker lists.
    fn launch(&self, id: &str, target: Launch, profiles: Option<&orq_core::remote_inbox::LaunchProfiles>) -> Result<orq_core::remote_inbox::RunStart, String> {
        use orq_core::remote_inbox::RunStart;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(RunStart::Unknown) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let ledger = read_ledger(&orq_features::ledger_path(&git))?;
        // A group's members in the order to run them; a run of one feature has none.
        let archived = orq_features::archive::archived_completed_names(&self.roots.data, &orq_git::project_id(&git));
        let ordered: Vec<String> = match target {
            Launch::One(feature) | Launch::Queued(feature) => match orq_features::check_run_eligibility_with_archive(&ledger, feature, &archived) {
                Ok(_) => Vec::new(),
                Err(orq_features::Ineligible::NotFound) => return Ok(RunStart::Unknown),
                Err(why) => return Ok(RunStart::Refused(orq_core::redact::redact(&why.to_string()))),
            },
            Launch::Group(features) => match orq_features::order_run_group_with_archive(&ledger, features, &archived) {
                Ok(members) => members.into_iter().map(|member| member.feature_name.clone()).collect(),
                Err(why) => return Ok(RunStart::Refused(orq_core::redact::redact(&why.to_string()))),
            },
        };
        if let Some(chosen) = profiles {
            let listed = self.model_profiles(&git.root)?;
            for name in [&chosen.implementer, &chosen.reviewer] {
                if !listed.profiles().iter().any(|profile| &profile.name == name) {
                    return Ok(RunStart::Refused(orq_core::redact::redact(&format!("the profile {name} is not defined in the model profiles"))));
                }
            }
        }
        let Some(launcher) = &self.launcher else { return Ok(RunStart::Refused("this server cannot start runs".to_string())) };
        // Checked here so a refusal is the reason itself; the run checks again as it is recorded.
        if let Some(why) = self.start_blocked_of(&git)? {
            return Ok(RunStart::Refused(why));
        }
        // A person starting one feature by hand may start it before its dependency has landed, but is
        // told so, in the words the queue page uses.
        let waiting = match target {
            Launch::One(feature) => self.dependency_wait(&git, &ledger, feature)?,
            _ => None,
        };
        let started = match (target, profiles) {
            (Launch::Group(_), profiles) => launcher.start_group(&project.path, &ordered, profiles),
            (Launch::One(feature), Some(chosen)) => launcher.start_with(&project.path, feature, chosen),
            (Launch::One(feature), None) => launcher.start(&project.path, feature),
            (Launch::Queued(feature), profiles) => launcher.start_unattended(&project.path, feature, profiles),
        };
        match started {
            Ok(run) => {
                let warnings: Vec<String> = waiting.into_iter().chain(launcher.start_warning(&project.path)).collect();
                Ok(if warnings.is_empty() {
                    RunStart::Started(run)
                } else {
                    RunStart::StartedWithWarning { run, warning: orq_core::redact::redact(&warnings.join("; ")) }
                })
            }
            Err(LaunchError::Refused(why)) => Ok(RunStart::Refused(orq_core::redact::redact(&why))),
            Err(LaunchError::Failed(why)) => Err(why),
        }
    }

    fn model_profiles(&self, root: &Path) -> Result<orq_features::ModelProfiles, String> {
        orq_features::load_model_profiles(&self.roots.config_file(), &root.join(".orq.toml")).map_err(|error| error.to_string())
    }

    /// Project `id` and the run store, when `run` is a run of that project; only such a run can be
    /// acted on through it. `None` when either is unknown.
    /// Where the feature ledger of project `id` is; `None` when there is no such project.
    fn ledger_of(&self, id: &str) -> Result<Option<PathBuf>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        Ok(Some(orq_features::ledger_path(&git)))
    }

    fn own_run(&self, id: &str, run: &str) -> Result<Option<(Project, orq_orchestrator::RunStore)>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        match store.load(run) {
            Ok(record) if record.project_id == key => Ok(Some((project, store))),
            Ok(_) | Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => Ok(None),
            Err(error) => Err(format!("run {run} is unreadable: {error:?}")),
        }
    }
}

/// Finds the harnesses on this machine and the models each allows.
pub type AgentOptionsSource = dyn Fn() -> Vec<(String, Vec<String>)> + Send + Sync;

/// The harness options and what they were found to be, once asked.
struct AgentOptions {
    source: std::sync::Arc<AgentOptionsSource>,
    found: std::sync::OnceLock<serde_json::Value>,
}

/// What a launch starts: a run of one feature, or one group run of several.
#[derive(Clone, Copy)]
enum Launch<'a> {
    One(&'a str),
    /// A run of one feature the queue starts: nobody watches it, so it runs unattended.
    Queued(&'a str),
    Group(&'a [String]),
}

/// Starts a run the way the CLI does. The server never starts one by itself: whoever builds it supplies this.
/// What a resume carries beyond the run itself.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResumeRequest<'a> {
    /// The person's answer, put in the resumed session's prompt.
    pub answer: Option<&'a str>,
    /// Review the commits after the run's last review again and fix what that review finds,
    /// changing nothing else.
    pub review_again: bool,
}

pub trait RunLauncher {
    /// Starts a run of `feature` in the project at `repo` and returns its id.
    fn start(&self, repo: &Path, feature: &str) -> Result<String, LaunchError>;

    /// Starts a run as `start` does, with the chosen implementer and reviewer instead of the launch defaults.
    fn start_with(&self, _repo: &Path, _feature: &str, _profiles: &orq_core::remote_inbox::LaunchProfiles) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("this server cannot choose the profiles".to_string()))
    }

    /// Starts one group run of `features` in the project at `repo`, in that order, with the chosen implementer
    /// and reviewer or the launch defaults, and returns its id.
    fn start_group(&self, _repo: &Path, _features: &[String], _profiles: Option<&orq_core::remote_inbox::LaunchProfiles>) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("this server cannot start a group run".to_string()))
    }

    /// Starts a run of `feature` as the queue does: unattended, as nobody watches it, so its session
    /// never waits for an answer. A launcher that cannot start one so starts it as `start` or
    /// `start_with` does.
    fn start_unattended(&self, repo: &Path, feature: &str, profiles: Option<&orq_core::remote_inbox::LaunchProfiles>) -> Result<String, LaunchError> {
        match profiles {
            Some(chosen) => self.start_with(repo, feature, chosen),
            None => self.start(repo, feature),
        }
    }

    /// What whoever started a run in the project at `repo` should know about it, such as commits only the
    /// local target branch has, which the run will not see; `None` when there is nothing to say.
    fn start_warning(&self, _repo: &Path) -> Option<String> {
        None
    }

    /// Continues run `run` of the project at `repo` and returns the id of the run that carries on.
    fn resume(&self, _repo: &Path, _run: &str) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("this server cannot resume runs".to_string()))
    }

    /// Continues run `run` as `resume` does, with what `request` adds: an answer for the resumed
    /// session, or a review again of the commits after its last review.
    fn resume_with(&self, repo: &Path, run: &str, request: &ResumeRequest) -> Result<String, LaunchError> {
        if request.answer.is_none() && !request.review_again {
            return self.resume(repo, run);
        }
        Err(LaunchError::Refused("this server cannot resume a run with an answer or a review again".to_string()))
    }

    /// Merges run `run` of the project at `repo`, blocked by uncommitted changes in the checkout to files it
    /// changes, with those changes set aside meanwhile, and returns the id of the run that carries on.
    fn merge_set_aside(&self, _repo: &Path, _run: &str) -> Result<String, LaunchError> {
        Err(LaunchError::Refused("this server cannot merge runs".to_string()))
    }
}

/// Cleans up storage the way the CLI's prune does. The server never prunes by itself: whoever builds it supplies this.
pub trait StorageCleanup {
    /// What a prune would remove now: the lines a person reads, and the plan itself.
    fn preview(&self) -> Result<(Vec<String>, PrunePlan), String>;

    /// Removes what `plan` names and says, in lines a person reads, what was freed.
    fn prune(&self, plan: &PrunePlan) -> Result<Vec<String>, CleanupError>;
}

/// Why a cleanup did not run.
#[derive(Debug)]
pub enum CleanupError {
    /// It may not run now, such as while another prune runs; the reason is meant to be read by the person who asked.
    Refused(String),
    /// Anything else; the reason is for this machine's operator and is never sent to the web.
    Failed(String),
}

/// The hash of a cleanup set: the files (run, path, size) and the runs whose metadata would go, in a stable
/// order, so a prune can tell whether the set is still the one previewed.
fn cleanup_hash(plan: &PrunePlan) -> String {
    let mut files: Vec<String> = plan.candidates.iter().map(|c| format!("file\0{}\0{}\0{}\n", c.artifact.run_id, c.artifact.rel.display(), c.artifact.bytes)).collect();
    files.sort();
    let mut metadata: Vec<String> = plan.metadata.iter().map(|run| format!("metadata\0{run}\n")).collect();
    metadata.sort();
    files.extend(metadata);
    ledger_version(files.concat().as_bytes())
}

/// What the web is told of a launch that continues a run: the run that carries on, or the launch's own
/// refusal with this machine's paths left out; any other failure is for this machine's operator.
fn carried_on(launched: Result<String, LaunchError>, project: &Path) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
    use orq_core::remote_inbox::RunControlOutcome;
    match launched {
        Ok(run) => Ok(RunControlOutcome::Done(run)),
        Err(LaunchError::Refused(why)) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&crate::discard::paths_within_project(&why, project)))),
        Err(LaunchError::Failed(why)) => Err(why),
    }
}

/// Why a launch did not start a run.
#[derive(Debug)]
pub enum LaunchError {
    /// The run's own checks said no; the reason is meant to be read by the person who asked.
    Refused(String),
    /// Anything else; the reason is for this machine's operator and is never sent to the web.
    Failed(String),
}

/// Failures older than this are not counted as needing attention; the API states it with the count.
pub const FAILED_WINDOW_HOURS: u64 = 24;

#[derive(Clone, Default)]
struct RunCounts {
    /// The runs that have not ended, as (run id, feature id).
    active: Vec<(String, String)>,
    failed: usize,
}

struct Counts {
    git: orq_git::Project,
    features: usize,
    active: Vec<(String, String)>,
    failed_runs: usize,
    reviews_waiting: usize,
}

/// This machine's `[runs]` limits, its active runs (project id, run id) when it has a run limit,
/// and why its settings could not be read.
#[derive(Debug, Default)]
pub(super) struct MachineRuns {
    pub(super) limits: orq_features::MachineRunLimits,
    pub(super) active: Vec<(String, String)>,
    pub(super) malformed: Option<String>,
}

impl MachineRuns {
    /// Why the machine refuses a new run of project `git`, as `orq run` words it: its settings, its
    /// run limit, and its disk floor. `None` when it may start.
    fn refusal(&self, git: &orq_git::Project) -> Result<Option<String>, String> {
        if let Some(why) = &self.malformed {
            return Ok(Some(why.clone()));
        }
        if let Some(why) = orq_orchestrator::machine_limit_refusal(self.limits.max_active, &self.active) {
            return Ok(Some(why));
        }
        if self.limits.min_free_disk_gb.is_none() {
            return Ok(None);
        }
        let free = orq_orchestrator::worktrees_free_disk(&git.root).map_err(|error| format!("cannot measure the free disk of {}: {error}", git.name))?;
        Ok(orq_orchestrator::machine_disk_refusal(self.limits.min_free_disk_gb, &free))
    }
}

/// Why a run of project `git` may not start now under the `machine`'s limits and its own
/// `settings`, with `active` its runs that have not ended: a run limit or a disk floor, as `orq run`
/// words it. `None` when it may.
fn start_blocked(git: &orq_git::Project, settings: &orq_core::project_settings::ProjectSettings, active: &[(String, String)], machine: &MachineRuns) -> Result<Option<String>, String> {
    if let Some(why) = machine.refusal(git)? {
        return Ok(Some(why));
    }
    if let Some(why) = orq_orchestrator::limit_refusal(&git.name, settings.max_active_runs, active) {
        return Ok(Some(why));
    }
    if settings.min_free_disk_gb.is_none() {
        return Ok(None);
    }
    let free = orq_orchestrator::worktrees_free_disk(&git.root).map_err(|error| format!("cannot measure the free disk of {}: {error}", git.name))?;
    Ok(orq_orchestrator::disk_refusal(&git.name, settings.min_free_disk_gb, &free))
}

impl orq_core::remote_inbox::ProjectsProvider for StoreProjects {
    fn server(&self) -> Option<&dyn orq_core::remote_inbox::ServerControl> {
        self.server.as_deref()
    }

    fn start_plan(&self, id: &str, request: &orq_core::remote_inbox::PlanRequest) -> Result<orq_core::remote_inbox::PlanStart, String> {
        self.start_plan_run(id, request)
    }

    fn plan(&self, id: &str, run: &str) -> Result<Option<serde_json::Value>, String> {
        self.plan_of(id, run)
    }

    fn decide_plan(&self, id: &str, run: &str, replace: bool) -> Result<orq_core::remote_inbox::PlanDecision, String> {
        self.decide_plan_run(id, run, replace)
    }

    fn push_text(&self, id: &str, run: Option<&str>, event: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(self.push_text_of(id, run, event)?.map(|text| serde_json::json!({"title": text.title, "body": text.body, "path": text.path})))
    }

    fn review_scope(&self, id: &str) -> Result<Option<orq_core::remote_inbox::ReviewScope>, String> {
        // Only a project found by the scan counts; the id is matched, never used as a path.
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        Ok(Some(orq_core::remote_inbox::ReviewScope { review_key: orq_git::project_id(&git), name: project.name }))
    }

    fn review_acted(&self, _project: &str, run: &str, action: orq_core::review_inbox::ReviewAction, head: &str) -> Result<(), String> {
        let runs = orq_orchestrator::RunStore::new(&self.roots.state);
        orq_orchestrator::record_review_action(&runs, run, action.event_kind(), head, "web").map_err(|error| format!("cannot record {} on run {run}: {error:?}", action.as_str()))
    }

    fn settings(&self) -> Result<Option<serde_json::Value>, String> {
        orq_core::remote_inbox::public_settings(&self.roots.config_file()).map(Some)
    }

    fn setting_options(&self) -> Result<Option<serde_json::Value>, String> {
        let Some(options) = &self.agent_options else { return Ok(None) };
        if let Some(found) = options.found.get() {
            return Ok(Some(found.clone()));
        }
        let listed = (options.source)();
        // A harness with no models may only have failed to answer this time, so such a list is not kept.
        let complete = listed.iter().all(|(_, models)| !models.is_empty());
        // A harness whose levels are not known lists none, so a save leaves its effort alone.
        let option = |(harness, models): (String, Vec<String>)| {
            let levels = orq_agents::effort_levels(&harness);
            let mut option = serde_json::json!({"harness": harness, "models": models});
            if !levels.is_empty() {
                option["efforts"] = serde_json::json!(levels);
            }
            option
        };
        let found: serde_json::Value = listed.into_iter().map(option).collect();
        if complete {
            let _ = options.found.set(found.clone());
        }
        Ok(Some(found))
    }

    fn pi_models(&self) -> Result<Option<String>, String> {
        let program = self.pi.clone().unwrap_or_else(|| "pi".into());
        Ok(list_pi_models(&program, std::time::Duration::from_secs(5)))
    }

    fn start_run(&self, id: &str, feature: &str) -> Result<orq_core::remote_inbox::RunStart, String> {
        self.launch(id, Launch::One(feature), None)
    }

    fn queue(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        self.queue_answer(id)
    }

    fn change_queue(&self, id: &str, change: &orq_core::remote_inbox::QueueChange) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        self.change_queue_of(id, change)
    }

    fn pull_project_target(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        self.pull_target_of(id)
    }

    fn push_target_state(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        self.push_target_state_of(id)
    }

    fn push_target(&self, id: &str, sha: &str) -> Result<orq_core::remote_inbox::TargetPushResult, String> {
        self.push_target_of(id, sha)
    }

    fn queue_wish(&self, id: &str, feature: &str) -> Result<Option<bool>, String> {
        self.queue_wish_of(id, feature)
    }

    fn set_queue_wish(&self, id: &str, feature: &str, wanted: bool) -> Result<Option<Result<(), String>>, String> {
        self.set_queue_wish_of(id, feature, wanted)
    }

    fn set_feature_kind(&self, id: &str, feature: &str, kind: orq_core::store::FeatureKind) -> Result<Option<Result<(), String>>, String> {
        let Some((git, root)) = self.settings_key(id)? else { return Ok(None) };
        Ok(Some(set_feature_kind_at(&self.roots.database(), &git, &root, feature, kind.as_str())?.map(|_| ())))
    }

    fn start_run_with(&self, id: &str, feature: &str, profiles: &orq_core::remote_inbox::LaunchProfiles) -> Result<orq_core::remote_inbox::RunStart, String> {
        self.launch(id, Launch::One(feature), Some(profiles))
    }

    fn start_group_run(&self, id: &str, features: &[String], profiles: Option<&orq_core::remote_inbox::LaunchProfiles>) -> Result<orq_core::remote_inbox::RunStart, String> {
        self.launch(id, Launch::Group(features), profiles)
    }

    fn control_run(&self, id: &str, run: &str, control: orq_core::remote_inbox::RunControl) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::{RunControl, RunControlOutcome};
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        if let (false, Some(busy)) = (matches!(control, RunControl::Accept), self.being_accepted(run)) {
            return Ok(RunControlOutcome::Refused(busy));
        }
        match control {
            // The orchestrator words its own refusals "<run> is ..."; any other failure is for this machine's operator.
            RunControl::Pause => match orq_orchestrator::request_pause(&store, run) {
                Ok(()) => Ok(RunControlOutcome::Done(run.to_string())),
                Err(why) if why.starts_with(&format!("{run} is ")) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
                Err(why) => Err(why),
            },
            // The popup's Stop: the owner process is ended only when its recorded start time proves it is the run's.
            RunControl::Stop => match orq_orchestrator::stop_run(&store, run, &orq_orchestrator::owner_state, &orq_orchestrator::terminate_process, &std::thread::sleep) {
                Ok(()) => Ok(RunControlOutcome::Done(run.to_string())),
                Err(why) if why.starts_with(&format!("{run} is ")) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
                Err(why) => Err(why),
            },
            RunControl::Resume => self.resume_now(&project, &store, run, &ResumeRequest::default()),
            RunControl::ReviewAgain => {
                let events = store.events(run).map_err(|error| format!("run {run} is unreadable: {error:?}"))?;
                let phase = orq_orchestrator::summarize_run(&events).phase;
                if !matches!(phase, orq_orchestrator::RunPhase::NeedsReview | orq_orchestrator::RunPhase::Failed) {
                    let phase = phase.as_str().replace('_', " ");
                    return Ok(RunControlOutcome::Refused(format!("{run} is {phase}: only a run that needs review or failed can be reviewed again")));
                }
                self.resume_now(&project, &store, run, &ResumeRequest { answer: None, review_again: true })
            }
            RunControl::Accept => self.accept(&project.path, store, run),
            RunControl::BuildReport => self.build_report_now(&project, run),
            RunControl::MergeSetAside => {
                // Only a run whose last merge was refused over the checkout's changes may set them aside.
                let events = store.events(run).map_err(|error| format!("run {run} is unreadable: {error:?}"))?;
                if !orq_orchestrator::waiting_on_dirty_checkout(&events) {
                    return Ok(RunControlOutcome::Refused("this run is not waiting on uncommitted changes in the checkout".to_string()));
                }
                let Some(launcher) = &self.launcher else { return Ok(RunControlOutcome::Refused("this server cannot merge runs".to_string())) };
                carried_on(launcher.merge_set_aside(&project.path, run), &project.path)
            }
        }
    }

    fn resume_with_answer(&self, id: &str, run: &str, answer: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        if let Some(busy) = self.being_accepted(run) {
            return Ok(RunControlOutcome::Refused(busy));
        }
        self.resume_now(&project, &store, run, &ResumeRequest { answer: Some(answer), review_again: false })
    }

    fn resume_check(&self, id: &str, run: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        // The orchestrator's "cannot ..." is a failure on this machine; any other answer is why the run cannot resume.
        match orq_orchestrator::resume_check(&store, &project.path, run) {
            Ok(next) => Ok(RunControlOutcome::Done(orq_core::redact::redact(&next))),
            Err(why) if why.starts_with("cannot ") => Err(why),
            Err(why) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&crate::discard::paths_within_project(&why, &project.path)))),
        }
    }

    /// The popup's launch picker: the profiles of this machine's config and the project's `.orq.toml`, and the
    /// implementer and reviewer it preselects. A role whose default cannot be worked out has none.
    fn launch_profiles(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let root = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?.root;
        let user_config = self.roots.config_file();
        let profiles = self.model_profiles(&root)?;
        let reviewers = orq_features::load_reviewer_config(&root.join(".tdd-config.json")).unwrap_or_default();
        let settings = orq_features::Settings::load(&user_config).ok();
        let agent = |role| settings.as_ref().and_then(|settings| settings.agent(role).ok().flatten());
        let (machine_implementer, machine_reviewer) = orq_features::default_role_profiles(
            &profiles,
            &reviewers,
            agent(orq_features::AgentRole::Implementer).as_ref(),
            agent(orq_features::AgentRole::Reviewer).as_ref(),
        );
        // The project's own agent for a role is what a run naming no profile uses, so it comes first:
        // the profile that runs as it does, with its harness, model and effort as the thinking level,
        // or none, and then only "Project default" starts with it.
        let own = self.stored_settings_of(id)?.map(|(_, stored)| stored.settings).unwrap_or_default();
        let mut project_default = false;
        let mut pick = |own: Option<orq_core::project_settings::ProjectAgent>, machine: Option<String>| match own {
            None => machine,
            Some(agent) => {
                let found = profiles.profiles().iter().find(|p| p.harness == agent.harness && p.model == agent.model && p.thinking == agent.effort).map(|p| p.name.clone());
                project_default |= found.is_none();
                found
            }
        };
        let implementer = pick(own.implementer, machine_implementer);
        let reviewer = pick(own.reviewer, machine_reviewer);
        let names: Vec<&str> = profiles.profiles().iter().map(|profile| profile.name.as_str()).collect();
        Ok(Some(serde_json::json!({ "profiles": names, "implementer": implementer, "reviewer": reviewer, "project_default": project_default })))
    }

    /// The project's own settings in this machine's orq.db, keyed by its main checkout, and for each
    /// role what a run would use if the project named none, with where it comes from. orq.db is only
    /// read; without it the project has no record (revision 0).
    fn project_settings(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some((git, stored)) = self.stored_settings_of(id)? else { return Ok(None) };
        Ok(Some(self.settings_answer(&git, &stored)))
    }

    /// The design roles stored in this machine's orq.db for the project, as saved, so a save that leaves one
    /// out keeps it.
    fn design_agents(&self, id: &str) -> Result<Option<[Option<orq_core::project_settings::ProjectAgent>; 3]>, String> {
        let Some((_, stored)) = self.stored_settings_of(id)? else { return Ok(None) };
        let settings = stored.settings;
        Ok(Some([settings.design_preparer, settings.design_reviewer, settings.design_verifier]))
    }

    /// Saves the project's own settings in this machine's orq.db, keyed by its main checkout like a
    /// read, if its record is still at `revision`; then gives the saved settings as a read gives them.
    fn save_project_settings(&self, id: &str, revision: u64, settings: &orq_core::project_settings::ProjectSettings) -> Result<orq_core::remote_inbox::ProjectSettingsSave, String> {
        use orq_core::project_settings::SaveOutcome;
        use orq_core::remote_inbox::ProjectSettingsSave;
        let Some((git, root)) = self.settings_key(id)? else { return Ok(ProjectSettingsSave::Unknown) };
        let mut store = orq_core::store::Store::open(&self.roots.database()).map_err(|error| error.to_string())?;
        match store.save_project_settings(&root, revision, settings).map_err(|error| error.to_string())? {
            SaveOutcome::Conflict => Ok(ProjectSettingsSave::Conflict),
            // Once committed, the answer is built from what was saved and the project already found,
            // so nothing read afterwards can report a save that happened as refused.
            SaveOutcome::Saved { revision } => {
                let saved = orq_core::project_settings::StoredProjectSettings { settings: settings.clone(), revision };
                Ok(ProjectSettingsSave::Saved(self.settings_answer(&git, &saved)))
            }
        }
    }

    /// The popup's editor fields of one feature, and the version of the ledger they were read from.
    fn feature_fields(&self, id: &str, feature: &str) -> Result<Option<serde_json::Value>, String> {
        use orq_core::redact::redact;
        let Some(path) = self.ledger_of(id)? else { return Ok(None) };
        let Some(bytes) = read_ledger_bytes(&path)? else { return Ok(None) };
        let version = ledger_version(&bytes);
        let Some(entry) = parse_ledger_bytes(bytes)?.into_iter().find(|entry| entry.feature_name == feature) else { return Ok(None) };
        let fields = serde_json::json!({
            "name": redact(&entry.feature_name),
            "description": entry.description.as_deref().map(redact),
            "depends_on": entry.depends_on.iter().map(|name| redact(name)).collect::<Vec<_>>(),
            "parallelizable": entry.parallelizable,
            "test_file": entry.test_file.as_deref().map(redact),
            "target_files": entry.target_files.iter().map(|path| redact(path)).collect::<Vec<_>>(),
        });
        Ok(Some(serde_json::json!({ "fields": fields, "version": version })))
    }

    /// The popup's Promote: draft to pending under the ledger's lock. A refusal is worded as the CLI's
    /// `orq feature promote` words it; a ledger that cannot be read or written is this machine's failure.
    fn promote_feature(&self, id: &str, feature: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        use orq_features::{LedgerError, PromoteError};
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(RunControlOutcome::Unknown) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let refused = |why: String| Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why)));
        match orq_features::promote_feature(&orq_features::ledger_path(&git), feature) {
            Ok(_) => Ok(RunControlOutcome::Done(feature.to_string())),
            Err(PromoteError::UnknownFeature(_)) | Err(PromoteError::Ledger(LedgerError::Missing { .. })) => Ok(RunControlOutcome::Unknown),
            Err(PromoteError::UnrecognisedStatus(text)) => refused(format!("feature {feature} has unrecognised status {text:?} and is left alone")),
            Err(PromoteError::IllegalTransition(t)) => refused(format!("feature {feature}: {:?} to {:?} is not allowed", t.from, t.to)),
            Err(PromoteError::Conflict) => refused("ledger changed while it was being edited; nothing was written, try again".to_string()),
            Err(PromoteError::Locked) => refused("ledger is locked by another writer; nothing was written, try again".to_string()),
            Err(error) => Err(format!("{error:?}")),
        }
    }

    /// The popup's Add and Edit for the web's editor, saved against the ledger exactly as the editor read it:
    /// a version that no longer matches, or a file changed before the write, is a conflict and nothing is written.
    fn save_feature(&self, id: &str, feature: Option<&str>, fields: &orq_core::remote_inbox::FeatureInput, version: Option<&str>) -> Result<orq_core::remote_inbox::FeatureSave, String> {
        use orq_core::remote_inbox::FeatureSave;
        use orq_features::{AppendError, EditFieldsError, SaveError};
        let Some(path) = self.ledger_of(id)? else { return Ok(FeatureSave::Unknown) };
        let changed = || Ok(FeatureSave::Conflict("The ledger changed since you opened it; nothing was written".to_string()));
        let refused = |why: String| Ok(FeatureSave::Invalid(orq_core::redact::redact(&why)));
        let loaded = match (read_ledger_bytes(&path)?, version) {
            (None, _) if feature.is_some() => return Ok(FeatureSave::Unknown),
            (None, Some(_)) => return changed(),
            (None, None) => orq_features::LoadedLedger::absent(&path),
            (Some(bytes), Some(version)) if ledger_version(&bytes) != version => return changed(),
            (Some(bytes), _) => orq_features::LoadedLedger::from_bytes(&path, bytes).map_err(|error| format!("{error:?}"))?,
        };
        // A dependency on an archived completed feature is accepted and dropped, as the CLI's save does.
        let loaded = match discover(&self.scan_root).into_iter().find(|project| project.id == id).and_then(|project| orq_git::resolve_project(&project.path).ok()) {
            Some(git) => loaded.with_archive(orq_features::archive::archived_completed_names(&self.roots.data, &orq_git::project_id(&git))),
            None => loaded,
        };
        let save_error = |error: SaveError| match error {
            SaveError::Conflict => changed(),
            SaveError::Locked => Ok(FeatureSave::Conflict("the ledger is locked by another writer; nothing was written, try again".to_string())),
            SaveError::Io(error) => Err(format!("{error}")),
        };
        let Some(feature) = feature else {
            let name = fields.name.clone().unwrap_or_default();
            // The web adds no feature it could not then name: never a path, and a refusal says which character.
            if let Some(problem) = orq_features::feature_name_problem(&name) {
                return refused(problem);
            }
            let draft = orq_features::FeatureDraft {
                feature_name: name,
                description: fields.description.clone(),
                depends_on: fields.depends_on.clone().filter(|names| !names.is_empty()),
                parallelizable: fields.parallelizable,
                test_file: fields.test_file.clone(),
                target_files: fields.target_files.clone().filter(|paths| !paths.is_empty()),
            };
            return match loaded.append(&draft) {
                Ok(_) => Ok(FeatureSave::Saved),
                Err(AppendError::Invalid(why)) => refused(why.to_string()),
                Err(AppendError::Save(error)) => save_error(error),
            };
        };
        let Some(current) = loaded.entries().iter().find(|entry| entry.feature_name == feature) else { return Ok(FeatureSave::Unknown) };
        // As the popup's Edit: only what differs is sent, a field left out stays as it is, and one sent empty is cleared.
        // The editor was shown redacted values, so one sent back as shown keeps the original it stands for.
        let text = |new: &Option<String>, old: &Option<String>| new.as_ref().map(|value| unredacted(value, old.iter()));
        let list = |new: &Option<Vec<String>>, old: &[String]| new.as_ref().map(|values| values.iter().map(|value| unredacted(value, old.iter())).collect::<Vec<_>>());
        let differs = |new: Option<String>, old: &Option<String>| new.filter(|value| Some(value) != old.as_ref());
        let edits = orq_features::FieldEdits {
            description: differs(text(&fields.description, &current.description), &current.description),
            depends_on: list(&fields.depends_on, &current.depends_on).filter(|names| *names != current.depends_on),
            parallelizable: fields.parallelizable.filter(|value| Some(*value) != current.parallelizable),
            test_file: differs(text(&fields.test_file, &current.test_file), &current.test_file),
            target_files: list(&fields.target_files, &current.target_files).filter(|paths| *paths != current.target_files),
        };
        match loaded.edit_fields(feature, &edits) {
            Ok(_) => Ok(FeatureSave::Saved),
            Err(EditFieldsError::UnknownFeature) => Ok(FeatureSave::Unknown),
            Err(EditFieldsError::Invalid(why)) => refused(why.to_string()),
            Err(EditFieldsError::Save(error)) => save_error(error),
            Err(EditFieldsError::Malformed) => Err("the ledger could not be edited safely".to_string()),
        }
    }

    fn switch_model(&self, id: &str, run: &str, role: &str, profile: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some((project, store)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        if let Some(busy) = self.being_accepted(run) {
            return Ok(RunControlOutcome::Refused(busy));
        }
        let root = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?.root;
        // Only a profile the launch picker offers, as the popup's Switch model allows.
        if self.model_profiles(&root)?.get(profile).is_none() {
            return Ok(RunControlOutcome::Refused(orq_core::redact::redact(&format!("the profile {profile} is not defined in the model profiles"))));
        }
        match orq_orchestrator::request_model_switch(&store, run, role, profile) {
            Ok(()) => Ok(RunControlOutcome::Done(run.to_string())),
            Err(why) if why.starts_with(&format!("{run} is ")) => Ok(RunControlOutcome::Refused(orq_core::redact::redact(&why))),
            Err(why) => Err(why),
        }
    }

    fn discard_warning(&self, id: &str, run: &str) -> Result<orq_core::remote_inbox::RunControlOutcome, String> {
        use orq_core::remote_inbox::RunControlOutcome;
        let Some((project, _)) = self.own_run(id, run)? else { return Ok(RunControlOutcome::Unknown) };
        Ok(match crate::discard::discard_warning_or_refusal(&self.roots.state, run, &project.path) {
            Ok(warning) => RunControlOutcome::Done(orq_core::redact::redact(&warning)),
            Err(why) => RunControlOutcome::Refused(orq_core::redact::redact(&why)),
        })
    }

    fn storage_preview(&self) -> Result<Option<serde_json::Value>, String> {
        let Some(storage) = &self.storage else { return Ok(None) };
        let (lines, plan) = storage.preview()?;
        let runs: std::collections::BTreeSet<&str> = plan.candidates.iter().map(|c| c.artifact.run_id.as_str()).collect();
        let bytes = plan.total_bytes();
        // Snapshots are never part of a cleanup; they are listed so the card shows their size and age.
        let names: std::collections::BTreeMap<String, String> =
            discover(&self.scan_root).into_iter().filter_map(|p| Some((orq_git::project_id(&orq_git::resolve_project(&p.path).ok()?), p.name))).collect();
        let (snapshot_bytes, snapshot_lines) = orq_core::storage::snapshot_summary(&self.roots.data, &|id| names.get(id).cloned());
        Ok(Some(serde_json::json!({
            "snapshots": {
                "bytes": snapshot_bytes,
                "size": orq_core::retention::human_bytes(snapshot_bytes),
                "lines": snapshot_lines.iter().map(|line| orq_core::redact::redact(line)).collect::<Vec<_>>(),
            },
            "lines": lines.iter().map(|line| orq_core::redact::redact(line)).collect::<Vec<_>>(),
            "files": plan.candidates.len(),
            "bytes": bytes,
            "size": orq_core::retention::human_bytes(bytes),
            "runs": runs.len(),
            "metadata": plan.metadata.len(),
            "hash": cleanup_hash(&plan),
        })))
    }

    fn storage_prune(&self, hash: &str) -> Result<Option<orq_core::remote_inbox::StoragePrune>, String> {
        use orq_core::remote_inbox::StoragePrune;
        let Some(storage) = &self.storage else { return Ok(None) };
        // Planned again now, so only the set the person saw is removed.
        let (_, plan) = storage.preview()?;
        if cleanup_hash(&plan) != hash {
            return Ok(Some(StoragePrune::Changed));
        }
        match storage.prune(&plan) {
            Ok(lines) => Ok(Some(StoragePrune::Pruned(lines.iter().map(|line| orq_core::redact::redact(line)).collect()))),
            Err(CleanupError::Refused(why)) => Ok(Some(StoragePrune::Refused(orq_core::redact::redact(&why)))),
            Err(CleanupError::Failed(why)) => {
                eprintln!("orq serve: storage cleanup failed: {}", orq_core::redact::redact(&why));
                Err(why)
            }
        }
    }

    fn kept_worktrees(&self) -> Result<Option<serde_json::Value>, String> {
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        // Every project's listing reads the same run records, so they are read once.
        // A store that cannot be read shows no kept worktrees, as when each project's listing failed.
        let index = orq_orchestrator::RunIndex::load(&store)
            .inspect_err(|why| eprintln!("orq serve: the kept worktrees could not be read: {}", orq_core::redact::redact(&why.to_string())))
            .ok();
        let projects = if index.is_some() { discover(&self.scan_root) } else { Vec::new() };
        let (mut count, mut bytes) = (0usize, 0u64);
        let mut groups = Vec::new();
        for (project, index) in projects.iter().zip(index.iter().cycle()) {
            let kept = match index.list(&store, &project.path) {
                Ok(kept) => kept,
                Err(why) => {
                    eprintln!("orq serve: the kept worktrees of {} could not be read: {}", project.name, orq_core::redact::redact(&why.to_string()));
                    continue;
                }
            };
            if kept.is_empty() {
                continue;
            }
            count += kept.len();
            bytes += kept.iter().filter_map(|worktree| worktree.size).sum::<u64>();
            let worktrees: Vec<serde_json::Value> = kept.iter().map(kept_worktree_json).collect();
            groups.push(serde_json::json!({ "id": project.id, "name": project.name, "worktrees": worktrees }));
        }
        Ok(Some(serde_json::json!({
            "projects": groups,
            "count": count,
            "bytes": bytes,
            "size": orq_core::retention::human_bytes(bytes),
        })))
    }

    fn delete_kept_worktree(&self, id: &str, branch: &str, version: &str, force: bool) -> Result<orq_core::remote_inbox::WorktreeDelete, String> {
        self.delete_kept_worktrees(&[(id, branch, version)], force).pop().unwrap_or(Ok(orq_core::remote_inbox::WorktreeDelete::Unknown))
    }

    fn delete_kept_worktrees(&self, items: &[(&str, &str, &str)], force: bool) -> Vec<Result<orq_core::remote_inbox::WorktreeDelete, String>> {
        // The projects are found and the run records read once for the batch; each delete still reads the
        // records of runs started since, and the events of the runs it checks, just before it deletes.
        let projects = discover(&self.scan_root);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let mut index = match orq_orchestrator::RunIndex::load(&store) {
            Ok(index) => index,
            Err(why) => {
                let why = why.to_string();
                eprintln!("orq serve: a kept worktree could not be deleted: {}", orq_core::redact::redact(&why));
                return items.iter().map(|_| Err(why.clone())).collect();
            }
        };
        items
            .iter()
            .map(|(id, branch, version)| {
                let project = projects.iter().find(|project| project.id == *id);
                project.map_or(Ok(orq_core::remote_inbox::WorktreeDelete::Unknown), |project| delete_with(&mut index, &store, project, branch, version, force))
            })
            .collect()
    }

    fn clean_up_run(&self, id: &str, run: &str) -> Result<orq_core::remote_inbox::RunCleanup, String> {
        self.clean_up(id, run)
    }

    fn discard_run(&self, id: &str, run: &str) -> Result<orq_core::remote_inbox::RunDiscard, String> {
        use orq_core::remote_inbox::RunDiscard;
        let Some((project, _)) = self.own_run(id, run)? else { return Ok(RunDiscard::Unknown) };
        if let Some(busy) = self.being_accepted(run) {
            return Ok(RunDiscard::Refused(busy));
        }
        // The orchestrator words why a run cannot go as "<run> ...", "... refusing ..." or "... is not the
        // repository of <run>"; any other failure is for this machine's operator.
        match crate::discard::discard_and_reopen(&self.roots.state, run, &project.path) {
            Ok(done) => Ok(RunDiscard::Discarded { note: orq_core::redact::redact(&done.web_note), cleared: done.merged }),
            Err(why) if why.starts_with(&format!("{run} ")) || why.contains("refusing") || why.ends_with(&format!("is not the repository of {run}")) => {
                Ok(RunDiscard::Refused(orq_core::redact::redact(&why)))
            }
            Err(why) => Err(why),
        }
    }

    fn machine_runs(&self) -> Result<Option<serde_json::Value>, String> {
        let machine = self.machine_runs_now()?;
        let limits = machine.limits;
        if limits.max_active.is_none() && limits.min_free_disk_gb.is_none() {
            return Ok(None);
        }
        let measured = self.scan_root.ancestors().find(|path| path.exists()).unwrap_or(&self.scan_root);
        // Shown to one decimal, rounded down, as a refusal words it.
        let free_disk_gb = orq_orchestrator::free_disk(measured).ok().map(|free| (free.bytes / 100_000_000) as f64 / 10.0);
        // Without a run limit the active runs were not counted for a refusal; the line still shows them.
        let active = match limits.max_active {
            Some(_) => machine.active.len(),
            None => orq_orchestrator::RunStore::new(&self.roots.state).active_machine_runs().map_err(|error| format!("{error:?}"))?.len(),
        };
        Ok(Some(serde_json::json!({"active_runs": active, "max_active_runs": limits.max_active, "min_free_disk_gb": limits.min_free_disk_gb, "free_disk_gb": free_disk_gb})))
    }

    fn start_blocked(&self, id: &str) -> Result<Option<String>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        self.start_blocked_of(&git)
    }

    fn features(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let entries = read_ledger(&orq_features::ledger_path(&git))?;
        let root = orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone());
        let frontend = if entries.is_empty() { Default::default() } else { frontend_features_at(&self.roots.database(), &root)? };
        let list = entries
            .into_iter()
            .map(|entry| {
                let kind = if frontend.contains(&entry.feature_name) { "frontend" } else { "standard" };
                serde_json::json!({"name": orq_core::redact::redact(&entry.feature_name), "description": entry.description.as_deref().map(orq_core::redact::redact), "status": orq_core::redact::redact(entry.status.as_str()), "depends_on": entry.depends_on.iter().map(|name| orq_core::redact::redact(name)).collect::<Vec<_>>(), "test_file": entry.test_file.as_deref().map(orq_core::redact::redact), "recorded_at": entry.recorded_at.as_deref().map(orq_core::redact::redact), "kind": kind})
            })
            .collect();
        Ok(Some(serde_json::Value::Array(list)))
    }

    /// The archived features, shaped like the features list plus when each was archived. The records
    /// that only note which dependencies were dropped are not features and are left out.
    fn archived_features(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let entries = orq_features::archive::read_archive(&orq_features::archive::archive_path(&self.roots.data, &orq_git::project_id(&git))).map_err(|error| error.to_string())?;
        let text = |entry: &serde_json::Value, key: &str| entry.get(key).and_then(|v| v.as_str()).map(orq_core::redact::redact);
        let list: Vec<serde_json::Value> = entries
            .iter()
            .filter(|entry| entry.get("status").is_some())
            .map(|entry| {
                let depends_on: Vec<String> = entry.get("dependsOn").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|n| n.as_str().map(orq_core::redact::redact)).collect()).unwrap_or_default();
                serde_json::json!({"name": text(entry, "featureName"), "description": text(entry, "description"), "status": text(entry, "status"), "depends_on": depends_on, "test_file": text(entry, "testFile"), "recorded_at": text(entry, "recordedAt"), "archived_at": text(entry, "archivedAt")})
            })
            .collect();
        Ok(Some(serde_json::json!({ "archived": list })))
    }

    fn archive_features(&self, id: &str, dry_run: bool) -> Result<orq_core::remote_inbox::ArchiveOutcome, String> {
        use orq_core::remote_inbox::ArchiveOutcome;
        use orq_features::archive::{archive_ledger, ArchiveError};
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(ArchiveOutcome::Unknown) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let ledger = orq_features::ledger_path(&git);
        if !ledger.exists() {
            return Ok(ArchiveOutcome::Unknown);
        }
        let project_id = orq_git::project_id(&git);
        let active = orq_orchestrator::RunStore::new(&self.roots.state).active_project_runs(&project_id).map_err(|error| format!("{error:?}"))?.len();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        let stamp = orq_core::storage::readable_utc(now);
        match archive_ledger(&ledger, &self.roots.data, &project_id, 100, dry_run, active, &stamp) {
            Ok(plan) => {
                let stripped: Vec<serde_json::Value> = plan
                    .stripped
                    .iter()
                    .map(|(feature, names)| serde_json::json!({"feature": orq_core::redact::redact(feature), "names": names.iter().map(|n| orq_core::redact::redact(n)).collect::<Vec<_>>()}))
                    .collect();
                Ok(ArchiveOutcome::Done(serde_json::json!({"archived": plan.archived.len(), "dry_run": dry_run, "stripped": stripped})))
            }
            Err(ArchiveError::ActiveRun) => Ok(ArchiveOutcome::Refused("a run of this project is active; wait for it to finish before archiving".to_string())),
            Err(ArchiveError::Ledger(why)) => Ok(ArchiveOutcome::Refused(orq_core::redact::redact(&format!("the ledger cannot be archived: {why}")))),
            Err(error) => Err(error.to_string()),
        }
    }

    fn runs(&self, id: &str) -> Result<Option<serde_json::Value>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
        let key = orq_git::project_id(&git);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let mut rows = Vec::new();
        // A poisoned lock only means an earlier request panicked; what it holds is still valid to re-check.
        let mut cache = self.run_logs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let run_ids = store.run_ids().map_err(|error| format!("{error:?}"))?;
        // Runs that are gone are forgotten, so the cache never outgrows the store.
        let live: std::collections::HashSet<&String> = run_ids.iter().collect();
        cache.retain(|id, _| live.contains(id));
        for run_id in run_ids {
            let record = match store.load(&run_id) {
                Ok(record) => record,
                Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
                Err(error) => return Err(format!("run {run_id} is unreadable: {error:?}")),
            };
            if let Some(row) = run_row(&store, &self.roots.state, &mut cache, record, &key)? {
                rows.push(row);
            }
        }
        drop(cache);
        let frontend = if rows.is_empty() { Default::default() } else { frontend_features_at(&self.roots.database(), &orq_git::main_checkout_root(&git).unwrap_or_else(|| git.root.clone()))? };
        for row in &mut rows {
            let is_frontend = row.2["feature"].as_str().is_some_and(|feature| frontend.contains(feature));
            row.2["feature_kind"] = if is_frontend { "frontend" } else { "standard" }.into();
        }
        // A merge leaves a run's log as it was, so a failed or review-waiting run is checked against git,
        // which is asked again only for a run whose branch or target has moved since it last was.
        if rows.iter().any(|row| matches!(row.2["state"].as_str(), Some("failed" | "needs_review"))) {
            let tips = branch_tips(&project.path);
            let mut merged = self.merged.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for row in &mut rows {
                if matches!(row.2["state"].as_str(), Some("failed" | "needs_review")) && merged.merged(&row.3, &tips, || orq_orchestrator::work_is_on_target(&project.path, &row.3)) {
                    row.2["state"] = "merged".into();
                }
            }
        }
        // Newest first; a run with no readable time goes last, ties by run id.
        rows.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => y.cmp(&x).then_with(|| a.1.cmp(&b.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.cmp(&b.1),
        });
        Ok(Some(serde_json::Value::Array(rows.into_iter().map(|row| row.2).collect())))
    }

    fn run_logs(&self, id: &str, run: &str, file: Option<&str>, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        use std::io::Read;
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let record = match store.load(run) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        if record.project_id != key {
            return Ok(None);
        }
        let logs_root = self.roots.state.join("logs");
        let dir = logs_root.join(run);
        // A link in place of either directory would lead outside the run's own logs.
        let real_dir = |path: &std::path::Path| match path.symlink_metadata() {
            Ok(meta) => Some(meta.is_dir()),
            Err(_) => None,
        };
        if real_dir(&logs_root) == Some(false) || real_dir(&dir) == Some(false) {
            return Ok(None);
        }
        // Only plain, non-hidden `.log` files that are not links are logs.
        let plain = plain_log_name;
        let Some(file) = file else {
            let mut files = Vec::new();
            // A run that has written no logs has no directory; any other failure is an error.
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry.map_err(|error| format!("logs of {run} are unreadable: {error}"))?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let meta = entry.path().symlink_metadata().map_err(|error| format!("log {name} is unreadable: {error}"))?;
                        if meta.is_file() && plain(&name) {
                            files.push(serde_json::json!({"name": name, "bytes": meta.len()}));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("logs of {run} are unreadable: {error}")),
            }
            files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            return Ok(Some(serde_json::json!({"files": files})));
        };
        if !plain(file) {
            return Ok(None);
        }
        let skip: usize = match after {
            None => 0,
            Some(cursor) => match cursor.parse() {
                Ok(lines) => lines,
                Err(_) => return Ok(None),
            },
        };
        let path = dir.join(file);
        match path.symlink_metadata() {
            Ok(meta) if meta.is_file() => {}
            _ => return Ok(None),
        }
        let meta = path.symlink_metadata().map_err(|error| format!("log {file} is unreadable: {error}"))?;
        let (len, modified) = (meta.len(), meta.modified().ok());
        let mut slot = self.log_snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = match slot.as_ref().filter(|known| after.is_some() && known.path == path && known.len == len && known.modified == modified && modified.is_some()) {
            Some(known) => (known.text.clone(), known.truncated),
            None => {
                let mut bytes = Vec::new();
                std::fs::File::open(&path).and_then(|f| f.take(LOG_READ_CAP + 1).read_to_end(&mut bytes)).map_err(|error| format!("log {file} is unreadable: {error}"))?;
                let truncated = bytes.len() as u64 > LOG_READ_CAP;
                bytes.truncate(LOG_READ_CAP as usize);
                // The whole readable text is redacted at once so a secret that spans lines is found.
                let text: std::sync::Arc<str> = String::from_utf8_lossy(&orq_core::redact::redact_bytes(&bytes)).into_owned().into();
                *slot = Some(LogSnapshot { path: path.clone(), len, modified, truncated, text: text.clone() });
                (text, truncated)
            }
        };
        drop(slot);
        let (text, truncated) = snapshot;
        let total = text.lines().count();
        if skip > total {
            return Ok(None);
        }
        let page: Vec<&str> = text.lines().skip(skip).take(LOG_PAGE).collect();
        let next = if skip + page.len() < total { Some((skip + page.len()).to_string()) } else { None };
        Ok(Some(serde_json::json!({"name": file, "lines": page, "next_cursor": next, "truncated": truncated})))
    }

    fn run_design_image(&self, id: &str, run: &str, path: &str) -> Result<Option<serde_json::Value>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let record = match store.load(run) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        if record.project_id != key {
            return Ok(None);
        }
        let events = match store.events(run) {
            Ok(events) => events,
            Err(StoreError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        let logs_dir = self.roots.state.join("logs").join(run);
        if !plain_design_path(path) || !design_image_listed(&events, &logs_dir.join("design"), path) {
            return Ok(None);
        }
        design_image(&self.roots.state.join("logs"), run, path)
    }

    fn run_timeline(&self, id: &str, run: &str, after: Option<&str>) -> Result<Option<serde_json::Value>, String> {
        use orq_orchestrator::StoreError;
        let Some(project) = discover(&self.scan_root).into_iter().find(|project| project.id == id) else { return Ok(None) };
        let key = orq_git::project_id(&orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?);
        let store = orq_orchestrator::RunStore::new(&self.roots.state);
        let record = match store.load(run) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        if record.project_id != key {
            return Ok(None);
        }
        let events = match store.events(run) {
            Ok(events) => events,
            Err(StoreError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(format!("run {run} is unreadable: {error:?}")),
        };
        let start = match after {
            None => 0,
            Some(cursor) => match events.iter().position(|event| event.event_id == cursor) {
                Some(at) => at + 1,
                None => return Ok(None),
            },
        };
        let summary = orq_orchestrator::summarize_run(&events);
        let page: Vec<_> = events.iter().skip(start).take(TIMELINE_PAGE).collect();
        let more = start + page.len() < events.len();
        let next_cursor = if more { page.last().map(|event| event.event_id.clone()) } else { None };
        // The payload is left out: it can hold reasons and paths the page has no need of.
        // A review round starts with each review stage start, counted over the whole run so a later page
        // still names its round; only review events carry one.
        let advance = |round: u64, event: &orq_orchestrator::Event| if event.kind == "stage.started" || round == 0 { round + 1 } else { round };
        let is_review = |event: &orq_orchestrator::Event| event.stage.as_deref() == Some("review");
        let mut round = events[..start].iter().filter(|event| is_review(event)).fold(0, advance);
        let logs_dir = self.roots.state.join("logs").join(run);
        let listed: Vec<_> = page
            .iter()
            .enumerate()
            .map(|(n, event)| {
                let mut listed = serde_json::json!({"event_id": event.event_id, "kind": event.kind, "stage": event.stage, "attempt": event.attempt, "occurred_at": event.occurred_at});
                if is_review(event) {
                    round = advance(round, event);
                    listed["review_round"] = serde_json::json!(round);
                }
                if let Some(round) = orq_orchestrator::design_round_label(event) {
                    listed["design_round"] = serde_json::json!(round);
                }
                if event.kind == "mcp.setup" {
                    listed["mcp"] = mcp_setup_shown(&event.payload);
                }
                if event.kind == "merge.set_aside" {
                    listed["set_aside"] = set_aside_shown(&record.run_id, &event.payload);
                }
                if let Some(flaky) = flaky_shown(event) {
                    listed["flaky"] = flaky;
                }
                if event.kind == "gate.flaky_tests" {
                    listed["flaky_tests"] = flaky_tests_shown(&event.payload);
                }
                if event.kind == "session.undeclared_files" {
                    listed["undeclared_files"] = undeclared_files_shown(&event.payload);
                }
                if event.kind == "session.format_reverted" {
                    listed["format_reverted"] = undeclared_files_shown(&event.payload);
                }
                if event.kind == "target.pull_warning" {
                    listed["target_pull"] = target_pull_shown(&event.payload);
                }
                if event.kind == "target.pushed" {
                    listed["pushed"] = serde_json::json!({"remote": event.payload["remote"], "target": event.payload["target"]});
                }
                if event.kind.ends_with(".failed") {
                    if let Some(failure) = failure_shown(&events, start + n, &logs_dir) {
                        listed["failure"] = failure;
                    }
                }
                listed
            })
            .collect();
        // How many review rounds the whole run has, so live events can carry on counting.
        let review_rounds = events.iter().filter(|event| is_review(event)).fold(0, advance);
        let mut shown = serde_json::json!({"run_id": record.run_id, "feature": record.feature_id, "target_branch": record.target_branch, "state": shown_state(&project.path, &record, listed_state(&summary)), "stage": summary.stage, "started_at": events.first().map(|event| &event.occurred_at), "updated_at": events.last().map(|event| &event.occurred_at), "delivery": record.delivery.as_str(), "ending": delivered_ending(&events, summary.phase)});
        add_resume_links(&mut shown, handed_over_to(&summary).as_ref(), resumed_from(&events).as_ref());
        if let Some(answer) = resume_answer(&events, &logs_dir) {
            shown["answer"] = answer.into();
        }
        add_dirty_files(&mut shown, dirty_files(&events, summary.phase).as_deref());
        if let Some(reason) = ending_reason(&events, &summary) {
            let findings = ending_findings(&reason, &record.worktree);
            if !findings.is_empty() {
                shown["ending_findings"] = serde_json::json!(findings);
            }
            shown["ending_reason"] = reason.into();
        }
        add_accept(&mut shown, &events, &summary, &logs_dir);
        // A merge-local run accepted before an accept merged it, whose work its target lacks (the page's Merge).
        if orq_orchestrator::merge_after_accept_pending(&record, &events) && !orq_orchestrator::work_is_on_target(&project.path, &record) {
            shown["merge_pending"] = serde_json::json!(true);
        }
        add_missing_report(&mut shown, &self.roots, run);
        // Read once, for the run's first page: a followed run asks for its later pages every second.
        if after.is_none() {
            if let Some(design) = design_shown(&events, &logs_dir) {
                shown["design"] = design;
            }
        }
        if summary.phase == orq_orchestrator::RunPhase::DesignFilesNeeded {
            if let Some(needed) = design_files_needed(&events) {
                shown["design_files_needed"] = needed;
            }
        }
        let planning = plan::is_plan(&events);
        if planning {
            shown["kind"] = serde_json::json!("plan");
            shown["plan"] = self.plan_of(id, run)?.unwrap_or(serde_json::Value::Null);
        }
        // A failed run with commits its last review never saw may be reviewed again (the page's Review again).
        let reviewable_again = !planning
            && summary.phase == orq_orchestrator::RunPhase::Failed
            && orq_orchestrator::last_review(&record.worktree, &record.base_sha).ok().flatten().is_some_and(|review| !review.unreviewed_commits.is_empty());
        if reviewable_again {
            shown["reviewable_again"] = serde_json::json!(true);
        }
        if cleanup::merged_review(&self.roots, run).is_some() {
            shown["cleanup"] = cleanup::card(&events);
        }
        // A run merged locally may have left its target ahead of origin: the page's Push.
        let merged_locally = record.delivery == orq_core::project_settings::Delivery::MergeLocal && events.iter().any(|event| event.kind == "merge.completed");
        if merged_locally {
            let root = orq_git::resolve_project(&project.path).ok().and_then(|git| orq_git::main_checkout_root(&git)).unwrap_or_else(|| project.path.clone());
            if let Some(unpushed) = cleanup::unpushed(&root) {
                shown["unpushed"] = unpushed;
            }
        }
        Ok(Some(serde_json::json!({
            "run": shown,
            "events": listed,
            "review_rounds": review_rounds,
            "next_cursor": next_cursor,
        })))
    }

    fn projects(&self) -> Result<serde_json::Value, String> {
        use serde_json::json;
        self.backfill_local_merges();
        let reviews = if self.roots.database().exists() {
            Some(orq_core::store::Store::open_read_only(&self.roots.database()).map_err(|error| format!("cannot open the review index: {error}"))?)
        } else {
            None
        };
        let runs = orq_orchestrator::RunStore::new(&self.roots.state);
        let active = active_runs_by_project(&runs);
        let machine = self.machine_runs_now()?;
        let waiting = cleanup::waiting_by_project(reviews.as_ref(), &runs);
        let list = discover(&self.scan_root)
            .into_iter()
            .map(|project| match counts(&project, reviews.as_ref(), &active).and_then(|counts| {
                let settings = self.settings_of_git(&counts.git)?;
                let blocked = start_blocked(&counts.git, &settings, &counts.active, &machine)?.map(|why| orq_core::redact::redact(&why));
                let queue = queue::queue_summary(&self.roots, reviews.as_ref(), &runs, &counts.git, settings.queue_wait_for_review)?;
                let cleanup = waiting.get(&orq_git::project_id(&counts.git)).cloned().unwrap_or_default();
                let unpushed = cleanup::unpushed(&orq_git::main_checkout_root(&counts.git).unwrap_or_else(|| counts.git.root.clone()));
                Ok((counts, settings.max_active_runs, blocked, queue, cleanup, unpushed))
            }) {
                Ok((counts, max_active_runs, start_blocked, queue, cleanup, unpushed)) => json!({"id": project.id, "name": project.name, "available": true, "features": counts.features, "active_runs": counts.active.len(), "max_active_runs": max_active_runs, "start_blocked": start_blocked, "queue": queue, "reviews_waiting": counts.reviews_waiting, "failed_runs": counts.failed_runs, "failed_window_hours": FAILED_WINDOW_HOURS, "cleanup": cleanup, "unpushed": unpushed}),
                Err(_) => json!({"id": project.id, "name": project.name, "available": false, "features": null, "active_runs": null, "reviews_waiting": null, "failed_runs": null, "failed_window_hours": FAILED_WINDOW_HOURS}),
            })
            .collect::<Vec<_>>();
        Ok(serde_json::Value::Array(list))
    }
}

/// What the run page shows of a `merge.set_aside` event: the stash commit and the message it is listed
/// under, whether the changes were put back, the files set aside and those that conflicted, and the note to
/// the person, all redacted. Anything else the payload holds is left out.
/// What a re-run after a failure reported unrelated shows: the report, redacted, whether the re-run
/// passed, and why it never started when it could not. Any other event shows none.
fn flaky_shown(event: &orq_orchestrator::Event) -> Option<serde_json::Value> {
    let passed = match event.kind.as_str() {
        "gate.flaky" | "ci.flaky" => true,
        "gate.rerun_failed" | "ci.rerun_failed" => false,
        _ => return None,
    };
    let text = |key: &str| event.payload.get(key).and_then(|value| value.as_str()).map(orq_core::redact::redact);
    let mut shown = serde_json::json!({"summary": text("summary"), "passed": passed});
    // A retry the provider refused never ran: say why, not that it failed again.
    if !passed && event.payload.get("started").and_then(|value| value.as_bool()) == Some(false) {
        shown["not_started"] = text("reason").unwrap_or_else(|| "no reason given".to_string()).into();
    }
    Some(shown)
}

fn set_aside_shown(run: &str, payload: &serde_json::Value) -> serde_json::Value {
    let text = |key: &str| payload.get(key).and_then(|value| value.as_str()).map(orq_core::redact::redact);
    let mut shown = serde_json::json!({
        "stash": text("stash"),
        "message": format!("orq set-aside {run}"),
        "restored": payload.get("restored").and_then(|value| value.as_bool()).unwrap_or(false),
        "files": texts(&payload["files"]),
        "conflicted": texts(&payload["conflicted"]),
    });
    if let Some(note) = text("note") {
        shown["note"] = note.into();
    }
    shown
}

/// What the run page shows of a `gate.flaky_tests` event: each test that passed only on a retry, by name
/// (redacted), and the retry it passed on. Anything else the payload holds is left out.
fn flaky_tests_shown(payload: &serde_json::Value) -> serde_json::Value {
    let tests: Vec<_> = payload
        .get("tests")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|test| {
            let name = orq_core::redact::redact(test.get("name")?.as_str()?);
            Some(serde_json::json!({"name": name, "retry": test.get("retry").and_then(|v| v.as_u64())}))
        })
        .collect();
    serde_json::Value::Array(tests)
}

/// What the run page shows of a `session.undeclared_files` event: each file a session changed outside
/// its features' files, redacted; of a `session.format_reverted` event, each file whose formatting was
/// undone. Anything else the payload holds is left out.
fn undeclared_files_shown(payload: &serde_json::Value) -> serde_json::Value {
    let files: Vec<_> = payload.get("files").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|file| Some(orq_core::redact::redact(file.as_str()?))).collect();
    serde_json::json!(files)
}

/// What the run page shows of a `target.pull_warning` event: why the start could not pull its target,
/// its message redacted. Anything else the payload holds is left out.
fn target_pull_shown(payload: &serde_json::Value) -> serde_json::Value {
    let text = |key: &str| payload.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    serde_json::json!({ "outcome": text("outcome"), "message": orq_core::redact::redact(&text("message")) })
}

/// How many lines a failure's excerpt holds at most.
const EXCERPT_LINES: usize = 15;
/// How far before the end of its part a failure with no failure line points.
const TAIL_LINES: usize = 40;

/// Where an attempt's error is in its log: the line, numbered from 1, and the lines from it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSpot {
    pub line: usize,
    /// Where the line starts in the file, in bytes.
    pub offset: u64,
    pub excerpt: Vec<String>,
}

/// Which part of an appended log one attempt wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogPart {
    /// The lines after line `start` up to line `end`, or to the end of the file when `None`.
    Lines { start: usize, end: Option<usize> },
    /// The lines after line `line` up to line `end`, read from byte `offset`, where line `line` ends, on.
    After { line: usize, offset: u64, end: Option<usize> },
    /// The `n`th attempt (from 1), each starting with the log's first line again; when the log does not
    /// have that many, the whole file and its last failure line.
    Attempt(usize),
}

/// How surely a line says something failed: a test or build tool's own failure line, or only a command
/// that exited with a failure status, which a test's own output can also print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Strong,
    Weak,
}

/// Whether `line` is a failure line; `traceback` says a Python traceback came before it.
fn failure_marker(line: &str, traceback: bool) -> Option<Marker> {
    let trimmed = line.trim_start();
    let trimmed_end = line.trim_end();
    let first_word = line.split([':', ' ', '\t']).next().unwrap_or("");
    let summary_failed = line.match_indices("= ").any(|(at, _)| {
        let rest = &line[at + 2..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        digits > 0 && rest[digits..].starts_with(" failed")
    });
    let strong = line.contains("panicked at")
        || line.contains("test result: FAILED")
        || (trimmed.starts_with("test ") && trimmed_end.ends_with(" ... FAILED"))
        || (trimmed.starts_with("---- ") && trimmed_end.ends_with(" stdout ----"))
        || (trimmed.starts_with("error[E") && line.contains("]:"))
        || trimmed.starts_with("error: could not compile")
        || trimmed.starts_with("error: test failed")
        || line.starts_with("FAILED ")
        || (traceback && !line.starts_with([' ', '\t']) && first_word.contains("Error"))
        || summary_failed
        || line.contains(" FAIL ")
        || trimmed.starts_with('✕')
        || trimmed.starts_with('×')
        || line.contains("AssertionError");
    if strong {
        Some(Marker::Strong)
    } else if line.contains("exited with status") {
        Some(Marker::Weak)
    } else {
        None
    }
}

/// A failure line and the excerpt that runs on from it.
#[derive(Default)]
struct Capture {
    spot: Option<LogSpot>,
}

impl Capture {
    /// Take `line` as the failure line when `marker` and `replace` (or nothing is held yet); otherwise
    /// add it to the held excerpt while that has room.
    fn offer(&mut self, (number, offset): (usize, u64), line: &str, marker: bool, replace: bool) {
        match &mut self.spot {
            Some(_) if marker && replace => self.spot = Some(LogSpot { line: number, offset, excerpt: vec![line.to_string()] }),
            Some(spot) if spot.excerpt.len() < EXCERPT_LINES => spot.excerpt.push(line.to_string()),
            Some(_) => {}
            None if marker => self.spot = Some(LogSpot { line: number, offset, excerpt: vec![line.to_string()] }),
            None => {}
        }
    }

    fn full(&self) -> bool {
        self.spot.as_ref().is_some_and(|spot| spot.excerpt.len() == EXCERPT_LINES)
    }
}

/// One part of a log read line by line: its first (or, with `last`, last) strong and weak failure lines,
/// and its last lines for a part with neither.
#[derive(Default)]
struct PartScan {
    last: bool,
    traceback: bool,
    strong: Capture,
    weak: Capture,
    tail: std::collections::VecDeque<((usize, u64), String)>,
}

impl PartScan {
    fn add(&mut self, at: (usize, u64), line: &str) {
        let marker = failure_marker(line, self.traceback);
        if line.starts_with("Traceback") {
            self.traceback = true;
        }
        self.strong.offer(at, line, marker == Some(Marker::Strong), self.last);
        self.weak.offer(at, line, marker == Some(Marker::Weak), self.last);
        if self.tail.len() == TAIL_LINES + 1 {
            self.tail.pop_front();
        }
        self.tail.push_back((at, line.to_string()));
    }

    /// Whether reading on can change nothing: the first strong failure line and its excerpt are read.
    fn settled(&self) -> bool {
        !self.last && self.strong.full()
    }

    fn spot(self) -> Option<LogSpot> {
        if let Some(spot) = self.strong.spot.or(self.weak.spot) {
            return Some(spot);
        }
        let ((line, offset), _) = *self.tail.front()?;
        let excerpt = self.tail.into_iter().take(EXCERPT_LINES).map(|(_, text)| text).collect();
        Some(LogSpot { line, offset, excerpt })
    }
}

/// The first failure line of `part` of the log at `path`, read once, line by line, stopping at the part's end.
pub fn find_failure_line(path: &std::path::Path, part: LogPart) -> std::io::Result<Option<LogSpot>> {
    find_failure_line_in(std::fs::File::open(path)?, part)
}

/// The first failure line of `part` of the log read from `log`, from its start or, for `After`, its offset.
fn find_failure_line_in(mut log: impl std::io::Read + std::io::Seek, part: LogPart) -> std::io::Result<Option<LogSpot>> {
    use std::io::BufRead;
    // An attempt read from its offset numbers its lines on from the line count recorded there.
    let (mut number, mut offset, part) = match part {
        LogPart::After { line, offset, end } => (line, offset, LogPart::Lines { start: line, end }),
        part => (0, 0, part),
    };
    log.seek(std::io::SeekFrom::Start(offset))?;
    let mut reader = std::io::BufReader::new(log);
    let mut buffer = Vec::new();
    let mut first_line: Option<String> = None;
    let mut attempts = 0;
    let mut found = PartScan::default();
    // The whole file, for an attempt the log does not have.
    let mut whole = PartScan { last: true, ..PartScan::default() };
    loop {
        buffer.clear();
        let read = reader.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            break;
        }
        number += 1;
        let at = (number, offset);
        offset += read as u64;
        let line = String::from_utf8_lossy(&buffer).trim_end_matches(['\n', '\r']).to_string();
        match part {
            LogPart::Lines { start, end } => {
                if end.is_some_and(|end| number > end) {
                    break;
                }
                if number > start {
                    found.add(at, &line);
                }
            }
            LogPart::After { .. } => unreachable!("read as Lines from its offset"),
            LogPart::Attempt(wanted) => {
                if first_line.is_none() {
                    first_line = Some(line.clone());
                }
                if first_line.as_deref() == Some(line.as_str()) {
                    attempts += 1;
                }
                if attempts > wanted {
                    break;
                }
                if attempts == wanted {
                    found.add(at, &line);
                }
                whole.add(at, &line);
            }
        }
        if found.settled() {
            break;
        }
    }
    if matches!(part, LogPart::Attempt(wanted) if attempts < wanted) {
        return Ok(whole.spot());
    }
    Ok(found.spot())
}

/// The largest log a failure's error is looked for in.
const FAILURE_SEARCH_CAP: u64 = 20 * 1024 * 1024;

/// Whether `name` is one a run's logs page lists: a plain, non-hidden `.log` name.
fn plain_log_name(name: &str) -> bool {
    name.ends_with(".log") && !name.starts_with('.') && !name.contains(['/', '\\'])
}

/// The name of the log at `path` when it is a plain `.log` file, not a link, directly inside `logs_dir`,
/// itself no link either; `None` for anything else.
fn run_log_name(path: &std::path::Path, logs_dir: &std::path::Path) -> Option<String> {
    let is_dir = |path: &std::path::Path| path.symlink_metadata().is_ok_and(|meta| meta.is_dir());
    if path.parent() != Some(logs_dir) || !logs_dir.parent().is_some_and(is_dir) || !is_dir(logs_dir) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    (plain_log_name(name) && path.symlink_metadata().is_ok_and(|meta| meta.is_file())).then(|| name.to_string())
}

/// The run's own log at `path`, checked by `run_log_name` and then opened through handles that follow no
/// link: the logs directory, the run's directory in it and the file itself. A path swapped for a link or
/// a pipe after it was checked is not opened, or not read. `between` runs between the check and the open.
fn open_run_log_with(path: &std::path::Path, logs_dir: &std::path::Path, between: impl FnOnce()) -> Option<(String, std::fs::File)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let name = run_log_name(path, logs_dir)?;
    between();
    let open_dir = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(logs_dir.parent()?).ok()?;
    let at = |dir: &std::fs::File, entry: &std::ffi::OsStr, flags: libc::c_int| -> Option<std::fs::File> {
        let entry = std::ffi::CString::new(entry.as_bytes()).ok()?;
        let fd = unsafe { libc::openat(dir.as_raw_fd(), entry.as_ptr(), flags | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        (fd >= 0).then(|| unsafe { std::fs::File::from_raw_fd(fd) })
    };
    let run_dir = at(&open_dir, logs_dir.file_name()?, libc::O_DIRECTORY)?;
    let file = at(&run_dir, std::ffi::OsStr::new(&name), libc::O_NONBLOCK)?;
    file.metadata().is_ok_and(|meta| meta.is_file()).then_some((name, file))
}

/// What the run page shows of the failure event at `at`: its reason (also without a log) and, when it names one of the run's own
/// logs, the line in it where this attempt's error is, all redacted. The attempt's part of an appended log
/// runs from the line count its stage start recorded to the next start's.
fn failure_shown(events: &[orq_orchestrator::Event], at: usize, logs_dir: &std::path::Path) -> Option<serde_json::Value> {
    use orq_core::redact::redact;
    let event = &events[at];
    let reason = event.payload.get("reason").and_then(|v| v.as_str());
    let log = event.payload.get("log").and_then(|v| v.as_str());
    // A failure before any log was written still says why.
    let mut failure = serde_json::json!({"reason": redact(reason.unwrap_or(""))});
    if event.payload.get("failed_tests").is_some() {
        add_failed_tests(&mut failure, &event.payload);
    }
    let Some(log) = log else { return reason.map(|_| failure) };
    let path = std::path::Path::new(log);
    let Some((file, opened)) = open_run_log_with(path, logs_dir, || {}) else { return Some(failure) };
    let starts_of_log = |event: &&orq_orchestrator::Event| event.kind == "stage.started" && event.payload.get("log").and_then(|v| v.as_str()) == Some(log);
    let log_line = |event: &orq_orchestrator::Event| event.payload.get("log_line").and_then(|v| v.as_u64()).map(|n| n as usize);
    let log_offset = |event: &orq_orchestrator::Event| event.payload.get("log_offset").and_then(|v| v.as_u64());
    let started = events[..at].iter().rev().find(starts_of_log);
    let end = || events[at..].iter().find(starts_of_log).and_then(log_line);
    // A start that recorded where the log ended is read from there; one that recorded only its line count
    // is found by counting lines; a run from before either is split at the log's repeated first line.
    let part = match (started.and_then(log_line), started.and_then(log_offset)) {
        (Some(line), Some(offset)) => LogPart::After { line, offset, end: end() },
        (Some(start), None) => LogPart::Lines { start, end: end() },
        (None, _) => LogPart::Attempt(events[..at].iter().filter(starts_of_log).count().max(1)),
    };
    let size = opened.metadata().map_or(u64::MAX, |meta| meta.len());
    failure["log"] = log_shown(file, opened, size, part);
    Some(failure)
}

/// What the run page shows of the log `file`, `size` bytes long and read from `log`: its name and, when
/// it is small enough to search and reads to the end of `part`, the error's line and the redacted lines
/// from it. A log that fails to read part way is named only, since what was read may not hold the error.
fn log_shown(file: String, log: impl std::io::Read + std::io::Seek, size: u64, part: LogPart) -> serde_json::Value {
    let mut shown = serde_json::json!({"file": file});
    let small = size <= FAILURE_SEARCH_CAP;
    if let Some(spot) = small.then(|| find_failure_line_in(log, part).ok().flatten()).flatten() {
        // The logs page reads only a log's first part, so a line past it could not be opened.
        if spot.offset < LOG_READ_CAP {
            shown["line"] = serde_json::json!(spot.line);
        }
        shown["excerpt"] = serde_json::json!(orq_core::redact::redact(&spot.excerpt.join("\n")));
    }
    shown
}

/// What the run page shows of an `mcp.setup` event: each server's name, version and source, and the
/// notes, all as text. Anything else the payload holds is left out.
fn mcp_setup_shown(payload: &serde_json::Value) -> serde_json::Value {
    let text = |value: Option<&serde_json::Value>| value.and_then(|v| v.as_str()).map(str::to_string);
    let servers: Vec<_> = payload
        .get("servers")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|server| {
            let name = text(server.get("name"))?;
            Some(serde_json::json!({"name": name, "version": text(server.get("version")), "source": text(server.get("source"))}))
        })
        .collect();
    let notes: Vec<_> = payload.get("notes").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|note| text(Some(note))).collect();
    serde_json::json!({"servers": servers, "notes": notes})
}

fn counts(project: &Project, reviews: Option<&orq_core::store::Store>, active: &Result<std::collections::HashMap<String, RunCounts>, String>) -> Result<Counts, String> {
    let git = orq_git::resolve_project(&project.path).map_err(|error| format!("{error:?}"))?;
    let key = orq_git::project_id(&git);
    let features = ledger_entries(&orq_features::ledger_path(&git))?;
    let runs = active.as_ref().map_err(Clone::clone)?.get(&key).cloned().unwrap_or_default();
    let reviews_waiting = match reviews {
        Some(store) => orq_core::review_inbox::waiting_count(store, &key)?,
        None => 0,
    };
    Ok(Counts { git, features, active: runs.active, failed_runs: runs.failed, reviews_waiting })
}

/// Active runs per project id, read in one pass over the run store. A record or event log that
/// cannot be read has no known owner, so no project's count can be trusted and this is an error.
fn active_runs_by_project(runs: &orq_orchestrator::RunStore) -> Result<std::collections::HashMap<String, RunCounts>, String> {
    use orq_orchestrator::{RunPhase, StoreError};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut counts: std::collections::HashMap<String, RunCounts> = std::collections::HashMap::new();
    let mut all = Vec::new();
    for run_id in runs.run_ids().map_err(|error| format!("{error:?}"))? {
        let record = match runs.load(&run_id) {
            Ok(record) => record,
            Err(StoreError::NotFound(_)) | Err(StoreError::InvalidRunId(_)) => continue,
            Err(error) => return Err(format!("run {run_id} is unreadable: {error:?}")),
        };
        let events = runs.events(&run_id).map_err(|error| format!("run {run_id} is unreadable: {error:?}"))?;
        let entry = counts.entry(record.project_id.clone()).or_default();
        let summary = orq_orchestrator::summarize_run(&events);
        if summary.phase == RunPhase::Failed && failed_recently(summary.phase_at.as_deref(), now) {
            entry.failed += 1;
        }
        all.push((run_id, record, events));
    }
    // Active as a project's run limit counts them: the one rule the limit itself uses.
    let projects: std::collections::HashMap<&str, &str> = all.iter().map(|(run_id, record, _)| (run_id.as_str(), record.project_id.as_str())).collect();
    for (run_id, feature) in orq_orchestrator::active_runs(&all) {
        let project = projects[run_id.as_str()].to_string();
        counts.entry(project).or_default().active.push((run_id, feature));
    }
    Ok(counts)
}

/// Whether the event that failed the run is no older than the window. A failure with no readable
/// time, or one from the future, is not counted.
fn failed_recently(failed_at: Option<&str>, now: u64) -> bool {
    let Some(at) = failed_at.and_then(orq_orchestrator::parse_rfc3339) else { return false };
    at <= now && now - at <= FAILED_WINDOW_HOURS * 3600
}

/// The most a feature ledger may weigh before the server refuses to read it.
const LEDGER_LIMIT: u64 = 1024 * 1024;

/// How many features the ledger at `path` lists; none when it does not exist. The file is opened
/// without following a link and without waiting on a pipe, must be a regular file, and is read
/// only up to a limit, so a ledger planted in a repository cannot reach elsewhere or stall a request.
fn ledger_entries(path: &Path) -> Result<usize, String> {
    read_ledger(path).map(|entries| entries.len())
}

/// The features marked frontend of the project whose main checkout is `root`, from orq.db at `database`.
/// Without orq.db there are none, and reading never creates, migrates or writes it, so a project with no kinds set
/// is as before.
pub fn frontend_features_at(database: &Path, root: &Path) -> Result<std::collections::HashSet<String>, String> {
    if !database.exists() {
        return Ok(Default::default());
    }
    let names = orq_core::store::Store::read_frontend_features(database, root).map_err(|error| format!("cannot read the feature kinds: {error}"))?;
    Ok(names.into_iter().collect())
}

/// Sets the kind (`frontend` or `standard`) of `feature` of project `git`, whose main checkout is `root`, in orq.db
/// at `database`. `Ok(Err(why))` when it is refused: an unknown kind, or a feature that is not in the ledger.
pub fn set_feature_kind_at(database: &Path, git: &orq_git::Project, root: &Path, feature: &str, kind: &str) -> Result<Result<orq_core::store::FeatureKind, String>, String> {
    let Some(kind) = orq_core::store::FeatureKind::parse(kind) else {
        return Ok(Err(format!("unknown kind {:?}: a feature is frontend or standard", orq_core::redact::redact(kind))));
    };
    if !read_ledger(&orq_features::ledger_path(git))?.iter().any(|entry| entry.feature_name == feature) {
        return Ok(Err(format!("{} is not in this project's ledger", orq_core::redact::redact(feature))));
    }
    let store = orq_core::store::Store::open(database).map_err(|error| format!("cannot open orq.db: {error}"))?;
    store.set_feature_kind(root, feature, kind).map_err(|error| format!("cannot save the kind: {error}"))?;
    Ok(Ok(kind))
}

/// The ledger's entries; a missing ledger has none. It is opened without following a link and
/// read only up to a limit, so a hostile file cannot stall or exhaust the server.
fn read_ledger(path: &Path) -> Result<Vec<orq_features::FeatureEntry>, String> {
    match read_ledger_bytes(path)? {
        Some(bytes) => parse_ledger_bytes(bytes),
        None => Ok(Vec::new()),
    }
}

fn parse_ledger_bytes(bytes: Vec<u8>) -> Result<Vec<orq_features::FeatureEntry>, String> {
    let text = String::from_utf8(bytes).map_err(|_| "the ledger is not text".to_string())?;
    orq_features::parse_ledger(&text).map_err(|error| format!("the ledger is malformed: {error}"))
}

/// The ledger's bytes, read once without following a link and within the size limit; `None` when there is none.
fn read_ledger_bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("the ledger cannot be opened: {:?}", error.kind())),
    };
    let meta = file.metadata().map_err(|error| format!("the ledger cannot be inspected: {:?}", error.kind()))?;
    if !meta.is_file() {
        return Err("the ledger is not a regular file".to_string());
    }
    if meta.len() > LEDGER_LIMIT {
        return Err("the ledger is too large".to_string());
    }
    let mut bytes = Vec::new();
    file.take(LEDGER_LIMIT + 1).read_to_end(&mut bytes).map_err(|error| format!("the ledger cannot be read: {:?}", error.kind()))?;
    if bytes.len() as u64 > LEDGER_LIMIT {
        return Err("the ledger is too large".to_string());
    }
    Ok(Some(bytes))
}

/// The stored value a redacted one stands for: `value` if it is not the redacted form of one of `stored`.
fn unredacted<'a>(value: &str, mut stored: impl Iterator<Item = &'a String>) -> String {
    stored.find(|original| original.as_str() != value && orq_core::redact::redact(original) == value).cloned().unwrap_or_else(|| value.to_string())
}

/// What `program --list-models` prints, or None when it cannot run, fails, says more than 256 KiB
/// or is still running after `timeout`. Only argv is passed; it reads nothing. pi runs in its own process
/// group, and giving up stops the whole group, so a descendant left holding its output can't keep the
/// reader waiting.
fn list_pi_models(program: &Path, timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    const CAP: u64 = 256 * 1024;
    let mut child = std::process::Command::new(program)
        .arg("--list-models")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let group = libc::pid_t::try_from(child.id()).ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.take(CAP + 1).read_to_end(&mut bytes);
        let _ = sender.send(read.ok().map(|_| bytes));
    });
    let deadline = std::time::Instant::now() + timeout;
    let mut output: Option<Option<Vec<u8>>> = None;
    let status = loop {
        if output.is_none() {
            output = receiver.try_recv().ok();
        }
        match child.try_wait() {
            Ok(Some(status)) if output.is_some() => break status,
            Ok(None | Some(_)) if std::time::Instant::now() < deadline => {}
            _ => {
                // Every member of the group is stopped, which closes the pipe and so ends the reader.
                unsafe { libc::killpg(group, libc::SIGKILL) };
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let output = output.flatten()?;
    (status.success() && output.len() as u64 <= CAP).then(|| String::from_utf8_lossy(&output).into_owned())
}

/// The version of a ledger as it was read, for a later save to compare against: the SHA-256 of its bytes.
fn ledger_version(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod failure_log_open_tests {
    use super::open_run_log_with;
    use std::io::Read;
    use std::path::{Path, PathBuf};

    fn root(tag: &str) -> PathBuf {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-failure-log-open-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs/r1")).unwrap();
        std::fs::create_dir_all(dir.join("private")).unwrap();
        std::fs::write(dir.join("logs/r1/gate.log"), "gate\n").unwrap();
        std::fs::write(dir.join("private/gate.log"), "secret\n").unwrap();
        dir
    }

    fn read(opened: Option<(String, std::fs::File)>) -> Option<(String, String)> {
        opened.map(|(name, mut file)| {
            let mut text = String::new();
            file.read_to_string(&mut text).unwrap();
            (name, text)
        })
    }

    fn open(dir: &Path, between: impl FnOnce()) -> Option<(String, String)> {
        let logs = dir.join("logs/r1");
        read(open_run_log_with(&logs.join("gate.log"), &logs, between))
    }

    /// A log that gives `good` and then fails to read.
    struct Failing {
        good: std::io::Cursor<Vec<u8>>,
    }

    impl std::io::Read for Failing {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.good.read(buf)? {
                0 => Err(std::io::Error::other("disk gone")),
                read => Ok(read),
            }
        }
    }

    impl std::io::Seek for Failing {
        fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
            self.good.seek(to)
        }
    }

    fn failing() -> Failing {
        // Partly read before the error: a command, then a line that is no failure yet.
        Failing { good: std::io::Cursor::new(b"$ cargo test\nrunning 3 tests\n".to_vec()) }
    }

    #[test]
    fn failure_log_open_a_read_error_after_part_of_the_log_is_an_error_of_the_scan() {
        let part = super::LogPart::Lines { start: 0, end: None };
        assert!(super::find_failure_line_in(failing(), part).is_err());
    }

    #[test]
    fn failure_log_open_a_log_that_fails_to_read_is_still_named_without_a_line_or_excerpt() {
        let part = super::LogPart::Lines { start: 0, end: None };
        assert_eq!(super::log_shown("gate.log".to_string(), failing(), 30, part), serde_json::json!({"file": "gate.log"}));
    }

    #[test]
    fn failure_log_open_reads_the_runs_own_log() {
        let dir = root("plain");
        assert_eq!(open(&dir, || {}), Some(("gate.log".to_string(), "gate\n".to_string())));
    }

    #[test]
    fn failure_log_open_never_follows_a_log_swapped_for_a_link_after_it_was_checked() {
        let dir = root("file-swap");
        let got = open(&dir, || {
            std::fs::remove_file(dir.join("logs/r1/gate.log")).unwrap();
            std::os::unix::fs::symlink(dir.join("private/gate.log"), dir.join("logs/r1/gate.log")).unwrap();
        });
        assert_eq!(got, None);
    }

    #[test]
    fn failure_log_open_never_follows_a_run_directory_swapped_for_a_link_after_it_was_checked() {
        let dir = root("dir-swap");
        let got = open(&dir, || {
            std::fs::rename(dir.join("logs/r1"), dir.join("logs/r1-old")).unwrap();
            std::os::unix::fs::symlink(dir.join("private"), dir.join("logs/r1")).unwrap();
        });
        assert_eq!(got, None);
    }

    #[test]
    fn failure_log_open_gives_nothing_for_a_log_removed_after_it_was_checked() {
        let dir = root("removed");
        assert_eq!(open(&dir, || std::fs::remove_file(dir.join("logs/r1/gate.log")).unwrap()), None);
    }

    #[test]
    fn failure_log_open_does_not_wait_on_a_pipe_swapped_in() {
        let dir = root("fifo-swap");
        let got = open(&dir, || {
            let path = dir.join("logs/r1/gate.log");
            std::fs::remove_file(&path).unwrap();
            let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        });
        assert_eq!(got, None);
    }
}

#[cfg(test)]
mod run_row_tests {
    use super::run_row;
    use orq_orchestrator::{RunRecord, RunStore};

    #[test]
    fn run_row_skips_a_run_removed_after_its_record_was_read() {
        let dir = orq_test_support::process_temp_dir().join(format!("orq-run-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = RunStore::new(&dir);
        let record = RunRecord { run_id: "gone".into(), project_id: "p".into(), feature_id: "f".into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: "b".into(), worktree: dir.join("w"), repository: String::new(), profiles: Default::default(), delivery: Default::default() };
        store.create(&record).unwrap();
        let loaded = store.load("gone").unwrap();
        // Cleanup removes the run between the two reads.
        std::fs::remove_dir_all(dir.join("runs")).ok();
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
        assert!(matches!(run_row(&store, &dir, &mut Default::default(), loaded, "p"), Ok(None)));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod merged_cache_tests {
    use super::MergedCache;
    use orq_orchestrator::RunRecord;
    use std::collections::HashMap;

    fn record(run: &str) -> RunRecord {
        RunRecord { run_id: run.into(), project_id: "p".into(), feature_id: "f".into(), remote: String::new(), provider: String::new(), target_branch: "main".into(), base_sha: "a".repeat(40), branch: format!("orq/{run}"), worktree: "w".into(), repository: String::new(), profiles: Default::default(), delivery: Default::default() }
    }

    fn tips(branch: &str, main: &str) -> HashMap<String, String> {
        HashMap::from([("orq/r-1".to_string(), branch.to_string()), ("main".to_string(), main.to_string())])
    }

    #[test]
    fn merged_cache_asks_git_again_only_when_the_branch_or_its_target_has_moved() {
        let mut cache = MergedCache::default();
        let mut asked = 0;
        let mut check = |answer: bool| {
            asked += 1;
            answer
        };
        assert!(!cache.merged(&record("r-1"), &tips("b1", "m1"), || check(false)));
        assert!(!cache.merged(&record("r-1"), &tips("b1", "m1"), || check(true)), "the same commits give the same answer");
        assert!(cache.merged(&record("r-1"), &tips("b1", "m2"), || check(true)), "the target moved, so git is asked again");
        assert!(cache.merged(&record("r-1"), &tips("b1", "m2"), || check(false)));
        assert!(!cache.merged(&record("r-1"), &HashMap::new(), || check(true)), "a branch that is gone is not merged");
        assert_eq!(asked, 2);
    }
}

/// One kept worktree for the web: what it holds and how it ended, with no path on this machine.
fn kept_worktree_json(worktree: &orq_orchestrator::KeptWorktree) -> serde_json::Value {
    use orq_orchestrator::ChangeKind;
    let redact = |text: &str| orq_core::redact::redact(text);
    let files: Vec<serde_json::Value> = worktree
        .files
        .iter()
        .map(|file| {
            let kind = match file.kind {
                ChangeKind::Tracked => "tracked",
                ChangeKind::Untracked => "untracked",
                ChangeKind::Ignored => "ignored",
            };
            serde_json::json!({ "path": redact(&file.path), "kind": kind })
        })
        .collect();
    serde_json::json!({
        "branch": redact(&worktree.branch),
        "run": worktree.run_id,
        "ending": worktree.ending,
        "reason": worktree.kept_reason.as_deref().map(redact),
        "target": redact(&worktree.target),
        "commits": worktree.commits,
        "files": files,
        "more_files": worktree.more_files,
        "files_unreadable": worktree.files_unreadable,
        "merged": worktree.merged(),
        "safe": worktree.safe(),
        "bytes": worktree.size,
        "size": worktree.size.map(orq_core::retention::human_bytes),
        "version": worktree.version,
    })
}

/// Deletes `project`'s kept worktree on `branch` through `index`, as the web is told it went.
fn delete_with(index: &mut orq_orchestrator::RunIndex, store: &orq_orchestrator::RunStore, project: &Project, branch: &str, version: &str, force: bool) -> Result<orq_core::remote_inbox::WorktreeDelete, String> {
    use orq_core::remote_inbox::WorktreeDelete;
    use orq_orchestrator::DeleteRefusal;
    match index.delete(store, &project.path, branch, version, force) {
        Ok(Ok(deleted)) => Ok(WorktreeDelete::Deleted(serde_json::json!({
            "project": project.id,
            "branch": orq_core::redact::redact(&deleted.branch),
            "sha": deleted.sha,
            "run": deleted.run_id,
        }))),
        Ok(Err(DeleteRefusal::NotFound)) => Ok(WorktreeDelete::Unknown),
        Ok(Err(refusal)) => Ok(WorktreeDelete::Refused(orq_core::redact::redact(&refusal.to_string()))),
        Err(why) => {
            let why = why.to_string();
            eprintln!("orq serve: a kept worktree could not be deleted: {}", orq_core::redact::redact(&why));
            Err(why)
        }
    }
}
