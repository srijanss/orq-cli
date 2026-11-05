//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod checkpoint;
mod ci_fix;
mod ci_wait;
mod cleanup;
mod conflict_fix;
mod gate;
mod gate_fix;
mod gate_stage;
mod handoff;
mod implement;
mod interrupted;
mod launch;
mod local_files;
mod notify;
mod orq_written;
mod outcome;
mod pane;
mod push_stage;
mod reap;
mod rebase;
mod resume;
mod retention_facts;
mod review_draft;
mod review_observe;
mod review_request;
mod run_list;
mod run_settings;
mod run_status;
mod session_references;
mod session_settings;
mod setup;
mod store;
mod worktree_databases;

pub use checkpoint::{
    latest_checkpoint, pause_at_boundary, record_checkpoint, validate_checkpoint, Checkpoint,
};
pub use ci_fix::{run_ci_fix_loop, CiFixConfig, CiFixOutcome, CiFixSteps};
pub use ci_wait::{wait_for_ci, CiOutcome, CiWaitConfig};
pub use cleanup::{cleanup_worktree, CleanupOutcome};
pub use conflict_fix::{fix_rebase_conflicts, ConflictFixOutcome};
pub use gate::{choose_gate, choose_gate_scoped, choose_gate_with, Gate, GateError, GateOptions};
pub use gate_fix::{run_gate_with_fixes, run_gate_with_fixes_env, FixSessionSpec, GateFixOutcome};
pub use gate_stage::{run_gate_stage, run_gate_stage_with_env, GateOutcome};
pub use handoff::{
    handoff_prompt, read_handoff, write_handoff, DiffFile, GateResult, Handoff, HandoffDiff,
    PlanItem, HANDOFF_FILE,
};
pub use implement::{run_implement_stage, ImplementOutcome, ImplementSession};
pub use interrupted::{
    owner_is_alive_in, process_is_alive, process_is_alive_in, process_start_ticks_in,
    reconcile_interrupted,
};
pub use launch::record_launch;
pub use local_files::{carry_local_files, LocalFiles};
pub use notify::{
    notification_message, send_notification, send_notification_with, wants_notification,
};
pub use orq_written::record_orq_written;
pub use outcome::{check_implement_outcome, ImplementCheck};
pub use pane::{record_pane, run_pane, PaneRef};
pub use push_stage::{push_run_branch, PushOutcome};
pub use reap::{
    plan_orphans, plan_reap, reap_orphan_ref, reap_run, reap_run_databases, Orphans, ReapItem,
    ReapVerdict,
};
pub use rebase::{private_ref_name, rebase_onto_target, RebaseOutcome};
pub use resume::{
    plan_group_resume, plan_resume, record_group, record_inherited_implement, run_members,
    supersede_paused, ResumePlan,
};
pub use retention_facts::{collect_run_facts, collect_run_facts_for};
pub use review_draft::{
    draft_review_description, issue_number, link_issue, DraftSession, DraftSource, ReviewDraft,
};
pub use review_observe::{observe_review, Observed};
pub use review_request::{
    create_review_request, create_review_request_with, recorded_review, resolve_repository_url,
    ReviewPeople, ReviewRef, ReviewRequestOutcome,
};
pub use run_list::{list_runs, list_runs_report, RunList, RunListing};
pub use run_settings::{
    load_run_settings, model as checked_model, save_mcp_servers_setting, RunSettings, SettingsError,
};
pub use run_status::{
    record_capability_skipped, record_model_switched, record_run_completed, request_model_switch,
    request_pause, summarize_run, RunPhase, RunSummary,
};
pub use session_references::{
    carry_session_references, resumable_session, session_references, SessionReference,
};
pub use session_settings::{exclude_worktrees_dir, setup_mcp_servers, write_session_settings};
pub use setup::{plan_setup, run_setup, run_setup_with_env, SetupError, SetupFailure, SetupPlan};
pub use store::{Event, RoleProfile, RunRecord, RunStore, StoreError};
pub use worktree_databases::{
    derive_worktree_databases, derive_worktree_databases_with, drop_test_databases,
    drop_test_databases_within, WorktreeDatabases,
};
