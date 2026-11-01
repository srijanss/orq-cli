//! The ordered stages of one pipeline run (MVP 2 headless orchestration).
//!
//! The order mirrors the Bash orchestrator: one implement-and-review session,
//! rebase, local gate, push, MR/PR, CI. A run ends at `review_ready`; there is
//! deliberately no merge stage, merging stays a human action in the provider.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Prepare,
    ImplementAndReview,
    Rebase,
    Verify,
    Push,
    CreateReview,
    CiCheck,
    ReviewReady,
}

impl Stage {
    const ALL: [Stage; 8] = [
        Stage::Prepare,
        Stage::ImplementAndReview,
        Stage::Rebase,
        Stage::Verify,
        Stage::Push,
        Stage::CreateReview,
        Stage::CiCheck,
        Stage::ReviewReady,
    ];

    /// Stable name used in run records and events.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Prepare => "prepare",
            Stage::ImplementAndReview => "implement_and_review",
            Stage::Rebase => "rebase",
            Stage::Verify => "verify",
            Stage::Push => "push",
            Stage::CreateReview => "create_review",
            Stage::CiCheck => "ci_check",
            Stage::ReviewReady => "review_ready",
        }
    }

    pub fn parse(name: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

/// Which optional stages run (`--no-rebase`, `--no-gate`, `--no-ci-check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageOptions {
    pub rebase: bool,
    pub verify: bool,
    pub ci_check: bool,
}

impl Default for StageOptions {
    fn default() -> Self {
        StageOptions {
            rebase: true,
            verify: true,
            ci_check: true,
        }
    }
}

/// The enabled stages of a run, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    stages: Vec<Stage>,
}

impl StagePlan {
    pub fn new(options: StageOptions) -> StagePlan {
        let enabled = |stage: &Stage| match stage {
            Stage::Rebase => options.rebase,
            Stage::Verify => options.verify,
            Stage::CiCheck => options.ci_check,
            _ => true,
        };
        StagePlan {
            stages: Stage::ALL.into_iter().filter(enabled).collect(),
        }
    }

    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    pub fn first(&self) -> Stage {
        self.stages[0]
    }

    /// The stage after `stage`, or `None` after the last one or for a stage
    /// this plan does not contain.
    pub fn next_after(&self, stage: Stage) -> Option<Stage> {
        let index = self.stages.iter().position(|s| *s == stage)?;
        self.stages.get(index + 1).copied()
    }
}
