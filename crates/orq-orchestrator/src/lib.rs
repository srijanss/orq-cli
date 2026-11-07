//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod checkpoint;
mod child_groups;
mod ci_fix;
mod ci_wait;
mod cleanup;
mod conflict_fix;
mod discard;
mod gate;
mod gate_fix;
mod gate_stage;
mod handoff;
mod implement;
mod interrupted;
mod launch;
mod local_files;
mod needs_input;
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
mod run_activity;
mod run_status;
mod stop;
mod session_references;
mod session_settings;
mod setup;
mod store;
mod terminal_session;
mod worktree_databases;

pub use checkpoint::{
    latest_checkpoint, pause_at_boundary, record_checkpoint, validate_checkpoint, Checkpoint,
};
pub use ci_fix::{run_ci_fix_loop, CiFixConfig, CiFixOutcome, CiFixSteps};
pub use ci_wait::{wait_for_ci, wait_for_ci_logged, CiOutcome, CiWaitConfig};
pub use cleanup::{cleanup_worktree, CleanupOutcome};
pub use conflict_fix::{fix_rebase_conflicts, ConflictFixOutcome};
pub use discard::{discard_run, discard_run_before_remove, discard_run_then, features_no_later_run_took, DiscardOutcome};
pub use gate::{choose_gate, choose_gate_scoped, choose_gate_with, Gate, GateError, GateOptions};
pub use gate_fix::{run_gate_with_fixes, run_gate_with_fixes_env, FixSessionSpec, GateFixOutcome, UNRELATED_FAILURE_FILE};
pub use gate_stage::{run_gate_stage, run_gate_stage_with_env, run_gate_stage_with_limit, GateOutcome};
pub use handoff::{
    handoff_prompt, read_handoff, write_handoff, DiffFile, GateResult, Handoff, HandoffDiff,
    PlanItem, HANDOFF_FILE,
};
pub use implement::{run_implement_stage, ImplementOutcome, ImplementSession};
pub use interrupted::{
    owner_is_alive_in, process_is_alive, process_is_alive_in, process_start_ticks_in,
    reconcile_interrupted,
};
pub use child_groups::end_children_with_this_process;
pub use launch::{record_launch, record_owner};
pub use local_files::{carry_local_files, LocalFiles};
pub use needs_input::{parse_needs_input, question_was_answered, record_input_given, record_needs_input, NeedsInput};
pub use notify::{
    notification_message, review_ready_message, send_notification, send_notification_with, wants_notification,
};
pub use orq_written::record_orq_written;
pub use terminal_session::run_in_terminal;
pub use outcome::{check_implement_outcome, ImplementCheck};
pub use pane::{record_pane, run_pane, PaneRef};
pub use push_stage::{push_run_branch, push_run_branch_with, PushOutcome, RetryPolicy};
pub use reap::{
    plan_orphans, plan_reap, reap_orphan_ref, reap_run, reap_run_databases, Orphans, ReapItem,
    ReapVerdict,
};
pub use rebase::{private_ref_name, rebase_onto_target, RebaseOutcome};
pub use resume::{
    carry_rebase, plan_group_resume, plan_group_resume_with, plan_resume, rebased_onto, record_group,
    record_inherited_implement, run_members,
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
pub use run_activity::{activity_line, newest_log_tail, run_activity, LogTail, RunActivity};
pub use run_status::{
    record_capability_skipped, record_model_switched, record_run_completed, request_model_switch,
    request_pause, summarize_run, RunPhase, RunSummary,
};
pub use session_references::{
    carry_session_references, resumable_session, session_references, SessionReference,
};
pub use session_settings::{exclude_worktrees_dir, prepare_mcp_servers, remove_needs_input_hook, setup_mcp_servers, write_needs_input_hook, write_session_settings};
pub use setup::{plan_setup, run_setup, run_setup_with_env, SetupError, SetupFailure, SetupPlan};
pub use stop::{owner_state, owner_state_in, stop_run, terminate_process, OwnerState};
pub use store::{Claim, Event, RoleProfile, RunRecord, RunStore, StoreError};
pub use worktree_databases::{
    derive_worktree_databases, derive_worktree_databases_with, drop_test_databases,
    drop_test_databases_within, WorktreeDatabases,
};
