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
        RunState::Queued | RunState::Preparing | RunState::Running | RunState::Blocked => {
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
    let report_days = if facts.state == RunState::Completed {
        let Some(review) = &facts.review else {
            return Assessment::Retained(Retained::RemoteStateUncertain);
        };
        if review.last_error.is_some() {
            return Assessment::Retained(Retained::RemoteStateUncertain);
        }
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
