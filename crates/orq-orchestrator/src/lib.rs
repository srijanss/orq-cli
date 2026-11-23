//! Stage logic for a pipeline run: what each stage decides and does, kept
//! apart from the CLI and the popup that drive it.

mod accept;
mod checkpoint;
mod child_groups;
mod ci_fix;
mod ci_wait;
mod cleanup;
mod conflict_fix;
mod design_capture;
mod design_infer;
mod design_loop;
mod design_package;
mod design_review;
mod discard;
mod fix_features;
mod format_revert;
mod gate;
mod gate_fix;
mod gate_stage;
mod handoff;
mod implement;
mod interrupted;
mod kept_worktrees;
mod launch;
mod ledger_events;
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
mod report_evidence;
mod resume;
mod retention_facts;
mod review_draft;
mod review_escalation;
mod review_events;
mod review_observe;
mod review_request;
mod run_activity;
mod run_list;
mod run_settings;
mod run_status;
mod session_files;
mod session_references;
mod session_settings;
mod setup;
mod start_limits;
mod stop;
mod store;
mod target_push;
mod terminal_session;
mod test_failures;
mod worktree_databases;

pub use accept::{
    accept_check, accept_run, accept_run_by, accept_run_delivering, fail_interrupted_accept,
    merge_accepted_check, merge_accepted_run, merge_after_accept_pending, run_steps,
    run_steps_with_env, run_worktree_tests, CompleteFeatures, ACCEPT_INTERRUPTED,
};
pub use checkpoint::{
    carry_conflict, latest_checkpoint, pause_at_boundary, record_checkpoint,
    record_rebase_finished_by_hand, validate_checkpoint, Checkpoint,
};
pub use child_groups::end_children_with_this_process;
pub use ci_fix::{run_ci_fix_loop, CiFixConfig, CiFixOutcome, CiFixSteps};
pub use ci_wait::{retry_ci, wait_for_ci, wait_for_ci_logged, CiOutcome, CiWaitConfig};
pub use cleanup::{cleanup_worktree, CleanupOutcome};
pub use conflict_fix::{conflict_fix_task, fix_rebase_conflicts, ConflictFixOutcome};
pub use design_capture::{
    capture_design, capture_viewports, design_routes, find_chromium_in, CaptureOutcome,
    CaptureRecord, CaptureRun, CaptureViewport, PageCapture,
};
pub use design_infer::{
    prepare_design, AgentPreparer, DesignPrepareOutcome, DesignPreparer, InferredValue, PrepareAnswer,
    PrepareConflict, PrepareRequest, PreparedDesign, PreparedFile, PreparerNeed,
};
pub use design_loop::{
    carry_design, design_code_review_task, design_fix_task, design_round_label, record_design_package, run_design_code_review,
    run_design_correction, run_design_loop, CorrectionOutcome, DesignLoopOutcome, DesignLoopSteps,
    DESIGN_CODE_REVIEW_STAGE, DESIGN_FIX_STAGE, DESIGN_PACKAGE_STAGE, DESIGN_REVIEW_STAGE,
};
pub use design_package::{
    resume_design_package, start_design_package, AssignedViewport, DesignFilesNeeded,
    DesignManifest, DesignPackageOutcome, DesignPackageRun, ManifestFile, MissingPieces,
    NeededFile, PackageLimits,
};
pub use design_review::{
    review_design, CheckStatus, DesignAgent, DesignReview, DesignReviewOutcome, DesignReviewRun,
    DesignVerdict, IssueStatus, ReviewCheck, ReviewIssue, Severity,
};
pub use discard::{
    discard_preview, discard_run, discard_run_before_remove, discard_run_then,
    features_finished_elsewhere, features_no_later_run_took, DeletedBranch, DiscardOutcome,
};
pub use fix_features::{carry_fix_features, fix_features, record_fix_features_added};
pub use format_revert::{undo_formatting_outside, FORMAT_REVERTED, FORMAT_REVERT_MESSAGE};
pub use gate::{choose_gate, choose_gate_scoped, choose_gate_with, Gate, GateError, GateOptions};
pub use gate_fix::{
    rerun_gate, run_gate_with_fixes, run_gate_with_fixes_env, run_gate_with_fixes_then,
    FixSessionSpec, GateFixOutcome, UNRELATED_FAILURE_FILE,
};
pub use gate_stage::{
    run_gate_stage, run_gate_stage_with_env, run_gate_stage_with_limit, GateOutcome,
};
pub use handoff::{
    handoff_prompt, read_handoff, write_handoff, DiffFile, GateResult, Handoff, HandoffDiff,
    PlanItem, HANDOFF_FILE,
};
pub use implement::{run_implement_stage, ImplementOutcome, ImplementSession};
pub use interrupted::{
    owner_is_alive_in, process_is_alive, process_is_alive_in, process_start_ticks_in,
    reconcile_interrupted,
};
pub use kept_worktrees::{
    delete_kept_worktree, list_kept_worktrees, ChangeKind, ChangedFile, DeleteRefusal,
    DeletedWorktree, KeptWorktree, RunIndex,
};
pub use launch::{
    record_launch, record_owner, record_resumed_launch, record_upgraded_under_runs,
    RESUME_ANSWER_LOG,
};
pub use ledger_events::record_ledger_features;
pub use local_files::{carry_local_files, LocalFiles};
pub use mcp_setup::{
    run_mcp_setup, McpServerVersion, McpSetupReport, McpSetupRequest, McpTimeouts,
};
pub use merge_local::{
    land_on_target, merge_into_base, merge_setting_aside, remove_merged_run,
    waiting_on_dirty_checkout, Landing, MergeOutcome, SetAsideOutcome, MERGE_LOCAL_RETRIES,
};
pub use needs_input::{
    parse_needs_input, parse_waiting, prompt_was_submitted, question_was_answered,
    record_input_given, record_needs_input, record_prompt_submitted, NeedsInput,
};
pub use notify::{
    notification_message, review_ready_message, send_notification, send_notification_with,
    wants_notification,
};
pub use orq_written::record_orq_written;
pub use outcome::{check_implement_outcome, record_implement_unfinished, ImplementCheck};
pub use pane::{record_pane, run_pane, PaneRef};
pub use push_stage::{push_run_branch, push_run_branch_with, PushOutcome, RetryPolicy};
pub use reap::{
    plan_orphans, plan_reap, reap_orphan_ref, reap_run, reap_run_databases, Orphans, ReapItem,
    ReapVerdict,
};
pub use rebase::{private_ref_name, rebase_onto_commit, rebase_onto_target, RebaseOutcome};
pub use report_evidence::{recorded_evidence, RecordedEvidence};
pub use resume::{
    active_runs, carry_rebase, check_answer_reaches_session, check_group_resume,
    check_group_resume_fixing, check_group_resume_reviewing_again, holds_run_slot, later_run_of,
    newest_run_of, newest_runs_of, plan_group_resume, plan_group_resume_fixing,
    plan_group_resume_reviewing_again, plan_group_resume_with, plan_resume, rebased_onto,
    record_group, record_inherited_implement, resumable_members, resume_check, resume_takes_a_slot,
    run_members, supersede_resumed, supersede_resumed_answered, supersede_resumed_with_session,
    ResumePlan,
};
pub use retention_facts::{collect_run_facts, collect_run_facts_for, parse_rfc3339};
pub use review_draft::{
    draft_review_description, issue_number, link_issue, plan_agent_args, plan_marker, plan_prompt,
    record_plan_event, run_plan, DraftSession, DraftSource, PlanIdea, PlanMarker, ReviewDraft,
    DEFAULT_DRAFT_PROMPT, PLAN_ENDING, PLAN_MARKER, PLAN_STAGE, PLAN_TIMEOUT, PLAN_TOOLS,
};
pub use review_escalation::{
    escalated_review, last_review, EscalatedFinding, EscalatedReview, UnreviewedCommit,
};
pub use review_events::{created_reviews, local_merges, record_review_action};
pub use review_observe::{observe_review, Observed};
pub use review_request::{
    create_review_request, create_review_request_with, recorded_review, resolve_repository_url,
    ReviewPeople, ReviewRef, ReviewRequestOutcome,
};
pub use run_activity::{
    activity_line, newest_log_tail, run_activity, step_lines, test_progress, test_total_line,
    LogTail, RunActivity, TestProgress,
};
pub use run_list::{
    list_runs, list_runs_report, mark_merged_runs, was_unattended, work_is_on_target, RunList,
    RunListing, UNATTENDED_EVENT,
};
pub use run_settings::{
    load_run_settings, model as checked_model, save_mcp_servers_setting, DesignMode,
    DesignSettings, RunSettings, SettingsError, Viewport,
};
pub use run_status::{
    record_capability_skipped, record_mcp_setup, record_model_switched, record_run_completed,
    request_model_switch, request_pause, summarize_live_run, summarize_run, summarize_run_with,
    AcceptFailure, RunPhase, RunSummary,
};
pub use session_files::{
    copy_session_files, finish_session_files, remove_session_files, SESSION_FILES_DIR,
};
pub use session_references::{
    carry_session_references, resumable_session, session_references, SessionReference,
};
pub use session_settings::{
    allows_command, exclude_worktrees_dir, prepare_mcp_servers, prepare_project_mcp, refresh_run_permissions,
    remove_needs_input_hook, setup_mcp_servers, write_needs_input_hook, write_run_permissions,
    write_session_settings, RunPermissions,
};
pub use setup::{plan_setup, run_setup, run_setup_with_env, SetupError, SetupFailure, SetupPlan};
pub use start_limits::{
    disk_refusal, free_disk, limit_refusal, machine_disk_refusal, machine_limit_refusal,
    worktrees_free_disk, FreeDisk,
};
pub use stop::{
    owner_state, owner_state_in, run_ending_lock, stop_run, terminate_process, OwnerState,
};
pub use store::{Claim, Event, RoleProfile, RunLimits, RunRecord, RunStore, StoreError};
pub use target_push::{
    push_target, target_push_state, PushCommit, TargetPush, TargetPushOutcome, TargetPushState,
    TARGET_FETCH_LIMIT,
};
pub use terminal_session::run_in_terminal;
pub use test_failures::{find_test_failures, FailedTest, TestFailures};
pub use worktree_databases::{
    derive_worktree_databases, derive_worktree_databases_with, drop_test_databases,
    drop_test_databases_within, WorktreeDatabases,
};
