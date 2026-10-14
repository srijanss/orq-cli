/// Status of a feature in a project's `.tdd-features.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureStatus {
    Draft,
    Pending,
    InProgress,
    Completed,
    Abandoned,
}

/// A status change Orq's policy does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: FeatureStatus,
    pub to: FeatureStatus,
}

pub fn check_transition(from: FeatureStatus, to: FeatureStatus) -> Result<(), IllegalTransition> {
    match (from, to) {
        (FeatureStatus::Draft, FeatureStatus::Pending) => Ok(()),
        _ => Err(IllegalTransition { from, to }),
    }
}
