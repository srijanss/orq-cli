//! `orq review build|show`: the deterministic review report, built from Git
//! and the run's recorded evidence and kept in Orq's own storage. Nothing here
//! contacts a provider.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orq_core::review_input::load_review_input;
use orq_core::review_report::{build_report, load_test_evidence, render_markdown, ReportFile, ReportInputs};
use orq_core::storage::{check_run_id, write_atomic, StorageRoots};
use orq_core::store::Store;

use crate::{fail, pipeline, usage_error};

pub(crate) fn review(args: &[OsString]) -> ExitCode {
    match args.first().and_then(|arg| arg.to_str()) {
        Some("build") => build(&args[1..]),
        Some("show") => show(&args[1..]),
        _ => usage_error(),
    }
}

fn roots() -> Result<StorageRoots, String> {
    let roots = StorageRoots::resolve(&|name| std::env::var(name).ok()).map_err(|err| format!("cannot locate the Orq data directory: {err}"))?;
    roots.create_all().map_err(|err| format!("cannot prepare the Orq data directory: {err}"))?;
    Ok(roots)
}

struct BuildArgs {
    run_id: String,
    repo: PathBuf,
    feature: String,
    base: String,
    head: String,
    review_input: Option<PathBuf>,
}

fn parse_build(args: &[OsString]) -> Option<BuildArgs> {
    let run_id = args.first()?.to_str()?.to_string();
    let (mut repo, mut feature, mut base, mut head, mut review_input) = (None, None, None, "HEAD".to_string(), None);
    let mut rest = args[1..].chunks(2);
    for pair in &mut rest {
        let [flag, value] = pair else { return None };
        match flag.to_str()? {
            "--repo" => repo = Some(PathBuf::from(value)),
            "--feature" => feature = Some(value.to_str()?.to_string()),
            "--base" => base = Some(value.to_str()?.to_string()),
            "--head" => head = value.to_str()?.to_string(),
            "--review-input" => review_input = Some(PathBuf::from(value)),
            _ => return None,
        }
    }
    Some(BuildArgs { run_id, repo: repo?, feature: feature?, base: base?, head, review_input })
}

fn build(args: &[OsString]) -> ExitCode {
    let Some(parsed) = parse_build(args) else { return usage_error() };
    if let Err(err) = check_run_id(&parsed.run_id) {
        return fail(format!("invalid run id: {err}"));
    }
    let project = match orq_git::resolve_project(&parsed.repo) {
        Ok(project) => project,
        Err(err) => return fail(format!("cannot resolve project: {err:?}")),
    };
    let diff = match orq_git::diff_evidence(&project.root, &parsed.base, &parsed.head) {
        Ok(diff) => diff,
        Err(err) => return fail(format!("cannot read the diff: {err:?}")),
    };
    let roots = match roots() {
        Ok(roots) => roots,
        Err(err) => return fail(err),
    };
    let project_id = pipeline::project_id(&project);
    let inputs = ReportInputs {
        run_id: parsed.run_id.clone(),
        project_id: project_id.clone(),
        feature_id: parsed.feature.clone(),
        base_sha: diff.base_sha,
        head_sha: diff.head_sha.clone(),
        files: diff.files.into_iter().map(|f| ReportFile { path: f.path, old_path: f.old_path, added: f.added, deleted: f.deleted }).collect(),
        tests: load_test_evidence(&roots.runs().join(&parsed.run_id).join("tests.json")),
        review: match &parsed.review_input {
            Some(path) => load_review_input(path),
            None => Err(orq_core::review_input::InputError::Missing),
        },
        generated_at: utc_now(),
    };
    let report = build_report(&inputs);
    let json = match serde_json::to_vec_pretty(&report) {
        Ok(json) => json,
        Err(err) => return fail(err),
    };
    let store = match Store::open(&roots.database()) {
        Ok(store) => store,
        Err(err) => return fail(format!("cannot open the review index: {err}")),
    };
    // The readable copy goes first: if it cannot be written nothing is
    // published or indexed, so a failed build leaves no half-visible report.
    let markdown = orq_core::redact::redact(&render_markdown(&report));
    let markdown_path = format!("runs/{}/review/{}/review.md", parsed.run_id, diff.head_sha);
    let previous = std::fs::read(roots.data.join(&markdown_path)).ok();
    if let Err(err) = write_atomic(&roots.data, Path::new(&markdown_path), markdown.as_bytes()) {
        return fail(format!("cannot store the readable report: {err}"));
    }
    let record = match store.save_review(&roots.data, &parsed.run_id, &project_id, &parsed.feature, &diff.head_sha, &json) {
        Ok(record) => record,
        Err(err) => {
            // Put the published pair back as it was: the old readable copy, or none.
            match previous {
                Some(old) => {
                    let _ = write_atomic(&roots.data, Path::new(&markdown_path), &old);
                }
                None => {
                    let _ = std::fs::remove_file(roots.data.join(&markdown_path));
                }
            }
            return fail(format!("cannot store the review report: {err}"));
        }
    };
    println!("{}", roots.data.join(&record.report_path).display());
    ExitCode::SUCCESS
}

fn show(args: &[OsString]) -> ExitCode {
    let (run_id, json) = match args {
        [run] => (run.to_str(), false),
        [run, flag] if flag == "--json" => (run.to_str(), true),
        _ => return usage_error(),
    };
    let Some(run_id) = run_id else { return usage_error() };
    if let Err(err) = check_run_id(run_id) {
        return fail(format!("invalid run id: {err}"));
    }
    let roots = match roots() {
        Ok(roots) => roots,
        Err(err) => return fail(err),
    };
    let store = match Store::open(&roots.database()) {
        Ok(store) => store,
        Err(err) => return fail(format!("cannot open the review index: {err}")),
    };
    let records = match store.reviews_for_run(run_id) {
        Ok(records) => records,
        Err(err) => return fail(format!("cannot read the review index: {err}")),
    };
    let Some(record) = records.last() else { return fail(format!("no review report for run {run_id}")) };
    let bytes = match std::fs::read(roots.data.join(&record.report_path)) {
        Ok(bytes) => bytes,
        Err(err) => return fail(format!("cannot read the stored report for run {run_id}: {err}")),
    };
    let report: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(report) => report,
        Err(err) => return fail(format!("the stored report for run {run_id} is unreadable: {err}")),
    };
    if json {
        println!("{}", String::from_utf8_lossy(&bytes));
    } else {
        print!("{}", render_markdown(&report));
    }
    ExitCode::SUCCESS
}

/// The current UTC time as RFC 3339, without a date-time dependency.
fn utc_now() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}
