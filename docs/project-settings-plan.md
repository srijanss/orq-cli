# Project settings: agents and what a run does when it finishes

## Why

Settings today edits `[implementer]` / `[reviewer]` in this machine's `config.toml`. One file serves
every project, and a run started without `--implementer` / `--reviewer` never reads it: it uses
`[roles]` or the built-in agents. Effort is saved but nothing passes it to a run. Every run also ends
the same way: push, open a PR/MR, wait for CI.

## Decisions

- Project settings live on this machine, in `orq.db` (the SQLite store in the data root), in a new
  `project_settings` table keyed by the canonical path of the project's main checkout. The web maps
  its project id to that path. Nothing is written to the repo, so saving leaves no uncommitted change.
- Each role (implementer, reviewer) is optional: harness, model, effort. `delivery` defaults to
  `review`. A `revision` counter makes a save that was based on a stale read fail instead of
  overwriting someone else's change.
- When a project has no record for a role, the fallbacks apply: `.tdd-config.json`, then the
  machine-wide settings in `config.toml`.
- The machine-wide `[implementer]` / `[reviewer]` tables become the machine defaults that runs use.
- Merge locally is fast-forward only. No merge commits.

```sql
CREATE TABLE project_settings (
    project_root          TEXT    NOT NULL PRIMARY KEY,
    implementer_harness   TEXT,
    implementer_model     TEXT,
    implementer_effort    TEXT,
    reviewer_harness      TEXT,
    reviewer_model        TEXT,
    reviewer_effort       TEXT,
    delivery              TEXT    NOT NULL DEFAULT 'review',
    revision              INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);
```

## Which agents a run uses

Implementer, first that is set wins:

1. `--implementer <profile>` (the web's "Start with…" passes this)
2. the project's implementer in `orq.db`
3. `[roles].implementer` (`.orq.toml`, then `config.toml`)
4. the machine default `[implementer]`
5. the built-in

Reviewer:

1. `--reviewer <profile>`
2. the project's reviewer in `orq.db`
3. `.tdd-config.json` `reviewers.default`
4. `[roles].reviewer`
5. the machine default `[reviewer]`
6. the built-in

`reviewers.allowedModels` still applies to every source. A set effort is the run's thinking level
(claude `--effort`, pi `--thinking`). Runs already started keep their agents.

## What a run does when it finishes

`delivery` is read when the run starts and recorded on the run, so a later change never alters a
run in flight. `orq run --delivery review|push|merge-local` overrides it for one run.

- `review` (today): push, open the PR/MR, wait for CI. Ends `review_ready`.
- `push`: push the branch and stop. Ends `pushed`.
- `merge_local`: after the gate passes, fast-forward the base branch to the run's branch. Nothing
  is pushed. Ends `merged`. If the base branch is checked out, its checkout must be clean and is
  updated with `merge --ff-only`. Otherwise the ref is moved only if it still points where the run
  rebased onto. If either check fails, the run ends `blocked` with the reason, and the branch and
  worktree are kept for a person.

This is the one place orq merges. The rule that agents never merge or approve (`no_merge.rs`)
stays.

## Web

- Each project gets a Settings page with implementer and reviewer (harness, model, effort), and
  "When a run finishes" (Open review / Push only / Merge locally). A role left unset shows "Machine
  default (…)" and can be reset to it. Saving uses the same version check as Settings and needs
  remote actions on.
- The Settings page's agents section is titled "Machine defaults" and says projects can override it.
- The launch picker preselects the project's agents. When no profile has that harness and model, it
  offers "Project default", which starts the run without `--implementer` / `--reviewer`.
- A run's page shows its delivery mode, and the endings Pushed and Merged locally.
