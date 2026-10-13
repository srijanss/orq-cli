# SPEC.md — Orq (`orq`)

**Status:** Proposed | **Version:** 0.2 | **Product:** Orq | **Command:** `orq` | **CLI crate:** `orq-cli` | **Primary language:** Rust | **Initial host:** Herdr on Debian development VM

## Naming and distribution

- **Product name:** **Orq** — durable, multi-agent coding workflow orchestration.
- **Terminal command / executable:** **`orq`** (not `orq-cli`).
- **CLI crate / package:** **`orq-cli`**; the published Rust binary is named `orq`.
- **Repository / Rust workspace:** **`orq`** (recommended monorepo name).
- **Other workspace crates:** `orq-core`, `orq-features`, `orq-orchestrator`, `orq-git`, `orq-agents`, `orq-storage`, `orq-herdr`, `orq-tui`, `orq-server` (introduced only when needed).
- **Future browser UI:** **Orq Web**, sourced from `web/`.
- **Herdr integration:** **Orq for Herdr**, a popup frontend to Orq, not a separate execution engine.
- **Configuration examples:** preserve each repository's existing `.tdd-features.json`; future optional Orq settings use `.orq.toml` only when justified.
- **Availability caveat:** verify crates.io, repository naming and command collisions before publishing; this spec does not assert that the names are available.

## 1. Purpose

Build a project-aware, mouse-first TDD feature orchestrator that starts inside a Herdr popup and evolves into a durable, multi-agent pipeline service with a browser dashboard. The initial source of features is **`<repo>/.tdd-features.json`**. The system must orchestrate feature implementation, review, push, merge request/pull request creation, and optional merge against an explicitly selected target branch (default `main`).

The current working Bash orchestrator is a **behavioural reference and migration fallback**, not the permanent engine. Rust will own orchestration rules, lifecycle, persistence, Git coordination and agent integrations. Rust can invoke external executables (`git`, `claude`, `codex`, `pi`, provider CLIs) with validated argument vectors; it should not run unconstrained shell commands supplied by feature files or agents.

## 2. Principles and invariants

1. **Project scope is authoritative.** Opening the popup from a pane displays only that pane's repository's `.tdd-features.json`. A separate All Projects view applies to pipeline history, not to choosing a feature to start.
2. **Explicit base.** Each run records its repository, remote, target branch (default `main`), resolved remote-tracking commit, feature ID and isolated worktree. The base is never inferred from the currently checked-out feature branch.
3. **One runtime, multiple clients.** CLI, Herdr popup and future browser use the same Rust orchestration core and public command/event contracts.
4. **Durable workflow, not portable model memory.** Persist stage state, worktree changes, native agent session identifiers when supported, checkpoints, handoff summaries and command results. Do not claim Claude's internal conversation can be loaded by Codex.
5. **Human-controlled risk.** Merges, destructive cleanup, abandoning changes and remote mutations require explicit policy and confirmations. Starting a run is an explicit action.
6. **Safe concurrency.** Prevent accidental duplicate active runs for a feature and guard shared repositories, branch names and Git operations. Multiple independent features may run in separate worktrees.
7. **Graceful control.** Pause and switching agents happen at safe checkpoints first; mid-task interruption is a later feature.
8. **Observable and testable.** Every meaningful transition produces an append-only event with timestamps, outcome and error reason; deterministic domain logic is unit-testable without Herdr, network or LLMs.
9. **Host separation.** An agent run continues even when its Herdr popup closes; browser clients never directly execute local shell commands.
10. **Local-first security.** Unix socket / restricted local API first; Tailscale-accessible read-only monitoring later, then separately authorised remote controls.

## 3. Scope

### In scope

- Discover the repository from Herdr's originating pane working directory, including nested paths and linked Git worktrees.
- Read, validate and update a project's existing `.tdd-features.json` without overwriting unknown fields or unnecessarily reformatting the entire file.
- List features, change eligible `draft` entries to `pending`, choose a target branch and start a run.
- Spawn independent tracked execution, record the Herdr workspace/tab/pane identifiers, navigate back to the pane, and show clickable MR/PR URLs.
- Orchestrate implementation → review → test/verification → push → MR/PR → optional merge, with bounded retries and approval gates.
- Graceful pause/resume, checkpoint-based agent switching, recovery after crash, global run history.
- Later: Rust web API and React monitoring/control dashboard.

### Out of scope for initial releases

- A new general-purpose LLM agent framework, multi-machine distributed scheduling, automatic prompt engineering, rebuilding Claude/Codex/Pi, auto-merging without configured approvals, remote terminal execution in the browser, native modifications to Herdr's internal Settings UI, automatic repair of arbitrary partial code changes.

## 4. Terminology and ownership

| Object | Authority | Description |
|---|---|---|
| Project | Git repository identity | Repository root, canonical Git common directory, remote/provider, config path |
| Feature | Project `.tdd-features.json` | Feature definition and its TDD lifecycle status |
| Pipeline run | Orchestrator database | Immutable feature/repo/remote/base inputs, progress, assigned agents |
| Stage attempt | Orchestrator database | One invocation, agent session, timestamps, result, retry count |
| Checkpoint | Orchestrator | Git state, test results, handoff artifact, pending next action |
| Herdr location | Herdr | Session/workspace/tab/pane reference; an optional navigational link, not execution state |
| MR/PR | Git provider | Remote review object's URL, ID, head/base, status and merge outcome |

Feature status and run status are **separate state machines**. A feature may remain `pending` while its first run is queued; a failed run does not silently mark a feature `completed`. Retain the existing feature schema rather than assuming new enum values are accepted by the user's TDD MCP.

## 5. Architecture

```text
Herdr focused pane (cwd)
       │
       ▼
Herdr popup: Rust + Ratatui + Crossterm ─────────┐
       │                                          │
       │ local commands                            │ future
       ▼                                          ▼
Rust orchestration core  ◄───────────────── Rust Axum API
  ├── project/feature repository                  ▲
  ├── scheduler + state machine                    │ SSE / authenticated HTTP
  ├── run supervisor + crash recovery              ▼
  ├── Git / worktree / provider adapters        React dashboard
  ├── agent adapters (Claude / Codex / Pi)
  ├── Herdr CLI/socket adapter
  └── SQLite repository + event log
       │
       ├── Git + GitLab / GitHub external CLIs/APIs
       ├── agent processes in isolated worktrees
       └── tracked panes / logs / checkpoints
```

**Process boundary:** A popup must not own the lifetime of a run. MVP 1 can use a standalone Rust CLI/background supervisor; before advertising crash recovery or web control, move execution ownership into a persistent local supervisor/service. The Herdr popup is a client. The core is a Rust library shared by CLI and supervisor, not embedded as independent competing runtimes in every popup.

### Suggested Rust workspace

```text
orq/
├── Cargo.toml
├── crates/
│   ├── orq-core/           # IDs, lifecycle, transitions, policies
│   ├── orq-features/       # .tdd-features.json adapter + atomic writes
│   ├── orq-orchestrator/   # scheduler, stages, checkpoints, approvals
│   ├── orq-git/            # Git worktrees + GitLab/GitHub adapters
│   ├── orq-agents/         # Claude/Codex/Pi process adapters, handoffs
│   ├── orq-storage/        # SQLite, migrations, event log
│   ├── orq-herdr/          # Herdr CLI/socket requests and location mapping
│   ├── orq-cli/            # `orq` executable and legacy parity commands
│   ├── orq-tui/            # Ratatui popup
│   └── orq-server/         # Axum API, added later
├── web/                    # React, added later
├── tests/                  # integration / fake-agent scenarios
├── fixtures/               # sample feature files and fake git repos
├── orq-herdr-plugin.toml
└── docs/
    ├── architecture.md
    ├── events.md
    ├── state-machines.md
    └── migration-from-bash.md
```

Use `tokio` for async process management and IPC, `serde` for data types, `sqlx` or `rusqlite` for SQLite (pick one), `ratatui` + `crossterm` for the UI, and `axum` only when the API MVP arrives. Avoid a generic plugin engine until multiple actual agent adapters need it.

## 6. Project discovery and feature definitions

**Resolution algorithm:**
1. Capture `HERDR_ACTIVE_PANE_CWD` when launching popup; pass explicit repo root in CLI/API clients.
2. Resolve `git rev-parse --show-toplevel`, `git rev-parse --path-format=absolute --git-common-dir`, and configured remote identities. Determine linked worktree versus canonical repository.
3. Locate `.tdd-features.json` according to the TDD MCP's existing rules. **Do not invent or silently create a feature file** in MVP 1.
4. Validate the document shape and feature IDs against an adapter/schema configured for the real existing file; display actionable parse/missing-file errors.
5. Display feature statuses and allow only valid TDD MCP-defined transitions. Use file lock + read/compare/write + atomic rename to prevent lost updates; preserve unknown fields and formatting when feasible. If concurrent external updates are detected, require reload/reconciliation.
6. If the originating pane is not in a repo, show an empty state and a way to select a known local project; never accidentally load the previous project's features.

**Schema compatibility gate:** Before coding mutation, examine actual `.tdd-features.json` and the existing TDD MCP semantics. Feature identifiers, statuses, dependencies and file structure are intentionally **not specified as assumptions** here.

## 7. Branch and Git behaviour

- Default `target_branch = main`, unless a user-selected override or explicit per-project configuration is present.
- Show both target remote and branch. Verify remote branch exists, fetch it, and record exact base commit SHA.
- Start a uniquely named worktree/feature branch from the fetched base commit, **not** the current checkout's HEAD. Never mutate the source worktree or discard dirty changes.
- Only the designated run owns its worktree; protect against duplicate worktree paths and branch-name collisions.
- All stage invocations run with the run worktree as cwd. Each adapter gets only required environment and explicit arguments.
- Push to selected remote and create a GitLab MR or GitHub PR against the stored target branch. Resolve provider via validated remote configuration; prefer structured API/CLI responses to parsing human output.
- Persist review URL and provider object ID. Before retrying creation, query existing open/merged MR/PR by repo/source/target to avoid duplicates.
- Merge must validate current approval, CI status, head SHA and target rules against repository policy. No blind retry of a potentially successful merge.
- Store source and target branch SHAs at important transitions; if the target branch moved, follow an explicit rebase/update policy rather than silently changing the original base.

## 8. Workflow and state machines

### Feature lifecycle

Feature transitions are delegated to the existing TDD MCP-compatible `.tdd-features.json` schema. Minimum required interface: list, validate, and `draft → pending` if supported. Feature completion is updated only after the configured definition of done, and never inferred from a successful push alone.

### Run lifecycle

`queued → preparing → running → pausing → paused → running → completed`

Additional terminal states: `failed`, `cancelled`; nonterminal `blocked` / `awaiting_approval`. Explicit retry creates a new attempt (and potentially a new run), never rewrites previous events. Invalid state transitions return typed errors.

### Stages

Default: `prepare → implement → review → verify → push → create_review → await_approval → merge → final_verify → done`. Stage enabling/policy configurable; a repo using manual merge can finish at `review_ready` rather than `merged`.

- Capture `stage.started`, `stage.completed`, `stage.failed`, `run.pause_requested`, `run.paused`, `run.resumed`, `agent.switch_requested`, `agent.switched`, `review.created`, `merge.completed`, etc.
- Every command has correlation IDs, stage-attempt IDs, recorded stdout/stderr log locations and exit codes.
- Review feedback loops have configurable bounded retries and a stop condition.

## 9. Pause, resume and agent switching

**MVP pause semantics:** cooperative *pause after current stage*. A request changes status to `pausing`, completes/reconciles the current operation, persists checkpoint, then enters `paused`. Pausing one run must not pause unrelated runs.

**Resume:** validate checkpoint, actual worktree and branch state, expected remote objects, stage result and agent readiness before scheduling the next unfinished stage. If uncertainty remains, enter `blocked` with a recovery action rather than automatically redoing remote effects.

**Switch agent (first implementation):** queue handoff at a checkpoint; keep the same worktree, immutable run inputs and stage history; create a structured summary of goal, changes, diff, tests, remaining tasks and review findings; start the replacement agent in the same worktree; record both adapter and native session IDs. The replacement agent verifies repository state. Claude/Codex/Pi session files are not assumed interchangeable.

**Advanced interruption (later):** request cooperative agent cancellation, wait for children to stop, snapshot dirty Git state and tests, and mark checkpoint validity. `SIGSTOP`/`SIGCONT` is not the standard persistence mechanism. Never kill a Git push/merge blindly without reconciliation.

## 10. Durable execution contract

SQLite is authoritative for run state/events; Git and remote providers remain the authorities for actual source and MR/PR state. On restart:

1. Acquire single-supervisor ownership and rehydrate nonterminal runs.
2. Reconcile processes, Herdr panes, worktrees, Git branches, and remote review objects.
3. Check stage attempts with ambiguous outcomes; reconcile before retrying.
4. Mark missing processes/panes without pretending the pipeline disappeared; offer resume or recovery.
5. Use idempotency keys and unique constraints for run launches, event IDs and side-effect attempts.

Exactly-once external side effects cannot be assumed; design for at-least-once invocation plus idempotent reconciliation. Use migrations, WAL, retention and safe backup practices. Keep secrets out of events, logs and handoff files.

## 11. UI / UX — mouse-first Herdr popup

The popup is launched from the originating project's active pane. It should resemble Herdr's Settings-style interaction but is a **plugin-hosted terminal UI, not native Settings UI**.

**Header:** project name, canonical repo path (truncated), remote/provider, target branch selector, active-run count.

**Features view:** searchable feature list, current TDD status, detail/dependencies, Draft → Pending action where legal, Start button (requires eligible feature). Clicking a feature must never silently start a run.

**Pipelines view:** project's active and recent runs by default; All Projects toggle for monitoring; stage/status/agent/badges; click row for detail; click 'Open pane' to dismiss popup and focus the stored Herdr location; click MR/PR URL to open/copy according to host configuration.

**Details view:** stage history, current agent, logs, tests, worktree path, source/target, review URL, Pause/Resume, Switch agent, retry/approval actions when allowed.

**Interaction:** left-click primary, mouse scrolling, visual hover/selected states where terminal capabilities allow; keyboard fallback (Tab/arrows, Enter, Esc, / search); errors and progress visible without relying on colour. Right-click is optional and must be tested against Herdr mouse forwarding. The TUI should never block while running Git/agent subprocesses.

**Herdr integration verification gate:** Confirm config/custom popup and `HERDR_ACTIVE_PANE_CWD`, validate mouse forwarding, popup close/focus ordering, stable pane IDs and recreation behaviour against the installed Herdr version. Popup has no normal pane ID; the origin pane is carried as launch context.

## 12. Public contracts (design targets, not finished API)

CLI examples (proposed `orq` interface; command flags finalised during MVP 1):

```text
orq project inspect --cwd <path>
orq features --repo <path>
orq feature set-status --repo <path> --id <id> --status pending
orq run --repo <path> --feature <id> --remote origin --target main
orq ps [--repo <path>]
orq pause <run-id>
orq resume <run-id>
orq agent switch <run-id> --agent codex
orq ui
```

Internal typed commands: `ResolveProject`, `ListFeatures`, `TransitionFeature`, `StartRun`, `PauseRun`, `ResumeRun`, `SwitchAgent`, `ApproveMerge`, `GetRuns`, `GetEvents`, `FocusRunPane`.

Representative event envelope:

```json
{
  "schema_version": 1,
  "event_id": "evt-...",
  "run_id": "run-...",
  "project_id": "repo-...",
  "feature_id": "COM-02",
  "type": "stage.completed",
  "stage": "review",
  "attempt": 1,
  "occurred_at": "2026-10-14T00:00:00Z",
  "payload": { "agent": "codex", "result": "passed" }
}
```

Run record must include: run ID, project/repo identity, feature ID, remote/provider, target branch, immutable base commit, source branch/worktree, state, current stage, attempts, agent assignment/session references, review URL/ID, current Herdr navigation identifiers, timestamps and last error.

## 13. MVP roadmap and acceptance criteria

### MVP 0 — Baseline + feasibility spike

**Goal:** preserve working behaviour before replacing Bash; verify Herdr plugin boundaries.

- Inventory existing Bash stages, flags, exit codes, TDD MCP interactions, `.tdd-features.json` schema, merge policy and existing resume behaviour.
- Add fixture repos and fake Git/agent executables; record comparison cases for GitLab and GitHub.
- Build minimal Rust binary opening as Herdr popup; read active-pane CWD, identify repo, render project name; verify mouse clicks and Esc.
- **Done when:** popup resolves two different repositories correctly; tests demonstrate fallback on non-repo paths; no real repo mutated.

### MVP 1 — Project feature browser

**Goal:** view and edit existing features safely.

- Parse exact real `.tdd-features.json` format and show features for current repository only.
- Search/select, inspect feature details, perform allowed `draft → pending` transition with atomic write and conflict detection.
- **Done when:** project A cannot display or modify project B's file; malformed JSON is read-only with error; unknown fields survive edits; concurrent modification is detected.

### MVP 2 — Rust headless orchestration (single run)

**Goal:** replace Bash for one feature end-to-end before adding more UI.

- Implement domain state machine and Rust CLI; fetch target, create isolated worktree from selected remote branch, invoke agent and review stages, verify, push and open MR/PR.
- Use fake agents/providers for automated tests; first real end-to-end runs against a disposable test repo.
- Preserve logs, stage exit codes and success/failure classification.
- **Done when:** both GitLab and GitHub flows can complete with correct target (`main` and a non-main branch), Bash parity scenarios pass, no implicit merge occurs without policy.

### MVP 3 — Popup launch + pipeline monitor

**Goal:** use Herdr rather than manually invoking orchestration.

- Start the Rust runner from the selected feature; ensure it survives closing the popup.
- Show run list, stage, agent, status; create/track a Herdr execution pane or tab; click to navigate; open/copy MR/PR link.
- **Done when:** two projects independently start and monitor runs, popup close does not cancel jobs, missing pane produces safe recovery UI.

### MVP 4 — Persistent supervisor + recovery

**Goal:** survive Herdr disconnects, restarts and controller crashes.

- Separate long-lived Rust supervisor; add SQLite schema/events, process reconciliation, restart discovery, ambiguous Git/provider side-effect handling.
- Implement bounded retry/backoff and dedupe for launches and MR creation.
- **Done when:** kill/restart tests across each stage recover safely, no duplicate MR/PR is created, completed stages are not blindly replayed, history remains queryable.

### MVP 5 — Graceful pause/resume

**Goal:** pause one run at stage boundaries and continue safely.

- Add pause requested/pausing/paused states, checkpoint validity, resume confirmation and UI controls.
- **Done when:** pausing run A leaves run B unaffected; closing Herdr does not remove checkpoint; resume does not rerun completed push or MR creation.

### MVP 6 — Agent handoff and switching

**Goal:** switch future work between Claude, Codex and optionally Pi.

- Agent adapter trait, capability matrix (start, prompt, resume native session, interrupt, status), structured context handoff and per-stage agent selection.
- Change agent only at safe checkpoint initially; preserve prior agent session references.
- **Done when:** Claude → Codex handoff continues in the same worktree using saved diff/tests/feature plan; reverse switch works; no assumption of shared conversation format.

### MVP 7 — Local Rust web service + streaming

**Goal:** API-backed views without redesigning core.

- Add Axum API for read-only project/run/event queries, logs and SSE updates; protect local socket/API; reconnect with event cursor.
- **Done when:** browser can follow a pipeline and reconnect after client disconnect without dropping persisted state.

### MVP 8 — React dashboard over Tailscale

**Goal:** mobile/desktop OpenClaw-inspired control centre.

- Project overview, filters, pipeline timeline, logs, feature detail and MR/PR deep links; responsive phone layout.
- Restrict to authenticated, initially read-only access; avoid making Rust API publicly reachable.
- **Done when:** Mac/iPhone over Tailscale can view active and completed runs with correct project scoping and access controls.

### MVP 9 — Authenticated remote controls + approvals

**Goal:** safe remote pause/resume, switch agent, approve merge.

- Per-action authorisation, audit log, replay protection, CSRF/origin protections where relevant, confirmation for consequential actions and explicit merge approval policy.
- **Done when:** unauthorised client cannot start or merge; approval and rejection are auditable; stale approvals/SHAs are rejected.

### MVP 10 — Advanced interruption + scheduling (optional)

**Goal:** finer-grained interaction only when operational evidence justifies it.

- Cooperative mid-stage interruption, robust dirty-worktree handoffs, bounded per-project concurrency, queue pending features, dependency constraints, notifications.
- **Done when:** interruption/recovery fault-injection tests reliably preserve code changes without pretending partial tasks completed.

## 14. Testing strategy (TDD-first)

- **Domain tests:** valid/invalid transitions; pause/retry/approval invariants; duplicate-run lock; stage-specific failure classification.
- **File tests:** missing/malformed `.tdd-features.json`, nested CWD, worktree resolution, lock contention, concurrent edit, field preservation, permissions, symlinks.
- **Git integration:** dirty base checkout, nonexistent target branch, branch collision, stale remote, failed fetch/push, detached HEAD, already-created MR, divergent targets.
- **Adapter contract tests:** fake agent exits, hangs, loses session, emits malformed output, requires approval, switches with dirty working tree.
- **Recovery tests:** crash before/after remote create, after push, during process start, after stage success but before persisted event, Herdr pane closed, supervisor restarted.
- **Security tests:** shell injection paths, malicious feature fields, Git URL/provider mismatches, secrets in logs, unauthorized remote control, double-click actions.
- **UI tests:** mouse select/scroll, keyboard fallback, stale data, narrow terminal, modal dismiss, focus failures, project switch.
- **E2E:** temp repositories and fake GitLab/GitHub endpoints first; opt-in real provider sandbox repositories.

Each MVP requires unit tests + one executable end-to-end demonstration + concise release notes; do not advance to the next MVP with a failing recovery or safety invariant.

## 15. Explicit safety policies

- No agent-generated arbitrary shell command is run with orchestrator authority.
- Constrain worktrees and paths to validated repository roots; defend against symlink/path traversal.
- Never accept credentials, SSH keys or tokens from `.tdd-features.json`.
- Limit log retention and redact credentials/tokens; separate raw terminal output from structured events.
- Prefer provider tokens scoped to specific repos with minimal permissions; remote merge is a privileged action.
- Lock per feature/run; require optimistic version checks for feature mutations and command idempotency for double-clicks.
- Keep local permissions restrictive on SQLite files, session IDs and handoff artifacts.
- For remote use, Tailscale is transport/private routing rather than a substitute for API authentication and authorisation.

## 16. Risks and rabbit-hole alerts

| Risk | Mitigation | Defer until |
|---|---|---|
| Building a general durable agent platform | Limit to stage-based coding workflow and explicit checkpoints | Only if real use cases exceed stages |
| Rewriting working Bash scripts wholesale | Rust adapter parity tests; migrate one stage at a time | MVP 2 |
| Herdr private API coupling | Public CLI/socket only; popup integration spike | MVP 0 |
| Cross-agent conversation portability | Handoff summaries + Git state, not shared session blobs | MVP 6 |
| Mid-token pause or process freezing | Graceful stage-boundary pause first | MVP 10 |
| Browser remote shell/security exposure | Read-only API first; authenticated action API later | MVP 9 |
| Multi-host distributed scheduler | Single Debian VM + isolated worktrees initially | Beyond MVP 10 |
| Full-featured native Git implementation | Invoke `git` with safe argv and verify results | Not planned |

## 17. First implementation sequence

1. Snapshot current Bash orchestrator inputs/outputs and representative full execution logs (with secrets removed).
2. Inspect `.tdd-features.json` + the user's TDD MCP transitions; write fixtures without changing its schema.
3. Verify Herdr popup, origin CWD, mouse and pane navigation on installed Debian/Herdr version.
4. Create Rust workspace with domain models and feature repository; write unit tests first.
5. Complete MVP 1; only then move the existing orchestration stages from Bash to Rust.

**First meaningful vertical slice:** From a `commerce-api` Herdr pane → open popup → see only `commerce-api/.tdd-features.json` → select eligible feature → change `draft` to `pending` safely → close/reopen and observe preserved state. No agent execution required for this slice.

## 18. Definition of project success

A developer can open a single mouse-first Herdr popup from any repository, choose a feature from that repository's `.tdd-features.json`, select a target branch (default `main`), start a tracked Rust orchestration run in an isolated worktree, watch implementation/review/test/push/MR or PR/merge progress, jump into the correct agent pane, gracefully pause/resume or switch agents at checkpoints, and inspect durable history through the future web dashboard without tying execution to a particular terminal or agent.

## 19. External integration references

- Herdr configuration/custom popup: https://herdr.dev/docs/configuration/
- Herdr plugin/popup model: https://github.com/herdrdev/herdr/blob/master/docs/preview/website/src/content/docs/plugins.mdx
- Herdr socket/CLI capabilities: https://herdr.dev/docs/socket-api/

**Note:** Names of proposed `orq` CLI commands, schema types and event contracts are design choices, not claims that Herdr already implements them.
