//! `orq storage status|list|prune [--dry-run]` and `orq run pin|unpin`: what
//! Orq keeps on disk, and the safe, previewable way to give space back.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::ExitCode;

use orq_core::prune::{prune, reconcile};
use orq_core::retention::{human_bytes, inventory, plan, RetentionPolicy};
use orq_core::storage::{check_run_id, StorageRoots};
use orq_core::store::Store;
use orq_orchestrator::{collect_run_facts, RunStore};

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
    Ok(())
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
    let settled = reconcile(&roots.data, &store).map_err(|e| format!("cannot settle an interrupted prune: {e}"))?;
    if !settled.completed.is_empty() || !settled.abandoned.is_empty() {
        println!("Settled an interrupted prune: {} finished, {} left in place.", settled.completed.len(), settled.abandoned.len());
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
    let current = |run_id: &str| collect_run_facts(&runs, &store).ok().and_then(|mut all| all.remove(run_id));
    let report = prune(&roots.data, &store, &policy, &the_plan, now, &current).map_err(|e| e.to_string())?;
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
