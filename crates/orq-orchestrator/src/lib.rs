//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod gate;
mod implement;
mod setup;
mod store;

pub use gate::{choose_gate, Gate, GateError};
pub use implement::{run_implement_stage, ImplementOutcome, ImplementSession};
pub use setup::{plan_setup, run_setup, SetupError, SetupFailure, SetupPlan};
pub use store::{Event, RoleProfile, RunRecord, RunStore, StoreError};
