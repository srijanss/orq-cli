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

A project set up for the old orchestrate script keeps its gate. When nothing above names a gate, orq takes `GATE_CMD` from `<git-dir>/orchestrate.env`, then from `.orchestrate.env` in the repo: the first file to set it wins, and setting it to nothing means detection. It runs with `bash -c` and `GATE_BASE` set to the commit the branch was rebased onto, the run prints which file it came from, and the file is only parsed, never evaluated. Every other key in those files is ignored, as is a `GATE_CMD` environment variable.

`allowed_tools` adds Claude Code permission rules for the run's sessions, such as `"Bash(cargo test:*)"`. A rule that runs an interpreter (`node`, `python`, `ruby`, `perl`, `bun`, `deno`) is kept only in the form `Bash(<interpreter> <script>)` or `Bash(<interpreter> <script>:*)`, where the script is a relative path to a file checked in at the commit the run starts from. Before each session, a rule whose script the run has changed, committed or not, is left out and the run says why. Any other interpreter rule, and any rule that could push or publish, is left out too. orq's own `Bash(node build.mjs)`, run from the folder that holds a `build.mjs`, is allowed only while every `build.mjs` in the worktree is one the base holds checked in and the run has not changed, and while the worktree holds no repository of its own, such as a submodule, whose files orq cannot check.

The full list of keys is in `crates/orq-orchestrator/src/run_settings.rs`.

### Setup steps (installing dependencies)

Before the agent session and the gate, a run installs dependencies in its worktree. It runs the `setup` steps from the settings first, then one install step it detects from the worktree **root** only: `uv sync --locked` for `uv.lock`, or `npm ci` / `pnpm` / `yarn` / `bun` for a root `package.json` with its lockfile. Steps run from the worktree root, each as an argv list, and their output goes to the run's `setup.log`. A failed step fails the run at setup.

A project whose packages live in a subfolder must name them, because detection does not look there. This repository's are in `crates/orq-remote/web-src`. Without them, the gate's `orq-remote` tests that check for them (`crates/orq-remote/tests/web_deps/mod.rs`, used by `web_a11y.rs` and others) fail with "node is installed but the web dependencies are not", and the run is blocked at `gate_fix` as an unrelated failure.

```toml
setup = [["npm", "ci", "--prefix", "crates/orq-remote/web-src"]]
```

This line is currently set only in this machine's private `.git/orq/run.toml` (added 2026-11-14), not in a checked-in `.orq/run.toml`. A fresh clone or another machine needs it added to either file. A `setup` list in the private file replaces the checked-in one rather than adding to it, so keep the two identical if both are set. To recover a run already blocked this way, run `npm ci` in `crates/orq-remote/web-src` inside the run's worktree and resume it. See `docs/run-setup-web-deps-draft.md`.

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

## Writing tests that run scripts

A test that writes a shell script and runs it writes it with `orq_test_support::write_executable(path, body)`, not `fs::write` and a `0o755` chmod. Tests in one binary run as threads, and a thread that forks while another still has its script open for writing makes running that script fail with ETXTBSY ("text file busy"); `write_executable` returns only once the script can be run. Move a test file to it when you next touch it.

## The web: `orq serve`

`orq serve` starts Orq's web: your projects, their features, runs and logs, the Reviews page, settings and storage. It only shows until you turn **Remote actions** on in Settings (your passkey confirms it); then it can also start, pause and resume runs, resolve, dismiss and reopen reviews, and clean up storage. Sign in with the token it creates and prints the location of, or with a passkey over HTTPS. See [docs/remote-inbox.md](docs/remote-inbox.md).

### Start the server with `tailscale serve` (HTTPS, passkey sign-in)

```sh
# build once (debug build, one job at a time to keep memory and disk low)
cargo build -j 1 -p orq-cli

# 1. find your machine's host name: <machine>.<tailnet>.ts.net
tailscale status

# 2. start the server on loopback, naming that host for passkeys (leave it running)
orq serve --repo . --listen 127.0.0.1:7878 --passkey-host box.tail1234.ts.net

# 3. in a second terminal, publish it over HTTPS on your tailnet
tailscale serve --bg 7878
```

Open `https://box.tail1234.ts.net` on the phone and sign in with the token once to add a passkey. Replace `box.tail1234.ts.net` with your own host name. Add `--no-loopback-token` to make a passkey the only way in. To stop publishing, run `tailscale serve reset`. Without HTTPS, use `orq serve --repo . --listen <tailscale-ip>:7878` and sign in with the token. Full details, token setup and recovery are in [docs/remote-inbox.md](docs/remote-inbox.md).

### Install it as an app

The page can be installed to the home screen, with its own icon and window:

- Android or desktop Chrome/Edge: open the page, then choose **Install app** from the browser menu.
- iPhone or iPad Safari: tap **Share**, then **Add to Home Screen**.

On iOS an installed app keeps its own cookies, apart from Safari's, so sign in once inside it. A passkey works there.
