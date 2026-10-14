# Using Orq from Herdr

Orq runs in a Herdr popup. Herdr starts `orq ui` in the active pane's
directory context and passes it through `HERDR_ACTIVE_PANE_CWD`.

## Keybinding

Add to your Herdr config, then run `herdr server reload-config`:

```toml
[[keys.command]]
key = "prefix+alt+o"
type = "popup"
command = "orq ui"
description = "Orq — feature and pipeline control"
width = "90%"
height = "85%"
```

## Which project is shown

Resolution order, with no silent fallback to any other project:

1. `--cwd <path>` (for `orq project inspect` / `orq doctor herdr`)
2. `HERDR_ACTIVE_PANE_CWD`
3. the process working directory

The repository root is then found with Git, so a pane in a nested
subdirectory shows the repository's ledger. A linked worktree without its own
`.tdd-features.json` shows the main checkout's. A pane directory that no
longer exists is an error naming `HERDR_ACTIVE_PANE_CWD`; outside a Git
repository the popup says so and shows no features.

## Diagnosing

`orq doctor herdr` prints the `HERDR_*` variables Orq saw, the Herdr version,
the launch directory, the repository root, whether it is a linked worktree,
and the ledger path and status. It never writes anything. Run it from a Herdr
pane (or as a popup command) to see exactly what the popup would resolve.

## Manual checklist (run in your Herdr session)

- [x] `prefix+alt+o` opens the popup; Esc closes it and focus returns.
- [x] Clicking a card selects it; the wheel scrolls; Enter opens detail.
- [ ] Resizing the terminal re-lays out the popup without garbling.
- [x] `orq doctor herdr` from a nested directory names the repo root.
- [x] From a deleted directory it fails naming `HERDR_ACTIVE_PANE_CWD`.
