# Orq — Herdr Integration Specification

**Version:** 0.1  
**Status:** Proposed, integration spike required  
**Companion:** `SPEC.md` (Orq main specification), `orq_extra_spec.md`  
**Environment:** Herdr in Debian VM; Orq executable `orq`  
**Goal:** Open `orq ui` as a mouse-first, project-aware session-modal Herdr popup and navigate from it to independent pipeline execution panes.

## 1. Scope and ownership

- **Orq Core** owns feature validation, orchestration, worktrees, state transitions and persistence.
- **`orq ui`** owns the Ratatui presentation and mouse/keyboard event handling. It is a client, never the lifetime owner of pipelines.
- **Herdr** owns popup hosting, active-pane context, panes/tabs/workspaces and navigation.
- **`orq-herdr` adapter** translates Herdr context and supported CLI/socket operations; Orq remains usable without Herdr.
- The project's canonical feature file is **`<git-repo-root>/.tdd-features.json`**, not `tdd-features.json`.
- The initial preferred integration is a **Herdr custom popup command**. A distributable manifest-based plugin is optional once the first path passes acceptance tests.

## 2. User stories

1. From any pane inside `commerce-api`, invoke the Orq shortcut; see **only that repository's** features, including when CWD is a nested subdirectory.
2. Opening from `fraud-engine` shows `fraud-engine` features instead; no stale project context is reused.
3. Select a feature, optionally promote `draft` to `pending`, choose an agent and the target branch (default `main`), and start a run.
4. Starting a run creates or associates an **independent Herdr execution pane**; closing the popup does not stop the run.
5. The popup's Pipelines tab shows current-project runs by default and optionally all-project runs.
6. Click a pipeline to close the popup and focus its existing execution pane. If the pane is gone, offer logs/recovery without falsely claiming it is focused.
7. Open or copy the exact GitLab MR/GitHub PR URL stored against the run. The remote provider is inferred from the selected repository's configured remote, not from the UI's host.
8. Users without Herdr can still run `orq ui` with an explicit `--repo` or the current working directory.

## 3. Recommended phase-one integration: custom popup command

### 3.1 Install and verify prerequisites

- Install the Orq binary in a directory on **Herdr server's PATH** inside the Debian VM, e.g. `~/.local/bin/orq`.
- Verify `command -v orq`, `orq --help`, `orq ui --help`, and `herdr --version` in the same user environment.
- Implement `orq ui` to accept **optional** `--repo <path>` and non-interactive diagnostics (e.g. `orq doctor herdr`). These are Orq design requirements, **not currently shipped commands**.
- Before changing config, confirm the installed Herdr version supports `[[keys.command]]` and `type = "popup"`.

### 3.2 Keybinding configuration

Add this to Herdr's `config.toml` (merge with existing configuration; avoid conflicting keys):

```toml
[[keys.command]]
key = "prefix+alt+o"
type = "popup"
command = "orq ui"
description = "Orq — feature and pipeline control"
width = "90%"
height = "85%"
```

Reload Herdr configuration:

```bash
herdr server reload-config
```

Then focus a terminal pane whose CWD is inside a Git repository and invoke the shortcut. Popup dimensions are percentages of the terminal display, including the popup border. `Esc` should be handled by Orq to exit cleanly; Herdr gives the popup terminal input until its command exits.

**Why this first?** It is officially documented, avoids plugin packaging mechanics, and exercises the exact Ratatui UI later used by a manifest plugin.

### 3.3 Launch environment contract

Herdr custom commands may supply:

| Variable | Orq usage |
|---|---|
| `HERDR_ACTIVE_PANE_CWD` | Primary project discovery input |
| `HERDR_ACTIVE_PANE_ID` | Origin pane reference; **not** a popup ID |
| `HERDR_ACTIVE_TAB_ID` | Origin tab for navigation/restoration |
| `HERDR_ACTIVE_WORKSPACE_ID` | Origin workspace |
| `HERDR_SOCKET_PATH` | Connect to the correct Herdr session |
| `HERDR_BIN_PATH` | Preferred CLI binary for host operations |

A popup **does not receive `HERDR_PANE_ID` and has no persistent pane identity**. Do not infer the project from the popup process's own CWD if an explicit active-pane CWD was supplied.

**Resolution order:** explicit `--repo` → valid `HERDR_ACTIVE_PANE_CWD` → process CWD (standalone mode). Resolve repository root using `git -C <path> rev-parse --show-toplevel`. Preserve canonical repository identity when the origin is a linked worktree; do not accidentally use another worktree's feature file without an explicit project policy. Do not scan parents outside Git's reported repository root for `.tdd-features.json`.

If no valid repo or feature file is found: display a project-specific error/empty state with the detected path and a retry/exit option; **never** fall back to the last opened project silently.

## 4. Runtime interaction model

```text
Herdr focused project pane
       │ custom popup command
       ▼
    orq ui (Ratatui)
       ├── Features: repo/.tdd-features.json
       ├── Pipelines: local/global read model
       └── Actions: start | pause | resume | switch | jump | open MR
                   │ validated commands
                   ▼
           Orq core / supervisor
                   ├── durable SQLite run/event state
                   ├── git worktrees + agent subprocesses
                   └── independent Herdr execution pane (optional host view)
```

**Do not run a long-lived orchestrator as a popup child.** The popup may exit or Herdr may disconnect; the supervisor must keep execution alive. For the earliest prototype, only demonstrate launching an independently owned process; durability claims require the supervisor/recovery MVP from the main spec.

### UI events

- Mouse: click tabs, rows, primary controls; wheel scrolling; hover where supported.
- Keyboard parity: arrows/Tab, Enter, Esc, shortcuts; accessible status labels beyond colour.
- `Esc` dismisses Orq UI gracefully, restoring the underlying Herdr view.
- No `SIGSTOP`/`SIGCONT` as a substitute for durable pause/resume.
- TUI never blocks its render thread on `git`, filesystem scans, LLM execution, or network calls.

## 5. Opening and focusing execution panes

Persist navigation **separately** from pipeline identity:

```json
{
  "run_id": "run-123",
  "project_root": "/home/dev/projects/commerce-api",
  "feature_id": "COM-02",
  "target_branch": "main",
  "herdr": {
    "session_socket": "<captured session socket path>",
    "workspace_id": "w1",
    "tab_id": "w1:t1",
    "pane_id": "w1:p2"
  },
  "review_url": null
}
```

`session_socket` is a local navigation hint, not a portable stable identifier across machines; revalidate it after reconnect/restart. Pane IDs are not durable execution IDs.

**Launch:** Orq creates a run with a unique ID and immutable repo/branch inputs. The supervisor starts or attaches the execution process. The Herdr adapter opens a dedicated pane/tab using a *documented* pane/tab launch operation and receives/queries authoritative IDs before recording the mapping. Never generate presumed `w1:pX` identifiers.

**Navigate:** On selecting “View pane”, validate the stored workspace/tab/pane in the active Herdr session. Prefer documented CLI helpers or the socket API (workspace/tab focus and/or `agent.focus` / `plugin.pane.focus` where appropriate). Herdr does **not** expose a generic `pane.focus` raw method; do not code against one. Close the popup at a verified point in the focus sequence, then focus the target; test ordering against installed Herdr because popups are modal. On failure, restore the popup or explain the missing pane and offer logs.

**MR/PR:** Orq stores the provider-returned exact URL. On Linux VM, opening via `xdg-open` can open the VM browser; provide **Copy URL** as a reliable cross-host fallback. Never construct GitLab/GitHub URLs from branch names.

## 6. Optional phase two: manifest-based Herdr plugin

After validating the command integration, investigate a distributable Herdr plugin with a `[[panes]]` entrypoint invoking `orq ui` and `placement = "popup"`. Its lifecycle is still session-modal and does not own the pipeline. Confirm the **current installed version's** manifest schema and `herdr plugin pane open` argument syntax from the official documentation before implementing the manifest. Use a separate `orq-herdr` adapter crate for host-specific functions; never copy Orq's orchestration state machine into the plugin.

The plugin path should behave identically to the custom command path for repo selection, mouse events, backend actions, and cancellation. Manifest pane invocations expose a different plugin context than custom command keybindings: normalize both launch contexts in one `HerdrLaunchContext` constructor and test them independently.

## 7. MVPs and acceptance criteria

### H0 — Herdr feasibility spike

- [x] `orq ui` opens and exits in a Herdr popup from a custom keybinding.
- [x] Project context from the originating pane is captured correctly.
- [x] Left click, scrolling and Esc tested in the Debian VM.
- [ ] Resizing tested in the Debian VM (still to verify).
- [x] Verify focus/close ordering and the versioned CLI/socket contract.
- [x] Report compatibility errors rather than crash when Herdr isn't present.

### H1 — Project-aware Features UI

- [ ] Read only the detected repo's `.tdd-features.json`.
- [ ] No silent cross-project feature leakage when switching tabs/workspaces.
- [ ] Validated `draft → pending` transition and safe file write (atomic replace with conflict detection).
- [ ] Target defaults to `main` and is validated against configured remote before launch.

### H2 — Run launching + navigation

- [ ] Start run in an isolated worktree and independently owned process.
- [ ] Persist run ID and real pane mapping; exiting popup doesn't kill run.
- [ ] “View pane” returns to the correct workspace/tab/pane, including from All Projects.
- [ ] Missing pane, missing Herdr socket and reconnect states show useful fallback.

### H3 — Pipeline control integration

- [ ] Mouse-first Pipelines view with project/all-project filter.
- [ ] Stage history and error text come from Orq run events, not terminal scraping.
- [ ] Pause/resume/agent switch invoke validated Orq commands and show pending/acknowledged states correctly.
- [ ] Review URL opens when supported or can be copied reliably.

### H4 — Plugin distribution (optional)

- [ ] Link/enable the packaged plugin according to installed Herdr manifest/CLI schema.
- [ ] Plugin entrypoint opens the same Orq UI in popup placement.
- [ ] Upgrade/uninstall doesn't remove Orq run data or kill independent runs.
- [ ] Test custom-command and manifest contexts on Linux; macOS optional.

## 8. TDD and failure tests

| Test | Expected |
|---|---|
| Launch from nested `commerce-api/src` | Resolves `commerce-api/.tdd-features.json` |
| Switch active Herdr project | Displays only new project's features |
| Launch outside Git repo | Explicit empty/error state, no stale features |
| Feature file absent/invalid | Readable failure, no mutation or launch |
| Close popup during active run | Run continues |
| Run pane removed | Navigation degrades to logs/recovery |
| Herdr session changes | Stale socket/pane mapping isn't trusted |
| Repeated Start clicks | At most one accepted active run per protected feature/branch key |
| Target branch missing | No worktree or MR created |
| Review URL available | Click opens or copy returns exact stored URL |
| Popup resized to minimum | Usable keyboard fallback and non-clipped critical actions |
| Host absent | `orq ui --repo ...` still works without Herdr integration |

Use dependency-injected `HerdrClient`/`RunController` traits and mock socket/CLI responses in tests. Add a handful of manual end-to-end verification cases against real installed Herdr; don't assert untested mouse capture or focus semantics solely from mocks.

## 9. Implementation guardrails / rabbit-hole alerts

- **Avoid forking Herdr** to draw native Settings widgets. Ratatui in a session-modal popup meets the initial UX goal.
- **Avoid inventing a global project registry** for MVP 1; use the origin pane's repo.
- **Avoid binding run lifecycle to Herdr panes**; run state lives in Orq, not in host UI state.
- **Avoid a web service for this integration spike**; Rust CLI + later persistent controller suffice.
- **Don't overpromise mouse parity:** test left/right-click and wheel forwarding on the actual Herdr build and outer terminal.
- **Git policy:** never derive target branch from active feature branch; default `main` and validate remote existence.
- **Security:** treat feature files/working paths as untrusted, avoid shell-string interpolation and command injection, never log agent OAuth credentials, and require confirmation for destructive or merge actions.

## 10. Documentation and validation references

Reference the installed Herdr version first, then these live official pages:

- [Herdr configuration — custom command popups](https://herdr.dev/docs/configuration/)
- [Herdr CLI reference](https://herdr.dev/docs/cli-reference/)
- [Herdr socket API](https://herdr.dev/docs/socket-api/)

**Definition of done for the integration:** With Herdr focused on a Git repo, the user invokes a shortcut, Orq opens as a mouse-first popup scoped to that repo's `.tdd-features.json`, the user starts a correctly based run, closes the popup, and can reopen it to navigate to the live execution pane and the exact MR/PR URL, without the popup being required for run survival.
