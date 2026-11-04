//! How long Orq keeps what it stores, and which runs may be pruned.

/// Days to keep each class of artifact after its run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub merged_days: u32,
    pub closed_days: u32,
    pub failed_days: u32,
    pub logs_days: u32,
    pub temporary_days: u32,
    pub checkpoint_days: u32,
    pub metadata_days: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy { merged_days: 90, closed_days: 90, failed_days: 30, logs_days: 14, temporary_days: 7, checkpoint_days: 30, metadata_days: 180 }
    }
}

impl RetentionPolicy {
    /// Reads the `[retention]` table of a config file; keys left out keep their
    /// defaults. Active runs and unresolved reviews are always protected, so
    /// switching that protection off is refused rather than honoured.
    pub fn parse(text: &str) -> Result<RetentionPolicy, String> {
        let doc: toml_edit::DocumentMut = text.parse().map_err(|err| format!("the config file is not valid TOML: {err}"))?;
        let mut policy = RetentionPolicy::default();
        let Some(item) = doc.get("retention") else { return Ok(policy) };
        let table = item.as_table_like().ok_or("[retention] must be a table")?;
        for (key, value) in table.iter() {
            let days = |slot: &mut u32| -> Result<(), String> {
                let n = value.as_integer().ok_or_else(|| format!("{key} must be a whole number of days"))?;
                *slot = u32::try_from(n).map_err(|_| format!("{key} must be between 0 and {}", u32::MAX))?;
                Ok(())
            };
            match key {
                "merged_days" => days(&mut policy.merged_days)?,
                "closed_days" => days(&mut policy.closed_days)?,
                "failed_days" => days(&mut policy.failed_days)?,
                "logs_days" => days(&mut policy.logs_days)?,
                "temporary_days" => days(&mut policy.temporary_days)?,
                "checkpoint_days" => days(&mut policy.checkpoint_days)?,
                "metadata_days" => days(&mut policy.metadata_days)?,
                "protect_active" | "protect_unresolved_reviews" => {
                    if value.as_bool() != Some(true) {
                        return Err(format!("{key} cannot be turned off: such work is always protected"));
                    }
                }
                other => return Err(format!("{other} is not a retention setting")),
            }
        }
        Ok(policy)
    }
}

use crate::review_lifecycle::ProviderState;
use crate::run::RunState;

/// What Orq knows about one run when deciding whether to prune it.
#[derive(Debug, Clone)]
pub struct RunFacts {
    pub run_id: String,
    pub state: RunState,
    /// Unix seconds of the run's terminal state or last relevant activity.
    pub ended_at: Option<u64>,
    /// What the provider last showed about the run's review, if anything.
    pub review: Option<ReviewFacts>,
    pub pinned: bool,
    pub worktree_in_use: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewFacts {
    pub state: ProviderState,
    /// Unix seconds when the provider was last asked.
    pub observed_at: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactClass {
    Temporary,
    Log,
    Checkpoint,
    Report,
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retained {
    Active,
    AwaitingReview,
    Pinned,
    WorktreeInUse,
    UnresolvedReview,
    RemoteStateUncertain,
    /// Orq has files for the run but no record of it, so nothing is known about it.
    UnknownRun,
    NoEndTime,
    TooRecent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assessment {
    Retained(Retained),
    Prunable(Vec<ArtifactClass>),
}

const DAY_SECS: u64 = 86_400;

pub fn assess(policy: &RetentionPolicy, facts: &RunFacts, now: u64) -> Assessment {
    if facts.pinned {
        return Assessment::Retained(Retained::Pinned);
    }
    match facts.state {
        RunState::Queued
        | RunState::Preparing
        | RunState::Running
        | RunState::PauseRequested
        | RunState::Pausing
        | RunState::Paused
        | RunState::Blocked => {
            return Assessment::Retained(Retained::Active)
        }
        RunState::ReviewReady => return Assessment::Retained(Retained::AwaitingReview),
        RunState::Completed | RunState::Failed | RunState::Cancelled => {}
    }
    if facts.worktree_in_use {
        return Assessment::Retained(Retained::WorktreeInUse);
    }
    let Some(ended_at) = facts.ended_at else {
        return Assessment::Retained(Retained::NoEndTime);
    };
    // Any review the run has, whatever way the run ended, protects it while it is open or unknown.
    if let Some(review) = &facts.review {
        if review.last_error.is_some() {
            return Assessment::Retained(Retained::RemoteStateUncertain);
        }
        if review.state == ProviderState::Open {
            return Assessment::Retained(Retained::UnresolvedReview);
        }
    }
    let report_days = if facts.state == RunState::Completed {
        let Some(review) = &facts.review else {
            return Assessment::Retained(Retained::RemoteStateUncertain);
        };
        match review.state {
            ProviderState::Open => return Assessment::Retained(Retained::UnresolvedReview),
            ProviderState::Closed => {
                if now.saturating_sub(review.observed_at) > DAY_SECS {
                    return Assessment::Retained(Retained::RemoteStateUncertain);
                }
                policy.closed_days
            }
            ProviderState::Merged => policy.merged_days,
        }
    } else {
        policy.failed_days
    };
    let age = now.saturating_sub(ended_at) / DAY_SECS;
    let classes: Vec<ArtifactClass> = [
        (ArtifactClass::Temporary, policy.temporary_days),
        (ArtifactClass::Log, policy.logs_days),
        (ArtifactClass::Checkpoint, policy.checkpoint_days),
        (ArtifactClass::Report, report_days),
        (ArtifactClass::Metadata, policy.metadata_days),
    ]
    .into_iter()
    .filter(|(_, days)| age >= u64::from(*days))
    .map(|(class, _)| class)
    .collect();
    if classes.is_empty() {
        Assessment::Retained(Retained::TooRecent)
    } else {
        Assessment::Prunable(classes)
    }
}

/// One file a run left under the data root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub run_id: String,
    pub class: ArtifactClass,
    /// Path relative to the data root.
    pub rel: std::path::PathBuf,
    pub bytes: u64,
}

/// The class of a file Orq itself wrote, from its path relative to the data
/// root (`runs/<run>/...`). `None` for anything Orq does not recognise.
pub fn classify(rel: &std::path::Path) -> Option<ArtifactClass> {
    use std::path::Component;
    let parts: Vec<&str> = rel
        .components()
        .map(|c| match c {
            Component::Normal(p) => p.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let is_temp = |name: &str| name.starts_with('.') && name.ends_with(".tmp");
    match parts.as_slice() {
        ["runs", _, name] => match *name {
            "events.jsonl" => Some(ArtifactClass::Log),
            "handoff.json" => Some(ArtifactClass::Checkpoint),
            "tests.json" | "review-rounds.json" => Some(ArtifactClass::Report),
            n if is_temp(n) => Some(ArtifactClass::Temporary),
            _ => None,
        },
        ["runs", _, "review", .., last] => Some(if is_temp(last) { ArtifactClass::Temporary } else { ArtifactClass::Report }),
        ["runs", _, "tmp", ..] if parts.len() >= 4 => Some(ArtifactClass::Temporary),
        _ => None,
    }
}

/// Lists the files Orq itself wrote for each run, by artifact class. Anything
/// Orq does not recognise (other tools' files, worktrees, symlinks) is left
/// out, so it can never be counted or pruned. Sorted by run, then path.
pub fn inventory(data_root: &std::path::Path) -> std::io::Result<Vec<Artifact>> {
    use std::path::{Path, PathBuf};

    /// Everything under a recognised directory shares its class, except
    /// leftover atomic-write temp files, which are always temporary.
    fn walk(dir: &Path, rel: &Path, run_id: &str, class: ArtifactClass, out: &mut Vec<Artifact>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let Some(name) = entry.file_name().to_str().map(String::from) else { continue };
            let rel = rel.join(&name);
            if kind.is_dir() {
                walk(&entry.path(), &rel, run_id, class, out)?;
            } else if kind.is_file() {
                let class = if is_temp_name(&name) { ArtifactClass::Temporary } else { class };
                out.push(Artifact { run_id: run_id.to_string(), class, rel, bytes: entry.metadata()?.len() });
            }
        }
        Ok(())
    }

    fn is_temp_name(name: &str) -> bool {
        name.starts_with('.') && name.ends_with(".tmp")
    }

    let runs = data_root.join("runs");
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(&runs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let Some(run_id) = entry.file_name().to_str().map(String::from) else { continue };
        if crate::storage::check_run_id(&run_id).is_err() || !entry.file_type()?.is_dir() {
            continue;
        }
        let run_dir = entry.path();
        let rel_run = PathBuf::from("runs").join(&run_id);
        for child in std::fs::read_dir(&run_dir)? {
            let child = child?;
            let kind = child.file_type()?;
            let Some(name) = child.file_name().to_str().map(String::from) else { continue };
            let rel = rel_run.join(&name);
            if kind.is_symlink() {
                continue;
            }
            if kind.is_file() {
                let class = match name.as_str() {
                    "events.jsonl" => Some(ArtifactClass::Log),
                    "handoff.json" => Some(ArtifactClass::Checkpoint),
                    "tests.json" | "review-rounds.json" => Some(ArtifactClass::Report),
                    n if is_temp_name(n) => Some(ArtifactClass::Temporary),
                    _ => None,
                };
                if let Some(class) = class {
                    found.push(Artifact { run_id: run_id.clone(), class, rel, bytes: child.metadata()?.len() });
                }
            } else if kind.is_dir() {
                let class = match name.as_str() {
                    "review" => ArtifactClass::Report,
                    "tmp" => ArtifactClass::Temporary,
                    _ => continue,
                };
                walk(&child.path(), &rel, &run_id, class, &mut found)?;
            }
        }
    }
    found.sort_by(|a, b| (&a.run_id, &a.rel).cmp(&(&b.run_id, &b.rel)));
    Ok(found)
}

/// One file a prune would remove, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub artifact: Artifact,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub run_id: String,
    pub why: Retained,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrunePlan {
    pub candidates: Vec<Candidate>,
    /// Runs whose stored metadata is also past retention.
    pub metadata: Vec<String>,
    pub kept: Vec<Kept>,
}

impl PrunePlan {
    pub fn total_bytes(&self) -> u64 {
        self.candidates.iter().map(|c| c.artifact.bytes).sum()
    }

    /// The preview a person reads before anything is removed: each file with
    /// its size and the reason, then what is being kept and why.
    pub fn render(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.candidates.is_empty() && self.metadata.is_empty() {
            lines.push("Nothing to prune.".to_string());
        } else {
            let runs: std::collections::BTreeSet<&str> = self.candidates.iter().map(|c| c.artifact.run_id.as_str()).collect();
            lines.push(format!("{} file(s), {} across {} run(s):", self.candidates.len(), human_bytes(self.total_bytes()), runs.len()));
            for c in &self.candidates {
                lines.push(format!("  {}  {}  {}", c.artifact.rel.display(), human_bytes(c.artifact.bytes), c.reason));
            }
            for run in &self.metadata {
                lines.push(format!("  metadata of run {run}  past its retention"));
            }
        }
        if !self.kept.is_empty() {
            lines.push("Kept:".to_string());
            for k in &self.kept {
                lines.push(format!("  {}  {}", k.run_id, k.why.describe()));
            }
        }
        lines
    }
}

impl Retained {
    pub fn describe(self) -> &'static str {
        match self {
            Retained::Active => "the run is still active",
            Retained::AwaitingReview => "the run is waiting for human review",
            Retained::Pinned => "the run is pinned",
            Retained::WorktreeInUse => "the run's worktree is still in use",
            Retained::UnresolvedReview => "its review is still open",
            Retained::RemoteStateUncertain => "the review's remote state is unknown or out of date",
            Retained::UnknownRun => "there is no record of the run",
            Retained::NoEndTime => "the run has no recorded end time",
            Retained::TooRecent => "not old enough yet",
        }
    }
}

impl ArtifactClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactClass::Temporary => "temporary",
            ArtifactClass::Log => "log",
            ArtifactClass::Checkpoint => "checkpoint",
            ArtifactClass::Report => "report",
            ArtifactClass::Metadata => "metadata",
        }
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Decides, for every file in the inventory, whether a prune would remove it.
/// A file whose run has no facts is kept: nothing is known about it.
pub fn plan(policy: &RetentionPolicy, facts: &std::collections::BTreeMap<String, RunFacts>, items: &[Artifact], now: u64) -> PrunePlan {
    use std::collections::BTreeMap;
    let mut plan = PrunePlan::default();
    let mut assessed: BTreeMap<&str, Assessment> = BTreeMap::new();
    for (run_id, run) in facts {
        let assessment = assess(policy, run, now);
        if matches!(&assessment, Assessment::Prunable(classes) if classes.contains(&ArtifactClass::Metadata)) {
            plan.metadata.push(run_id.clone());
        }
        assessed.insert(run_id, assessment);
    }
    let mut kept: BTreeMap<&str, Retained> = BTreeMap::new();
    for artifact in items {
        let run_id = artifact.run_id.as_str();
        match (facts.get(run_id), assessed.get(run_id)) {
            (Some(run), Some(Assessment::Prunable(classes))) if classes.contains(&artifact.class) => {
                let age = run.ended_at.map_or(0, |ended| now.saturating_sub(ended) / DAY_SECS);
                plan.candidates.push(Candidate {
                    artifact: artifact.clone(),
                    reason: format!("{} run ended {age} days ago; its {} files are past retention", run.state.as_str(), artifact.class.as_str()),
                });
            }
            (Some(_), Some(Assessment::Prunable(_))) => {}
            (Some(_), Some(Assessment::Retained(why))) => {
                kept.insert(run_id, *why);
            }
            _ => {
                kept.insert(run_id, Retained::UnknownRun);
            }
        }
    }
    plan.kept = kept.into_iter().map(|(run_id, why)| Kept { run_id: run_id.to_string(), why }).collect();
    plan
}

/// Optional scheduled maintenance, read from `[storage_maintenance]`. `None` means it is off,
/// which is the default; turning it on needs an interval in whole days.
pub fn parse_maintenance_interval(text: &str) -> Result<Option<u64>, String> {
    let doc: toml_edit::DocumentMut = text.parse().map_err(|_: toml_edit::TomlError| "the config file is not valid TOML".to_string())?;
    let Some(item) = doc.get("storage_maintenance") else { return Ok(None) };
    let table = item.as_table_like().ok_or("[storage_maintenance] must be a table")?;
    let mut enabled = false;
    let mut days = None;
    for (key, value) in table.iter() {
        match key {
            "enabled" => enabled = value.as_bool().ok_or("storage_maintenance.enabled must be true or false")?,
            "interval_days" => {
                let n = value.as_integer().ok_or("storage_maintenance.interval_days must be a whole number of days")?;
                days = Some(u64::try_from(n).map_err(|_| "storage_maintenance.interval_days must be at least 1")?);
            }
            _ => return Err("storage_maintenance has an unknown setting".to_string()),
        }
    }
    if !enabled {
        return Ok(None);
    }
    match days {
        Some(d) if d >= 1 => Ok(Some(d.saturating_mul(86_400))),
        Some(_) => Err("storage_maintenance.interval_days must be at least 1".to_string()),
        None => Err("storage_maintenance.enabled needs storage_maintenance.interval_days".to_string()),
    }
}
