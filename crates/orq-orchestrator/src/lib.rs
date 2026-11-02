//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod conflict_fix;
mod gate;
mod gate_fix;
mod gate_stage;
mod implement;
mod outcome;
mod push_stage;
mod rebase;
mod review_draft;
mod setup;
mod store;

pub use conflict_fix::{fix_rebase_conflicts, ConflictFixOutcome};
pub use gate::{choose_gate, Gate, GateError};
pub use gate_fix::{run_gate_with_fixes, FixSessionSpec, GateFixOutcome};
pub use gate_stage::{run_gate_stage, GateOutcome};
pub use implement::{run_implement_stage, ImplementOutcome, ImplementSession};
pub use outcome::{check_implement_outcome, ImplementCheck};
pub use push_stage::{push_run_branch, PushOutcome};
pub use rebase::{private_ref_name, rebase_onto_target, RebaseOutcome};
pub use review_draft::{draft_review_description, DraftSession, DraftSource, ReviewDraft};
pub use setup::{plan_setup, run_setup, SetupError, SetupFailure, SetupPlan};
pub use store::{Event, RoleProfile, RunRecord, RunStore, StoreError};
