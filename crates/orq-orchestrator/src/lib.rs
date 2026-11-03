//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod ci_fix;
mod ci_wait;
mod cleanup;
mod conflict_fix;
mod gate;
mod gate_fix;
mod gate_stage;
mod implement;
mod launch;
mod notify;
mod pane;
mod outcome;
mod push_stage;
mod rebase;
mod review_draft;
mod review_observe;
mod retention_facts;
mod review_request;
mod run_list;
mod run_settings;
mod run_status;
mod setup;
mod store;

pub use ci_fix::{run_ci_fix_loop, CiFixConfig, CiFixOutcome, CiFixSteps};
pub use ci_wait::{wait_for_ci, CiOutcome, CiWaitConfig};
pub use cleanup::{cleanup_worktree, CleanupOutcome};
pub use conflict_fix::{fix_rebase_conflicts, ConflictFixOutcome};
pub use gate::{choose_gate, Gate, GateError};
pub use gate_fix::{run_gate_with_fixes, FixSessionSpec, GateFixOutcome};
pub use gate_stage::{run_gate_stage, GateOutcome};
pub use implement::{run_implement_stage, ImplementOutcome, ImplementSession};
pub use launch::record_launch;
pub use pane::{record_pane, run_pane, PaneRef};
pub use outcome::{check_implement_outcome, ImplementCheck};
pub use push_stage::{push_run_branch, PushOutcome};
pub use rebase::{private_ref_name, rebase_onto_target, RebaseOutcome};
pub use review_draft::{draft_review_description, issue_number, link_issue, DraftSession, DraftSource, ReviewDraft};
pub use review_observe::{observe_review, Observed};
pub use retention_facts::{collect_run_facts, collect_run_facts_for};
pub use notify::{notification_message, send_notification, wants_notification};
pub use review_request::{
    create_review_request, create_review_request_with, recorded_review, resolve_repository_url, ReviewPeople, ReviewRef,
    ReviewRequestOutcome,
};
pub use run_list::{list_runs, list_runs_report, RunList, RunListing};
pub use run_settings::{load_run_settings, RunSettings, SettingsError};
pub use run_status::{record_run_completed, summarize_run, RunPhase, RunSummary};
pub use setup::{plan_setup, run_setup, SetupError, SetupFailure, SetupPlan};
pub use store::{Event, RoleProfile, RunRecord, RunStore, StoreError};
