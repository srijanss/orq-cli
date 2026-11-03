//! `orq storage status|list|prune [--dry-run]` and `orq run pin|unpin`: what
//! Orq keeps on disk, and the safe, previewable way to give space back.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::ExitCode;

use orq_core::prune::{prune, reconcile};
use orq_core::retention::{human_bytes, inventory, plan, RetentionPolicy};
use orq_core::storage::{check_run_id, StorageRoots};
use orq_core::store::Store;
use orq_orchestrator::{collect_run_facts, collect_run_facts_for, RunStore};

use crate::{fail, pipeline, usage_error};

fn roots() -> Result<StorageRoots, String> {
    let roots = StorageRoots::resolve(&|name| std::env::var(name).ok()).map_err(|err| format!("cannot locate the Orq data directory: {err}"))?;
    roots.create_all().map_err(|err| format!("cannot prepare the Orq data directory: {err}"))?;
    Ok(roots)
}

fn open() -> Result<(StorageRoots, Store, RunStore), String> {
    let roots = roots()?;
    let store = Store::open(&roots.database()).map_err(|err| format!("cannot open the Orq database: {err}"))?;
    let state = pipeline::state_root().ok_or_else(|| "no state location: set XDG_STATE_HOME or HOME".to_string())?;
    Ok((roots, store, RunStore::new(&state)))
}

fn policy(roots: &StorageRoots) -> Result<RetentionPolicy, String> {
    match std::fs::read_to_string(roots.config_file()) {
        Ok(text) => RetentionPolicy::parse(&text).map_err(|err| format!("{}: {err}", roots.config_file().display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(RetentionPolicy::default()),
        Err(err) => Err(format!("cannot read {}: {err}", roots.config_file().display())),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub(crate) fn storage(args: &[OsString]) -> ExitCode {
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    if words.len() != args.len() {
        return usage_error();
    }
    let result = match words.as_slice() {
        ["status"] => status(),
        ["list"] => list(),
        ["prune"] => run_prune(false),
        ["prune", "--dry-run"] => run_prune(true),
        _ => return usage_error(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(message),
    }
}

fn status() -> Result<(), String> {
    let (roots, store, _) = open()?;
    let items = inventory(&roots.data).map_err(|e| e.to_string())?;
    let mut by_class: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for item in &items {
        let entry = by_class.entry(item.class.as_str()).or_default();
        entry.0 += 1;
        entry.1 += item.bytes;
    }
    let runs: std::collections::BTreeSet<&str> = items.iter().map(|i| i.run_id.as_str()).collect();
    let total: u64 = items.iter().map(|i| i.bytes).sum();
    println!("data: {}", roots.data.display());
    println!("{} file(s), {} across {} run(s)", items.len(), human_bytes(total), runs.len());
    for (class, (count, bytes)) in by_class {
        println!("  {class}: {count} file(s), {}", human_bytes(bytes));
    }
    let pinned = store.pinned().map_err(|e| e.to_string())?;
    println!("pinned runs: {}", if pinned.is_empty() { "none".to_string() } else { pinned.join(", ") });
    println!("retention: {}", policy_summary(&policy(&roots)?));
    println!("last cleanup: {}", last_cleanup(&roots).map_or("never".to_string(), format_time));
    Ok(())
}

fn policy_summary(p: &RetentionPolicy) -> String {
    format!(
        "merged {}d, closed {}d, failed {}d, logs {}d, temporary {}d, checkpoints {}d, metadata {}d",
        p.merged_days, p.closed_days, p.failed_days, p.logs_days, p.temporary_days, p.checkpoint_days, p.metadata_days
    )
}

/// Where the time of the last real cleanup is kept: beside the runs, never inside them, so it is never pruned.
fn cleanup_stamp(roots: &StorageRoots) -> std::path::PathBuf {
    roots.data.join("last-cleanup")
}

fn last_cleanup(roots: &StorageRoots) -> Option<u64> {
    std::fs::read_to_string(cleanup_stamp(roots)).ok()?.trim().parse().ok()
}

fn record_cleanup(roots: &StorageRoots, at: u64) {
    let stamp = cleanup_stamp(roots);
    let tmp = stamp.with_extension("tmp");
    // Best effort: failing to note the time must not turn a finished cleanup into an error.
    if std::fs::write(&tmp, at.to_string()).is_ok() && std::fs::rename(&tmp, &stamp).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `YYYY-MM-DD HH:MM UTC` of unix seconds.
fn format_time(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", rem / 3_600, rem % 3_600 / 60)
}

fn list() -> Result<(), String> {
    let (roots, _, _) = open()?;
    for item in inventory(&roots.data).map_err(|e| e.to_string())? {
        println!("{}  {}  {}  {}", item.run_id, item.class.as_str(), human_bytes(item.bytes), item.rel.display());
    }
    Ok(())
}

fn run_prune(dry_run: bool) -> Result<(), String> {
    let (roots, store, runs) = open()?;
    // A dry run changes nothing, not even the bookkeeping of an interrupted prune.
    if dry_run {
        let pending = store.pending_removals().map_err(|e| e.to_string())?.len();
        if pending > 0 {
            println!("{pending} removal(s) of an interrupted prune are pending; a real prune will settle them.");
        }
    } else {
        let settled = reconcile(&roots.data, &store).map_err(|e| format!("cannot settle an interrupted prune: {e}"))?;
        if !settled.completed.is_empty() || !settled.abandoned.is_empty() {
            println!("Settled an interrupted prune: {} finished, {} left in place.", settled.completed.len(), settled.abandoned.len());
        }
    }
    let policy = policy(&roots)?;
    let facts = collect_run_facts(&runs, &store).map_err(|e| e.to_string())?;
    let items = inventory(&roots.data).map_err(|e| e.to_string())?;
    let now = now();
    let mut the_plan = plan(&policy, &facts, &items, now);
    // A run whose review metadata is already gone has nothing left to forget.
    the_plan.metadata.retain(|run_id| store.has_run_metadata(run_id).unwrap_or(true));
    for line in the_plan.render() {
        println!("{line}");
    }
    if dry_run {
        if !the_plan.candidates.is_empty() {
            println!("Dry run: nothing was removed.");
        }
        return Ok(());
    }
    // Facts are read again per run just before its files go: the world may have moved on since the preview.
    let current = |run_id: &str| collect_run_facts_for(&runs, &store, run_id).ok().flatten();
    let report = prune(&roots.data, &store, &policy, &the_plan, now, &current).map_err(|e| e.to_string())?;
    record_cleanup(&roots, now);
    if !the_plan.candidates.is_empty() || !report.metadata_removed.is_empty() {
        println!("Removed {} file(s), freed {}.", report.removed.len(), human_bytes(report.freed));
    }
    for rel in &report.missing {
        println!("  already gone: {rel}");
    }
    for (rel, why) in &report.skipped {
        println!("  kept {rel}: {why}");
    }
    for run in &report.metadata_removed {
        println!("  forgot review metadata of run {run}");
    }
    Ok(())
}

/// `orq run pin|unpin <run-id>`.
pub(crate) fn pin(args: &[OsString], pinned: bool) -> ExitCode {
    let [run_id] = args else { return usage_error() };
    let Some(run_id) = run_id.to_str() else { return usage_error() };
    let result = (|| -> Result<(), String> {
        check_run_id(run_id).map_err(|e| e.to_string())?;
        let (_, store, _) = open()?;
        if pinned { store.pin(run_id) } else { store.unpin(run_id) }.map_err(|e| e.to_string())
    })();
    match result {
        Ok(()) => {
            println!("{} run {run_id}.", if pinned { "Pinned" } else { "Unpinned" });
            ExitCode::SUCCESS
        }
        Err(message) => fail(message),
    }
}
