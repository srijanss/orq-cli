pub mod naming;
pub mod no_merge;
pub mod ownership;
pub mod passkey_challenge;
pub mod passkey_enroll;
pub mod passkey_login;
pub mod passkey_register;
pub mod passkey_store;
mod es256;
pub mod passkey_routes;
pub mod provider_observation;
pub mod prune;
pub mod redact;
pub mod remote_inbox;
pub mod retention;
pub mod review_inbox;
pub mod review_intelligence;
pub mod review_lifecycle;
pub mod review_report;
pub mod review_input;
pub mod review_summary;
pub mod run;
pub mod session;
pub mod stage;
pub mod storage;
pub mod store;

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
