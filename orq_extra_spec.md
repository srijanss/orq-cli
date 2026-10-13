# orq_extra_spec.md — Orq Agent Availability Integration

**Status:** Proposed addendum | **Version:** 0.2 | **Applies to:** `SPEC.md` (Orq v0.3) | **Stack:** Rust

> This document adds **optional usage-aware agent selection and handoff hints** to Orq. The standalone Herdr Agents monitor is specified separately in `agents_plugin_spec.md`. Orq's core feature → implement → review → test → push → MR/PR → merge workflow and existing MVPs remain authoritative. Each project owns its **`<repo>/.tdd-features.json`**.

## 1. Objective

Let Orq read trustworthy, timestamped subscription usage snapshots from an independent usage provider, show quota/reset hints when choosing the implementer and reviewer model profiles (`SPEC.md` §5), and provide informed human-approved model switches at durable checkpoints. Usage is reported per provider subscription (Claude and Codex, see §4), so a hint attaches to a profile through its `provider` field, not its harness; two profiles on the same provider share one quota. The usage system must **never** be a required dependency for starting or continuing a pipeline.

## 2. Responsibilities

| Component | Responsibility |
|---|---|
| Orq core | Project detection, target branch (default `main`), worktrees, pipeline lifecycle, retries and checkpoints |
| Orq harness adapters | Start/resume models through `claude` or `pi`, track native session IDs and handoff artifacts |
| Orq availability adapter | Consume normalized read-only usage snapshots with schema validation and timeouts |
| Orq policy | Offer advisory warnings/suggestions; manual selection remains default |
| Agents plugin | Independently collect provider usage and offer a sanitized versioned interface |
| Orq Herdr / future web UI | Display hints and provenance, never act as state machine or secret store |

**Invariant:** An unavailable, stale, or incompatible collector cannot block Orq execution or change an existing run. Do not merge the Agents plugin UI into Orq.

## 3. User stories

1. Open Orq from a project's active Herdr pane and see only that repository's `.tdd-features.json` entries.
2. Choose an implementer model and a reviewer model, target branch (default `main`), and initiate a run in an isolated worktree.
3. See fresh quota/reset status next to eligible models, or `Usage unavailable` when unknown.
4. Receive an optional warning when a selected provider is near its reported limit, without preventing manual selection.
5. Pause a run, checkpoint safely, and switch a role's model (including to a model on another harness) using explicit worktree and task context, not shared conversation memory.
6. Resume without changing the run's immutable feature ID, base commit, target branch or Git remote.
7. See advisory usage in the future Orq Web dashboard with authentication and source/freshness labels.

## 4. External availability contract

Consume the **versioned normalized snapshot specified in `agents_plugin_spec.md` §4**, rather than defining a duplicate schema. At minimum it carries `schema_version`, provider, opaque profile ID, collection timestamp, source, health, windows, and optional bonus resets. All provider fields are nullable and unknown is not zero.

**Initial transport:** a short-lived local read-only command, conceptually `herdr-agents usage --json`, with a bounded timeout; its name is not guaranteed by Herdr. An authenticated Unix socket can be considered later. Never copy OAuth tokens, cookies or session transcripts into Orq.

### Quota sources

There are two subscription quota sources, and they are independent of which harness runs the model:

| Profile `provider` | Quota source | Collector prerequisite |
|---|---|---|
| `anthropic` | Claude subscription | `claude` installed and signed in |
| `openai` | Codex (ChatGPT) subscription | `codex` installed and signed in, so its RPC interface can report rate limits |

- **Pi is not a quota source.** Pi is a harness only; a `pi` profile's hint comes from its `provider`. An OpenAI model run through `pi` shows the Codex subscription's usage.
- **Codex is a collector dependency, not an Orq harness.** The Agents plugin needs the `codex` CLI installed and authenticated to query usage. Orq never launches work through `codex`, and `SPEC.md` keeps only the `claude` and `pi` adapters.
- **Missing or signed-out `codex`** yields `Usage unavailable` for `openai` profiles. It does not hide those profiles or block a run.
- **Same-account assumption:** the Codex figure describes a `pi` run only when `pi` is signed in with the same ChatGPT subscription. If `pi` uses an API key or another account, the collector must report unknown for that profile rather than the Codex number.
- A profile with any other `provider`, or none, has no quota source and shows `Quota unknown`.

- Cache provenance and freshness separately from run state; suggested stale threshold: 5 minutes (configurable).
- `provider_reported` and `local_estimate` are not interchangeable; local tokens do not establish remaining subscription capacity.
- Unsupported contract versions or collection errors return an unknown availability indication; never a default green/available status.
- Persist only the selected agent and **why** it was selected plus minimal telemetry provenance for audits.
- Do not make subscription usage a required field on core run models.

## 5. Launch UI

### Orq feature launch integration

In Orq's **project-aware** popup, continue to resolve the active pane's repository and read **that project's** `.tdd-features.json`. Keep target branch default `main` (or configured override) and record selected remote/base commit per run. Annotate the *Implementer* and *Reviewer* model selectors (`SPEC.md` §11) with status:

```text
Project: commerce-api    Target branch: main
Feature: COM-02 — Partial refund support

Implementer
  (●) opus          claude   Claude sub · weekly 91% used · reset in 9h ⚠
  ( ) sonnet        claude   Claude sub · weekly 91% used · reset in 9h ⚠

Reviewer
  (●) gpt-6-astra   pi       Codex sub · weekly 34% used · reset in 4d
  ( ) opus          claude   Claude sub · weekly 91% used · reset in 9h ⚠

[Start orchestration]
```

Which models appear is decided by `SPEC.md` §5 (role restrictions and `reviewers.allowedModels`), never by usage data.

The Orq popup can show a warning or recommendation but must **not block starting** solely because quota is high or telemetry is unavailable. Do not confuse the selected provider's quota with the running agent's token/context window.

## 6. Local consumption behaviour

**MVP integration transport:** a read-only local command, e.g. `herdr-agents usage --json`, or a restricted Unix-socket API. This is a conceptual command name, not an asserted existing command. Pin a schema version and prefer one integration transport for the initial implementation.

- Orq obtains a snapshot asynchronously with a short timeout; if unavailable, show `Usage unavailable` and continue.
- Cache snapshots separately from pipeline state. A cached snapshot must be timestamped and may never be treated as live usage.
- Suggested default freshness threshold: **5 minutes** (configurable); show **Stale** past the threshold. Avoid making scheduling promises from these numbers.
- The data may annotate models in `orq models`, `orq run`, `orq model switch`, Herdr popup, and future Orq Web.
- Do not make provider usage a required field in Orq's core `Run` entity.
- Record **why** a human/optional policy selected an agent (`user_selected`, `suggested_alternative`, `fallback_unavailable`) in an append-only event; record only minimal telemetry provenance, not authentication details.

### Suggested Orq commands (design targets)

```bash
orq models                # configured model profiles, with optional usage hints
orq run F-001 --implementer opus --reviewer gpt-6-astra --target main
orq pause run-123         # graceful checkpoint request
orq model switch run-123 --role reviewer --model opus
orq resume run-123
```

## 7. Selection policy

### Level A — Default for initial release

- User explicitly chooses implementation/review agents, or existing Orq configuration supplies defaults.
- Usage monitoring is display-only; no auto-selection.
- High usage (> configurable warning threshold, suggested **85%**) raises an informational warning only. This threshold is **not** a guaranteed rate-limit predictor.

### Level B — Optional suggestions

- Consider stage compatibility, agent installed/authenticated status, user preference, **fresh provider-reported** usage, and reset time.
- Offer a preferred alternative; user explicitly confirms any switch.
- If data conflicts or is stale, treat it as unknown, not as available.
- Keep implementation/review independence: selecting Claude for implementation does not silently force Claude for review.

### Level C — Future opt-in automatic scheduling

- Only at stage boundaries, after durable checkpoints and explicit per-project policy.
- Respect eligibility and user-defined provider restrictions. Do not interpret a reset timestamp as a guarantee an agent can accept a job.
- Emit a decision event and keep manual override. Fail safe if the chosen agent cannot start.
- Never cancel, kill, or migrate an active agent merely because estimated quota crossed a threshold.

## 8. Pause, resume and switching

A quota warning is not itself a checkpoint.

```text
User requests switch (role + new model profile)
  → pipeline status: pausing
  → wait for safe stage boundary / agent cooperative stop
  → persist worktree state, stage outcome, tests, decisions
  → write handoff summary and verify files/diff
  → launch the new model in the SAME isolated feature worktree
  → resume from explicit handoff, not the previous conversation memory
     (same harness with session resume may keep its native session)
  → record new session and switch event
```

**First milestone:** support switching **between stages**. Mid-stage switches require an independently testable interruption/reconciliation design; never simply stop a tool process and declare the stage complete. Native session IDs remain provider-specific; don't assert cross-provider conversation portability.

### Recovery rules

- A restart must not duplicate pushes, MRs/PRs, or merges. Reconcile remote state before retrying externally visible actions.
- Run state transitions live in Orq's durable controller, never in the popup or usage plugin.
- Agent usage collection cannot pause, resume, or merge a pipeline.
- Existing Orq invariants — feature scope, worktree isolation, target branch, explicit merge approval — remain unchanged.

## 9. Incremental Orq MVPs

These are **additions**, not replacements for Orq's core `SPEC.md` MVP 0–10.

| Extra MVP | Scope | Acceptance criteria |
|---|---|---|
| **O0 — Contract spike** | Agree schema, transport, version negotiation and error behaviour with Agents plugin | Mock provider works; no secrets cross boundary |
| **O1 — Optional reader** | Fetch snapshot asynchronously with timeouts and cache | Runs work normally when collector missing, stale, unsupported or offline |
| **O2 — Launch hints** | Display usage/window reset hints next to agent selection in project-scoped popup | Manual choice authoritative; target branch unchanged |
| **O3 — Switch hints** | Suggest alternatives during *graceful* pause/checkpoint/handoff | Same feature, worktree and base commit preserved |
| **O4 — Web projection** | Authenticated read-only display in future Orq web UI | Freshness and source labelled; no identifiers or secrets leaked |
| **O5 — Optional policy** | Consent-based stage-boundary agent suggestions / scheduling | Deterministic, auditable, user-configured; no mid-stage kill on quota estimates |

**Dependencies:** O1 requires a stable Agents interface (or mock equivalent). O2 needs Orq project-aware launch support; O3 requires Orq durable checkpoints/model switching; O4 requires Orq API/web MVP. O5 remains optional.

## 10. TDD and failure injection

1. Usage integration missing/timeout/unsupported schema → feature start and manual switching still work.
2. Collector says 90% used but timestamp is stale → clearly labelled stale, no auto-switch.
3. Bonus reset count absent differs from zero; normal window reset is not a bonus allowance.
4. Changes to target branch in popup never mutate already-started run target or base commit.
5. Switching providers preserves worktree changes and writes a testable handoff; never assumes native session portability.
6. Restart after push or MR/PR creation reconciles remote state instead of duplicating operations.
7. Herdr popup closes, restarts or loses connection without affecting pipeline execution.
8. Concurrent reads and UI updates do not change state transitions or stall the controller.
9. Web responses redact tokens, headers, session transcripts and local credential paths.
10. High quota usage triggers advisory signals only until explicit opt-in policies are enabled.

## 11. Rabbit-hole alerts

- **Automatic cost optimisation:** Percent used isn't enough to predict whether a feature can complete; present hints first.
- **Portable agent sessions:** Handoff is structured files, state and worktree, not private conversation transplant.
- **Universal provider integration:** Do not implement collectors inside Orq; use the independent Agents boundary.
- **Mid-stage interruption:** Focus on stage boundaries, checkpoints and recovery first.
- **Building Orq Web too early:** Finish project-scoped launch and durable execution before remote control.

## 12. Definition of done

Orq can optionally read and present timestamped, provider-grounded availability hints for agent selection and graceful switching, without depending on the Agents plugin to operate. Pipeline safety, project scoping, target branches, isolation, and approval policies remain unchanged.
