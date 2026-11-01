//! Pipeline run lifecycle. Separate from a feature's TDD status: a failed run
//! never marks its feature completed, and a feature may stay `pending` while
//! its run is queued.

/// State of one pipeline run. Pause states arrive with MVP 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunState {
    Queued,
    Preparing,
    Running,
    /// Stopped and waiting for a human: the worktree is kept for recovery.
    Blocked,
    /// The MR/PR exists and waits for human review; Orq never merges.
    ReviewReady,
    Completed,
    Failed,
    Cancelled,
}

impl RunState {
    pub const ALL: [RunState; 8] = [
        RunState::Queued,
        RunState::Preparing,
        RunState::Running,
        RunState::Blocked,
        RunState::ReviewReady,
        RunState::Completed,
        RunState::Failed,
        RunState::Cancelled,
    ];

    /// Stable name used in run records and events.
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Queued => "queued",
            RunState::Preparing => "preparing",
            RunState::Running => "running",
            RunState::Blocked => "blocked",
            RunState::ReviewReady => "review_ready",
            RunState::Completed => "completed",
            RunState::Failed => "failed",
            RunState::Cancelled => "cancelled",
        }
    }

    pub fn parse(name: &str) -> Option<RunState> {
        RunState::ALL.into_iter().find(|s| s.as_str() == name)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunState::Completed | RunState::Failed | RunState::Cancelled
        )
    }
}

/// A run state change the lifecycle does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalRunTransition {
    pub from: RunState,
    pub to: RunState,
}

pub fn check_run_transition(from: RunState, to: RunState) -> Result<(), IllegalRunTransition> {
    use RunState::*;
    let allowed = matches!(
        (from, to),
        (Queued, Preparing | Cancelled)
            | (Preparing, Running | Blocked | Failed | Cancelled)
            | (Running, Blocked | ReviewReady | Failed | Cancelled)
            | (Blocked, Running | Failed | Cancelled)
            | (ReviewReady, Completed)
    );
    if allowed {
        Ok(())
    } else {
        Err(IllegalRunTransition { from, to })
    }
}
