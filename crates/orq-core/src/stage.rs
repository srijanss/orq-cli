//! The ordered stages of one pipeline run (MVP 2 headless orchestration).
//!
//! The order mirrors the Bash orchestrator: one implement-and-review session,
//! rebase, local gate, push, MR/PR, CI. A run ends at `review_ready`; there is
//! deliberately no merge stage, merging stays a human action in the provider.
//! A frontend run (a feature with a design package) has two more steps: its
//! package is checked before the implementer starts, and its page is reviewed
//! against the design after the rebase, before the gate.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Prepare,
    /// Frontend only: the design package is frozen and checked.
    DesignPackage,
    ImplementAndReview,
    Rebase,
    /// Frontend only: capture, design review and correction rounds.
    DesignReview,
    Verify,
    Push,
    CreateReview,
    CiCheck,
    ReviewReady,
}

impl Stage {
    /// Every stage a stored run may name, in a frontend run's order.
    const ALL: [Stage; 10] = [
        Stage::Prepare,
        Stage::DesignPackage,
        Stage::ImplementAndReview,
        Stage::Rebase,
        Stage::DesignReview,
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
            Stage::DesignPackage => "design_package",
            Stage::ImplementAndReview => "implement_and_review",
            Stage::Rebase => "rebase",
            Stage::DesignReview => "design_review",
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

    fn is_design(self) -> bool {
        matches!(self, Stage::DesignPackage | Stage::DesignReview)
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
    /// The standard run's stages: never a design step.
    pub fn new(options: StageOptions) -> StagePlan {
        StagePlan::with(options, false)
    }

    /// A frontend run's stages: the standard ones with the design package and design review.
    pub fn frontend(options: StageOptions) -> StagePlan {
        StagePlan::with(options, true)
    }

    fn with(options: StageOptions, frontend: bool) -> StagePlan {
        let enabled = |stage: &Stage| match stage {
            Stage::Rebase => options.rebase,
            Stage::Verify => options.verify,
            Stage::CiCheck => options.ci_check,
            stage if stage.is_design() => frontend,
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
