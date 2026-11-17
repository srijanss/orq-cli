//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod checkpoint;
mod child_groups;
mod ci_fix;
mod ci_wait;
mod accept;
mod cleanup;
mod conflict_fix;
mod discard;
mod fix_features;
mod ledger_events;
mod review_events;
mod gate;
mod gate_fix;
mod gate_stage;
mod handoff;
mod implement;
mod interrupted;
mod launch;
mod local_files;
mod mcp_setup;
mod merge_local;
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
mod review_escalation;
mod review_observe;
mod review_request;
mod run_list;
mod run_settings;
mod session_files;
mod run_activity;
mod run_status;
mod start_limits;
mod stop;
mod session_references;
mod session_settings;
mod setup;
mod kept_worktrees;
mod store;
mod terminal_session;
mod worktree_databases;

pub use checkpoint::{
    carry_conflict, latest_checkpoint, pause_at_boundary, record_checkpoint, record_rebase_finished_by_hand, validate_checkpoint, Checkpoint,
};
pub use ci_fix::{run_ci_fix_loop, CiFixConfig, CiFixOutcome, CiFixSteps};
pub use ci_wait::{retry_ci, wait_for_ci, wait_for_ci_logged, CiOutcome, CiWaitConfig};
pub use accept::{accept_check, accept_run, run_steps, run_worktree_tests};
pub use cleanup::{cleanup_worktree, CleanupOutcome};
pub use merge_local::{merge_into_base, merge_setting_aside, waiting_on_dirty_checkout, MergeOutcome, SetAsideOutcome};
pub use conflict_fix::{conflict_fix_task, fix_rebase_conflicts, ConflictFixOutcome};
pub use discard::{discard_preview, discard_run, discard_run_before_remove, discard_run_then, features_no_later_run_took, DeletedBranch, DiscardOutcome};
pub use fix_features::{carry_fix_features, fix_features, record_fix_features_added};
pub use ledger_events::record_ledger_features;
pub use review_events::{created_reviews, local_merges, record_review_action};
pub use gate::{choose_gate, choose_gate_scoped, choose_gate_with, Gate, GateError, GateOptions};
pub use gate_fix::{rerun_gate, run_gate_with_fixes, run_gate_with_fixes_env, run_gate_with_fixes_then, FixSessionSpec, GateFixOutcome, UNRELATED_FAILURE_FILE};
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
pub use kept_worktrees::{
    delete_kept_worktree, list_kept_worktrees, ChangeKind, ChangedFile, DeleteRefusal, DeletedWorktree, KeptWorktree,
    RunIndex,
};
pub use local_files::{carry_local_files, LocalFiles};
pub use session_files::{copy_session_files, finish_session_files, remove_session_files, SESSION_FILES_DIR};
pub use needs_input::{parse_needs_input, parse_waiting, prompt_was_submitted, question_was_answered, record_input_given, record_needs_input, record_prompt_submitted, NeedsInput};
pub use notify::{
    notification_message, review_ready_message, send_notification, send_notification_with, wants_notification,
};
pub use orq_written::record_orq_written;
pub use terminal_session::run_in_terminal;
pub use outcome::{check_implement_outcome, record_implement_unfinished, ImplementCheck};
pub use pane::{record_pane, run_pane, PaneRef};
pub use push_stage::{push_run_branch, push_run_branch_with, PushOutcome, RetryPolicy};
pub use reap::{
    plan_orphans, plan_reap, reap_orphan_ref, reap_run, reap_run_databases, Orphans, ReapItem,
    ReapVerdict,
};
pub use rebase::{private_ref_name, rebase_onto_commit, rebase_onto_target, RebaseOutcome};
pub use resume::{
    carry_rebase, check_group_resume, check_group_resume_fixing, later_run_of, newest_run_of, plan_group_resume, plan_group_resume_fixing, plan_group_resume_with, plan_resume, rebased_onto, record_group, resumable_members,
    record_inherited_implement, resume_check, run_members,
    supersede_resumed, ResumePlan,
};
pub use retention_facts::{collect_run_facts, collect_run_facts_for, parse_rfc3339};
pub use review_draft::{
    draft_review_description, issue_number, link_issue, DraftSession, DraftSource, ReviewDraft,
};
pub use review_observe::{observe_review, Observed};
pub use review_request::{
    create_review_request, create_review_request_with, recorded_review, resolve_repository_url,
    ReviewPeople, ReviewRef, ReviewRequestOutcome,
};
pub use run_list::{list_runs, list_runs_report, mark_merged_runs, work_is_on_target, RunList, RunListing};
pub use run_settings::{
    load_run_settings, model as checked_model, save_mcp_servers_setting, RunSettings, SettingsError,
};
pub use run_activity::{activity_line, newest_log_tail, run_activity, step_lines, test_progress, test_total_line, LogTail, RunActivity, TestProgress};
pub use review_escalation::{escalated_review, EscalatedFinding, EscalatedReview, UnreviewedCommit};
pub use run_status::{
    record_capability_skipped, record_mcp_setup, record_model_switched, record_run_completed, request_model_switch,
    request_pause, summarize_run, RunPhase, RunSummary,
};
pub use session_references::{
    carry_session_references, resumable_session, session_references, SessionReference,
};
pub use mcp_setup::{run_mcp_setup, McpServerVersion, McpSetupReport, McpSetupRequest, McpTimeouts};
pub use session_settings::{exclude_worktrees_dir, prepare_mcp_servers, prepare_project_mcp, refresh_run_permissions, remove_needs_input_hook, RunPermissions, setup_mcp_servers, write_needs_input_hook, write_run_permissions, write_session_settings};
pub use setup::{plan_setup, run_setup, run_setup_with_env, SetupError, SetupFailure, SetupPlan};
pub use stop::{owner_state, owner_state_in, stop_run, terminate_process, OwnerState};
pub use start_limits::{disk_refusal, free_disk, limit_refusal, FreeDisk};
pub use store::{Claim, Event, RoleProfile, RunRecord, RunStore, StoreError};
pub use worktree_databases::{
    derive_worktree_databases, derive_worktree_databases_with, drop_test_databases,
    drop_test_databases_within, WorktreeDatabases,
};
