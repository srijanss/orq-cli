//! `orq storage status|list|prune [--dry-run]` and `orq run pin|unpin`: what
//! Orq keeps on disk, and the safe, previewable way to give space back.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::ExitCode;

use orq_core::prune::{prune, reconcile};
use orq_core::retention::{human_bytes, inventory, plan, PrunePlan, RetentionPolicy};
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
    // Best effort: failing to note the time must not turn a finished cleanup into an error.
    // The write never follows a link planted in the data directory.
    let _ = orq_core::storage::write_atomic(&roots.data, std::path::Path::new("last-cleanup"), at.to_string().as_bytes());
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
    let ctx = Ctx { roots, store, runs };
    // A dry run changes nothing, not even the bookkeeping of an interrupted prune.
    if dry_run {
        let pending = ctx.store.pending_removals().map_err(|e| e.to_string())?.len();
        if pending > 0 {
            println!("{pending} removal(s) of an interrupted prune are pending; a real prune will settle them.");
        }
    } else {
        let settled = reconcile(&ctx.roots.data, &ctx.store).map_err(|e| format!("cannot settle an interrupted prune: {e}"))?;
        if !settled.completed.is_empty() || !settled.abandoned.is_empty() {
            println!("Settled an interrupted prune: {} finished, {} left in place.", settled.completed.len(), settled.abandoned.len());
        }
    }
    let policy = policy(&ctx.roots)?;
    let now = now();
    let the_plan = ctx.plan(&policy, now)?;
    for line in the_plan.render() {
        println!("{line}");
    }
    if dry_run {
        if !the_plan.candidates.is_empty() {
            println!("Dry run: nothing was removed.");
        }
        return Ok(());
    }
    let report = ctx.prune(&policy, &the_plan, now)?;
    for line in report_lines(&the_plan, &report) {
        // The CLI indents what it kept or forgot; the headline stands alone.
        if line.starts_with("Removed ") { println!("{line}") } else { println!("  {line}") }
    }
    Ok(())
}

/// The storage context the Settings tab acts on, opened the same way the commands open it.
pub(crate) fn open_ctx() -> Result<Ctx, String> {
    let (roots, store, runs) = open()?;
    Ok(Ctx { roots, store, runs })
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

/// Everything storage commands need, so the CLI and the Settings tab act on the same things.
pub(crate) struct Ctx {
    pub(crate) roots: StorageRoots,
    pub(crate) store: Store,
    pub(crate) runs: RunStore,
}

impl Ctx {
    fn plan(&self, policy: &RetentionPolicy, now: u64) -> Result<orq_core::retention::PrunePlan, String> {
        let facts = collect_run_facts(&self.runs, &self.store).map_err(|e| e.to_string())?;
        let items = inventory(&self.roots.data).map_err(|e| e.to_string())?;
        let mut the_plan = plan(policy, &facts, &items, now);
        // A run whose review metadata is already gone has nothing left to forget.
        the_plan.metadata.retain(|run_id| self.store.has_run_metadata(run_id).unwrap_or(true));
        Ok(the_plan)
    }

    /// Facts are read again per run just before its files go: the world may have moved on since the preview.
    fn prune(&self, policy: &RetentionPolicy, the_plan: &orq_core::retention::PrunePlan, now: u64) -> Result<orq_core::prune::PruneReport, String> {
        let current = |run_id: &str| collect_run_facts_for(&self.runs, &self.store, run_id).ok().flatten();
        let report = prune(&self.roots.data, &self.store, policy, the_plan, now, &current).map_err(|e| e.to_string())?;
        record_cleanup(&self.roots, now);
        Ok(report)
    }
}

fn report_lines(the_plan: &orq_core::retention::PrunePlan, report: &orq_core::prune::PruneReport) -> Vec<String> {
    let mut lines = Vec::new();
    if !the_plan.candidates.is_empty() || !report.metadata_removed.is_empty() {
        lines.push(format!("Removed {} file(s), freed {}.", report.removed.len(), human_bytes(report.freed)));
    }
    lines.extend(report.missing.iter().map(|rel| format!("already gone: {rel}")));
    lines.extend(report.skipped.iter().map(|(rel, why)| format!("kept {rel}: {why}")));
    lines.extend(report.metadata_removed.iter().map(|run| format!("forgot review metadata of run {run}")));
    lines
}

pub(crate) fn ui_view(ctx: &Ctx) -> orq_tui::StorageView {
    let failed = |error: String| orq_tui::StorageView { error: Some(error), ..Default::default() };
    let policy = match policy(&ctx.roots) {
        Ok(policy) => policy,
        Err(error) => return failed(error),
    };
    let items = match inventory(&ctx.roots.data) {
        Ok(items) => items,
        Err(error) => return failed(format!("cannot measure storage: {error}")),
    };
    let pinned = match ctx.store.pinned() {
        Ok(pinned) => pinned,
        Err(error) => return failed(format!("cannot read pinned runs: {error}")),
    };
    let mut by_class: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for item in &items {
        let entry = by_class.entry(item.class.as_str()).or_default();
        entry.0 += 1;
        entry.1 += item.bytes;
    }
    let total: u64 = items.iter().map(|i| i.bytes).sum();
    let mut usage = vec![format!("total: {} file(s), {}", items.len(), human_bytes(total))];
    usage.extend(by_class.into_iter().map(|(class, (count, bytes))| format!("{class}: {count} file(s), {}", human_bytes(bytes))));
    orq_tui::StorageView {
        error: None,
        usage,
        policy: vec![policy_summary(&policy)],
        last_cleanup: last_cleanup(&ctx.roots).map(format_time),
        pinned,
    }
}

/// What a cleanup would remove, as lines to read; empty when there is nothing to remove.
pub(crate) fn ui_preview(ctx: &Ctx) -> Result<(Vec<String>, PrunePlan), String> {
    let policy = policy(&ctx.roots)?;
    let the_plan = ctx.plan(&policy, now())?;
    if the_plan.candidates.is_empty() && the_plan.metadata.is_empty() {
        return Ok((Vec::new(), the_plan));
    }
    Ok((the_plan.render(), the_plan))
}

/// Carries out exactly `previewed`, the plan the person confirmed. Each file is still checked
/// afresh just before it goes, so the plan can only shrink, never grow.
pub(crate) fn ui_prune(ctx: &Ctx, previewed: &PrunePlan) -> Result<Vec<String>, String> {
    reconcile(&ctx.roots.data, &ctx.store).map_err(|e| format!("cannot settle an interrupted prune: {e}"))?;
    let policy = policy(&ctx.roots)?;
    let report = ctx.prune(&policy, previewed, now())?;
    Ok(report_lines(previewed, &report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orq_orchestrator::{Event, RunRecord};

    fn ctx(tag: &str) -> Ctx {
        let base = orq_test_support::process_temp_dir().join(format!("orq-storage-ui-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
        roots.create_all().unwrap();
        let store = Store::open(&roots.database()).unwrap();
        let runs = RunStore::new(&base.join("state/orq"));
        Ctx { roots, store, runs }
    }

    fn old_failed_run(ctx: &Ctx, run_id: &str, bytes: usize) {
        ctx.runs
            .create(&RunRecord {
                run_id: run_id.into(),
                project_id: "p".into(),
                feature_id: "F".into(),
                remote: "origin".into(),
                provider: "github".into(),
                target_branch: "main".into(),
                base_sha: "a".repeat(40),
                branch: "b".into(),
                worktree: ctx.roots.state.join("no-such-worktree"),
                repository: String::new(),
                profiles: std::collections::BTreeMap::new(),
            })
            .unwrap();
        ctx.runs
            .append_event(&Event {
                schema_version: 1,
                event_id: "e1".into(),
                run_id: run_id.into(),
                project_id: "p".into(),
                feature_id: "F".into(),
                kind: "run.failed".into(),
                stage: None,
                attempt: 1,
                occurred_at: "2020-01-01T00:00:00Z".into(),
                payload: serde_json::json!({ "reason": "boom" }),
            })
            .unwrap();
        let log = ctx.roots.data.join(format!("runs/{run_id}/events.jsonl"));
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(log, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn the_view_has_usage_by_category_the_policy_the_last_cleanup_and_pins() {
        let c = ctx("view");
        old_failed_run(&c, "old-run", 2048);
        c.store.pin("keep-me").unwrap();
        let view = ui_view(&c);
        assert_eq!(view.error, None);
        assert!(view.usage.iter().any(|l| l.contains("log") && l.contains("2.0 KiB")), "{:?}", view.usage);
        assert!(view.policy.iter().any(|l| l.contains("failed") && l.contains("30")), "{:?}", view.policy);
        assert_eq!(view.last_cleanup, None);
        assert_eq!(view.pinned, vec!["keep-me".to_string()]);
    }

    #[test]
    fn preview_lists_what_would_go_and_removes_nothing() {
        let c = ctx("preview");
        old_failed_run(&c, "old-run", 2048);
        let (lines, _) = ui_preview(&c).unwrap();
        assert!(lines.iter().any(|l| l.contains("runs/old-run/events.jsonl")), "{lines:?}");
        assert!(c.roots.data.join("runs/old-run/events.jsonl").exists());
        assert_eq!(ui_view(&c).last_cleanup, None, "a preview is not a cleanup");
    }

    #[test]
    fn nothing_to_remove_gives_an_empty_preview() {
        let c = ctx("empty");
        assert_eq!(ui_preview(&c).unwrap().0, Vec::<String>::new());
    }

    #[test]
    fn prune_removes_reports_and_stamps_the_time() {
        let c = ctx("prune");
        old_failed_run(&c, "old-run", 2048);
        let (_, previewed) = ui_preview(&c).unwrap();
        let lines = ui_prune(&c, &previewed).unwrap();
        assert!(lines.iter().any(|l| l.contains("Removed 1 file(s)") && l.contains("2.0 KiB")), "{lines:?}");
        assert!(!c.roots.data.join("runs/old-run/events.jsonl").exists());
        assert!(ui_view(&c).last_cleanup.is_some());
    }

    #[test]
    fn cleanup_removes_only_what_the_confirmed_preview_showed() {
        let c = ctx("confirmed");
        old_failed_run(&c, "shown-run", 100);
        let (_, previewed) = ui_preview(&c).unwrap();
        // The confirmation is open while another run becomes eligible.
        old_failed_run(&c, "later-run", 100);
        ui_prune(&c, &previewed).unwrap();
        assert!(!c.roots.data.join("runs/shown-run/events.jsonl").exists());
        assert!(c.roots.data.join("runs/later-run/events.jsonl").exists(), "never shown, so never removed");
    }

    #[cfg(unix)]
    #[test]
    fn the_cleanup_stamp_is_never_written_through_a_planted_symlink() {
        let c = ctx("symlink");
        let outside = c.roots.state.join("precious.txt");
        std::fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, c.roots.data.join("last-cleanup.tmp")).unwrap();
        std::os::unix::fs::symlink(&outside, c.roots.data.join("last-cleanup")).unwrap();
        let (_, previewed) = ui_preview(&c).unwrap();
        ui_prune(&c, &previewed).unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert!(ui_view(&c).last_cleanup.is_some(), "the stamp replaces the link instead");
    }

    #[test]
    fn a_broken_config_is_an_error_not_a_default_policy() {
        let c = ctx("badconfig");
        std::fs::write(c.roots.config_file(), "[retention]\nfailed_days = \"x\"").unwrap();
        assert!(ui_view(&c).error.is_some());
        assert!(ui_preview(&c).is_err());
        assert!(ui_prune(&c, &PrunePlan::default()).is_err());
    }
}
