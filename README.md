# orq

Runs a feature through a pipeline in an isolated git worktree: implement and review, rebase, gate, push, review request, CI wait and fix, cleanup.

## Run settings

No configuration file is required. Every setting has a default, and the gate is detected from the project's files.

| Setting | Default |
|---|---|
| Gate | Detected: Python, then Node, then Rust. Nothing detected skips the gate. |
| Forge | Detected from the remote's host (`github.com` and `gitlab.com` are built in). |
| Base branch | Passed as `--target`. |
| Branch prefix | `fix--` |
| MCP servers | Every server in `.mcp.json` is approved; none is set up with `mcpctl`. |

Add `.orq/run.toml` (checked in) or `<git-dir>/orq/run.toml` (private) only to change a default:

```toml
gate = ["make", "check"]          # one command as an argv list; overrides detection
model = "opus"
mcp_servers = ["project-mcp", "outside-in-tdd-mcp"]
mr_assignee = "you"
mr_reviewer = "teammate"
ci_timeout = 1800
forge = "gh"                      # for a self-hosted host; "gh" or "glab"
```

Layering, key by key: `ORQ_*` environment variables beat the private file, which beats the checked-in file. Commands are argv lists, never shell text; put a compound command in a script and call that.

The full list of keys is in `crates/orq-orchestrator/src/run_settings.rs`.

## Running a feature

```
orq feature add --repo . --name <name> --description "<text>"
orq run --repo . --feature <name> --remote origin --target main
orq runs --repo .
```

`--no-ci-check`, `--no-gate` and `--no-rebase` skip those stages.

## Testing without a forge

A run pins its repository to a remote URL with a host, so a plain local path is refused. To try the pipeline against a local bare repository, use an ssh-style URL with a shim that runs git locally, and name the forge:

```
printf '#!/bin/sh\nfor a; do last=$a; done\nexec sh -c "$last"\n' > ssh && chmod +x ssh
git remote set-url origin git@forge.example:/abs/path/remote.git
GIT_SSH_COMMAND=$PWD/ssh ORQ_FORGE=gh orq run --repo . --feature <name> --remote origin --target main --no-ci-check
```

The run stops at the review request, since no forge answers.
