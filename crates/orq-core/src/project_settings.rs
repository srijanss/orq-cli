//! A project's own settings, kept in orq.db on this machine and keyed by the path of the project's
//! main checkout: the agents a run uses when it names none, and what a run does when it finishes.
//! A role left unset falls back to `.tdd-config.json`, `[roles]` and the machine defaults.

use std::io::{Error, ErrorKind, Result};

/// What a finished run does with its branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// Push, open the PR/MR and wait for CI.
    #[default]
    Review,
    /// Pushed the branch and stopped. Removed: no run or setting may choose it, and it is kept only
    /// so a run record that names it still reads.
    Push,
    /// Fast-forward the base branch locally; nothing is pushed.
    MergeLocal,
}

/// Why a run, a flag or a setting may not name [`Delivery::Push`].
pub const PUSH_REMOVED: &str = "the push delivery was removed: use review (pushes and opens a review) or merge-local";

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Delivery::Review => "review",
            Delivery::Push => "push",
            Delivery::MergeLocal => "merge_local",
        }
    }

    pub fn parse(text: &str) -> Option<Delivery> {
        match text {
            "review" => Some(Delivery::Review),
            "push" => Some(Delivery::Push),
            "merge_local" => Some(Delivery::MergeLocal),
            _ => None,
        }
    }
}

/// What the server does once it sees a run's MR merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AfterMerge {
    /// Offer Clean up on the run, its review and the project, for a person to press.
    #[default]
    Ask,
    /// Clean up on its own, and send a notification saying how it went.
    Auto,
}

impl AfterMerge {
    pub fn as_str(self) -> &'static str {
        match self {
            AfterMerge::Ask => "ask",
            AfterMerge::Auto => "auto",
        }
    }

    pub fn parse(text: &str) -> Option<AfterMerge> {
        match text {
            "ask" => Some(AfterMerge::Ask),
            "auto" => Some(AfterMerge::Auto),
            _ => None,
        }
    }
}

/// The agent a project uses for one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAgent {
    pub harness: String,
    pub model: String,
    /// The run's thinking level; unset leaves the harness's own default.
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSettings {
    pub implementer: Option<ProjectAgent>,
    pub reviewer: Option<ProjectAgent>,
    pub delivery: Delivery,
    /// How many of the project's runs may be active at once; unset is no limit.
    pub max_active_runs: Option<u8>,
    /// The free disk, in GB, a run needs to start or resume; unset is no check.
    pub min_free_disk_gb: Option<u32>,
    /// When the queue may start runs, as `HH:MM-HH:MM` in local time (the end excluded; a window
    /// may wrap midnight); unset is any time.
    pub queue_hours: Option<String>,
    /// Whether a queued run that fails or is blocked pauses the queue.
    pub queue_stop_on_failure: bool,
    /// Whether the queue starts nothing more while a run it started waits for review: an open MR,
    /// or a run that needs a person's review. On by default; off, only the run limit holds it.
    pub queue_wait_for_review: bool,
    /// Whether a project whose runs end in an MR has its local target fast-forwarded from the
    /// remote after an MR merges and before each run. On by default; off, the target is left as it is.
    pub pull_target_before_runs: bool,
    /// What happens once a run's MR merged: Clean up offered (the default), or done unasked.
    pub after_merge: AfterMerge,
}

impl Default for ProjectSettings {
    fn default() -> ProjectSettings {
        ProjectSettings {
            implementer: None,
            reviewer: None,
            delivery: Delivery::default(),
            max_active_runs: None,
            min_free_disk_gb: None,
            queue_hours: None,
            queue_stop_on_failure: false,
            queue_wait_for_review: true,
            pull_target_before_runs: true,
            after_merge: AfterMerge::default(),
        }
    }
}

/// The range a project's run limit may take.
pub const MAX_ACTIVE_RUNS: std::ops::RangeInclusive<u8> = 1..=8;
/// The range a project's disk floor may take, in GB.
pub const MIN_FREE_DISK_GB: std::ops::RangeInclusive<u32> = 1..=500;

/// A project's settings with the revision a later save must name.
/// Revision 0 means the project has no record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProjectSettings {
    pub settings: ProjectSettings,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved { revision: u64 },
    /// The project was saved since the revision the save was based on; nothing was written.
    Conflict,
}

/// Every value is a single non-empty line with no surrounding blanks, the delivery is one that
/// may still be chosen, and each limit is in its range.
pub(crate) fn check(settings: &ProjectSettings) -> Result<()> {
    if settings.delivery == Delivery::Push {
        return Err(Error::new(ErrorKind::InvalidInput, PUSH_REMOVED));
    }
    if let Some(limit) = settings.max_active_runs.filter(|limit| !MAX_ACTIVE_RUNS.contains(limit)) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("max_active_runs must be empty or 1 to 8, got {limit}")));
    }
    if let Some(floor) = settings.min_free_disk_gb.filter(|floor| !MIN_FREE_DISK_GB.contains(floor)) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("min_free_disk_gb must be empty or 1 to 500, got {floor}")));
    }
    if let Some(hours) = settings.queue_hours.as_deref().filter(|hours| parse_queue_hours(hours).is_none()) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("queue_hours must be empty or HH:MM-HH:MM with two different times, got {hours:?}")));
    }
    for (role, agent) in [("implementer", &settings.implementer), ("reviewer", &settings.reviewer)] {
        let Some(agent) = agent else { continue };
        check_value(role, "harness", &agent.harness)?;
        check_value(role, "model", &agent.model)?;
        if let Some(effort) = &agent.effort {
            check_value(role, "effort", effort)?;
        }
    }
    Ok(())
}

fn check_value(role: &str, field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{role} {field} must be a single non-blank line without surrounding spaces, got {value:?}")));
    }
    Ok(())
}

/// The start and end of a queue window `HH:MM-HH:MM`, in minutes after midnight; `None` when it is
/// not one, or starts and ends at the same time.
pub fn parse_queue_hours(hours: &str) -> Option<(u16, u16)> {
    let minute = |time: &str| -> Option<u16> {
        let (h, m) = time.split_once(':')?;
        if h.len() != 2 || m.len() != 2 || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
        (h < 24 && m < 60).then_some(h * 60 + m)
    };
    let (start, end) = hours.split_once('-')?;
    let (start, end) = (minute(start)?, minute(end)?);
    (start != end).then_some((start, end))
}

/// Whether `minute_of_day` (minutes after local midnight) falls inside the window `hours`: from its
/// start up to, not including, its end, across midnight when the end comes first. A window that
/// cannot be read holds no time.
pub fn within_queue_hours(hours: &str, minute_of_day: u16) -> bool {
    match parse_queue_hours(hours) {
        Some((start, end)) if start < end => (start..end).contains(&minute_of_day),
        Some((start, end)) => minute_of_day >= start || minute_of_day < end,
        None => false,
    }
}
