# MVP 1.1 — Feature creation and spec-assisted generation

## What is new

- Create and edit features from the popup; rename is disabled.
- Generate candidate features from a spec with the implementation agent
  (Settings tab: harness + model name; names only, never secrets).
- Preview generated candidates (new / duplicate / invalid) before adding any;
  accepted candidates are appended in one atomic save with conflict detection.
- New features are always written as `draft`; Orq never invents `recordedAt`
  or `cyclesCompleted`, and agent-supplied `status`, `recordedAt` and
  `cyclesCompleted` are ignored.
- `parallelizable` is an optional boolean; absent means exclusive. A generated
  `true` is only a suggestion the user may remove.

## Demo

`cargo test -p orq-features --test mvp11_end_to_end_demo` creates a feature by
hand and from a spec with a fake agent in a disposable repository, promotes it
to `pending`, and reads the file the way the TDD MCP server does.

## Schema items resolved against MCP-produced ledgers

Checked by `crates/orq-features/tests/mcp_compat_fixtures.rs`:

- Status vocabulary: `draft`, `pending`, `in_progress`, `completed`,
  `abandoned` are recognised; any other value is kept verbatim, never coerced
  and never promoted.
- `recordedAt` (ISO-8601 string) and `cyclesCompleted` (integer) are written
  only by the MCP; Orq preserves them and never creates them.
- `testFile` is stored as given (a path or cargo argument string) and is never
  executed by Orq.
- Unknown extra properties on entries survive append, edit and promote.

## Notes

- `.tdd-config.json` stays authoritative for reviewers and is never written.
- `TDD_FEATURES_PATH` is not read by Orq.
