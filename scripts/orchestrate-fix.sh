#!/usr/bin/env bash
# Orchestrate a feature/fix in an isolated git worktree:
#   1. create a worktree + branch from the base branch
#   2. ask which MCP servers from `mcpctl catalog list` to initialise and run
#      `mcpctl setup --mcp <name>` for each selected one (this also scaffolds
#      the /implement-and-review command and skill into the worktree)
#   3. pre-approve the selected MCP servers for the Claude session, and install
#      the dependencies in the worktree so tests can run (uv sync, npm/pnpm/
#      yarn/bun install by lockfile; SETUP_CMD overrides)
#   4. start Claude with `/implement-and-review <feature-name>`
#   5. when the session ends: rebase, run the local gate (the stack's checks:
#      Django migration check + pytest, Node typecheck + test, cargo test;
#      Claude fixes failures),
#      push the branch, have Claude draft the MR/PR title/description from the
#      commits, and open it with glab (GitLab) or gh (GitHub, picked from the
#      origin URL or FORGE=gh|glab); the description ends with
#      `Closes #<issue>` when -i is given
#   6. wait for the CI pipeline (GitHub: all workflow runs of the commit); when
#      jobs fail, have Claude read their logs, fix the issues and push, until it
#      is green (or attempts run out)
#   7. remove the worktree, tell you to check the MR and send a notification
#      with its URL and the CI result (NOTIFY_ON=fail: only errors notify)
#
# Progress of every run is recorded in <git-dir>/orchestrate/<branch>.json
# (stage, session id, attempt counters, MR url). A re-run resumes the recorded
# implement session when its transcript still exists.
#
# Usage:
#   orchestrate-fix.sh [-i <issue-id>] [-t <target-branch>] [--mcp <name>]... [--model <model>] [--assignee <user>] [--reviewer <user>] <feature-name> [extra request text...]
#   orchestrate-fix.sh --mr [-i <issue-id>] [-t <target-branch>] [--model <model>] [--assignee <user>] [--reviewer <user>] [<branch>]
#   orchestrate-fix.sh --reap [--dry-run] [--yes] [-t <target-branch>]
#
# Run it from inside any git repository; the repo root, default branch and
# worktree location are all derived from the current directory.
#
# Steps 5-7 are skipped (worktree kept) if the session left uncommitted
# changes or made no commits. The worktree is also kept if the pipeline is
# still red after the fix attempts.
#
# --mr (MR only): skip steps 1-4 and 6. Pushes an existing local branch (default:
# the branch currently checked out here) and creates the MR for it. Nothing is
# created or removed besides the MR.
#
# Env overrides:
#   BASE_BRANCH   default target branch, same as -t
#                 (default: origin's HEAD branch, else main/master/develop)
#   BRANCH_PREFIX prefix for the new branch        (default: fix--)
#   CI_FIX_ATTEMPTS  how often Claude may fix a failed pipeline (default: 2)
#   CI_WAIT_TIMEOUT  seconds to wait for one pipeline          (default: 3600)
#   GATE_CMD      shell command for the local gate, run in the worktree with
#                 GATE_BASE set to the ref the branch was rebased onto
#                 (default: auto-detected, see --help)
#   GATE_FIX_ATTEMPTS  how often Claude may fix a failed gate    (default: 2)
#   GATE_SCOPE    "full" runs the whole test suite (default), "changed" only
#                 the changed src/<app> directories that have a tests/ package
#                 (Django only)
#   GATE_LINT     set to 1 to also run the linters: ruff on changed files, the
#                 package.json `lint` script, cargo fmt --check + clippy
#                 (GATE_RUFF is the old name)
#   GATE_BUILD    set to 1 to also run the package.json `build` script
#   SETUP_CMD     dependency install run in the new worktree
#                 (default: uv sync / npm ci / pnpm / yarn / bun by lockfile)
#
# Per-repo settings: <git-dir>/orchestrate.env (private, not part of the repo)
# or <main checkout>/.orchestrate.env with KEY=value lines for
# FORGE BASE_BRANCH BRANCH_PREFIX GATE_CMD GATE_SCOPE GATE_LINT GATE_BUILD
# GATE_FIX_ATTEMPTS SETUP_CMD CI_FIX_ATTEMPTS CI_WAIT_TIMEOUT WORKTREE_DB_VARS
# KEEP_TEST_DB NOTIFY_CMD NOTIFY_ON. Environment variables win over the file.
#   NOTIFY_CMD    command run when a run stops with an error or finishes with
#                 an MR/PR to review, message in $NOTIFY_MESSAGE
#                 (e.g. an ntfy.sh / Pushover call)
#   NOTIFY_ON     all (default) or fail: whether completion also notifies
#   NOTIFY_CLAUDE 1 (default): also push through the Claude app via `claude -p`
#                 and the PushNotification tool (needs Remote Control)
#   AUTOCOMPACT_WINDOW  context size at which the interactive sessions
#                    auto-compact (100k-1M, or "auto"; default: 150k;
#                    set to "" for Claude's own default)
set -euo pipefail

# Everything below sits in one brace group so bash parses the whole script
# before running any of it. Without that, bash reads the file incrementally and
# a running instance breaks ("syntax error near unexpected token") when the
# file is overwritten meanwhile, e.g. by re-installing it with cp while
# parallel runs are active.
{

SYNOPSIS="usage: $0 [-i <issue-id>] [-t <target-branch>] [--mcp <name>]... [--model <model>] [--assignee <user>] [--reviewer <user>] <feature-name> [extra request text...]
       $0 --mr [-i <issue-id>] [-t <target-branch>] [--model <model>] [--assignee <user>] [--reviewer <user>] [<branch>]
       $0 --reap [--dry-run] [--yes] [-t <target-branch>]
       $0 -h"

usage() {
  printf '%s\nRun with -h for details.\n' "$SYNOPSIS" >&2
  exit 64
}

help() {
  cat <<EOF
$SYNOPSIS

Create a git worktree for <feature-name>, set up the MCP servers you pick from
the mcpctl catalog, run /implement-and-review in Claude, rebase onto the target
branch, then push, open an MR (glab) or PR (gh) and remove the worktree. Run it from inside the target repository.

Options:
  -i <issue-id>      Numeric issue id; adds "Closes #<issue-id>" to the MR
  -t <target-branch> Branch to fork from and open the MR against
                     (default: \$BASE_BRANCH, else origin's HEAD branch,
                     else main/master/develop; must exist locally)
  --mcp <name>       MCP server from \`mcpctl catalog list\` to initialise;
                     repeatable. Without it you are asked interactively.
  --mr               MR only: push an existing branch (default: the current
                     one) and create its MR; no worktree, no Claude session
  --no-rebase        Do not rebase the branch onto the target branch before
                     pushing. By default it is fetched and rebased first; on
                     conflicts a Claude session resolves them (or, if that
                     fails, the worktree is kept mid-rebase for you).
  --no-ci-check      Do not wait for the MR's CI pipeline. By default the
                     full pipeline is watched after the MR is created; when
                     jobs fail, a Claude session reads their logs, fixes
                     the issues and pushes (up to \$CI_FIX_ATTEMPTS times).
  --no-gate          Skip the local gate before the push. By default, after
                     the rebase, \$GATE_CMD runs in the worktree; without it
                     the gate is auto-detected per stack at the repo root:
                     Django: \`makemigrations --check\` + the full pytest suite
                     (GATE_SCOPE=changed: only the changed apps); uv: pytest;
                     Node: typecheck (script or tsc) + the \`test\` script;
                     Rust: cargo test --workspace. GATE_LINT=1 adds linters,
                     GATE_BUILD=1 the Node build. When it fails, the implement session is resumed to
                     fix it (up to \$GATE_FIX_ATTEMPTS times); the push only
                     happens once it passes.
  --notify-test      Send a test notification through NOTIFY_CMD / the Claude
                     push route and exit. Runs that stop with an error (any
                     non-zero exit except Ctrl-C) send one automatically.
  --reap             Clean up after interrupted runs, then exit: worktrees
                     under .claude/worktrees whose branch is merged (or whose
                     MR is merged/closed and fully pushed), that are clean and
                     have no running session; their local branch (if safely
                     merged), backup ref and run state; and per-worktree test
                     databases that no worktree owns. Lists what it would do
                     and asks first.
  --dry-run          With --reap: only list, change nothing
  --yes              With --reap: do not ask for confirmation
  --no-auto-exit     Do not close the Claude session automatically. By
                     default Claude is asked to create a marker file as its
                     last step and the session is then closed so push + MR
                     follow hands-off; with this flag you exit it yourself.
  --model <model>    Claude model (alias or full id, e.g. sonnet) for the
                     implement session and the MR message draft
                     (default: your Claude Code default)
  --assignee <user>  Username (comma-separated for several) to assign the
                     MR/PR to; "none" leaves it unassigned. Default:
                     \$MR_ASSIGNEE, else yourself (GitLab quick action
                     "/assign me", GitHub "--assignee @me").
  --reviewer <user>  Username (comma-separated for several) to request a
                     review from; reviewers are notified. Default:
                     \$MR_REVIEWER, else none.
  -h, --help         Show this help and exit

Arguments:
  <feature-name>     Slug; the branch becomes \${BRANCH_PREFIX}<feature-name>
  [extra text...]    Appended to the /implement-and-review request
  [<branch>]         With --mr, the local branch to open the MR for

Environment:
  FORGE              gh or glab: the CLI that opens the MR/PR and reads its CI
                     (default: gh for a github.com origin, else glab; set it
                     for GitHub Enterprise or a self-hosted forge)
  BASE_BRANCH        Default target branch (default: auto-detected)
  BRANCH_PREFIX      Prefix for the new branch            (default: fix--)
  CI_FIX_ATTEMPTS    How often Claude may fix a failing pipeline   (default: 2)
  CI_WAIT_TIMEOUT    Seconds to wait for a pipeline to finish      (default: 3600)
  GATE_CMD           Command for the local gate (see --no-gate)
  GATE_FIX_ATTEMPTS  How often Claude may fix a failing gate       (default: 2)
  REMOTE_CONTROL     1 (default): the interactive Claude sessions start with
                     --remote-control=<branch>, so they show up in the Claude
                     mobile app; 0 disables it
  FAILURE_SESSION    1 (default): when a run stops with an error, open a
                     Remote Control session in the worktree that explains the
                     failure and waits for you (needs a terminal and
                     REMOTE_CONTROL=1); 0 disables it
  NOTIFY_CMD         Command run on failure and on completion with the text in
                     \$NOTIFY_MESSAGE,
                     e.g. 'curl -s -d "\$NOTIFY_MESSAGE" ntfy.sh/<topic>'
  NOTIFY_ON          all (default): also notify when a run finishes with an
                     MR/PR to review (URL and CI result); fail: only errors
  NOTIFY_CLAUDE      1 (default) also pushes through the Claude app with the
                     PushNotification tool: from a short Remote Control
                     session that closes itself when a terminal and
                     REMOTE_CONTROL=1 are available, else a \`claude -p\`
                     call (often not delivered); 0 disables it
  GATE_SCOPE         full (default) or changed, see --no-gate
  GATE_LINT          1 = also run the linters (ruff, lint script, clippy)
  GATE_BUILD         1 = also run the package.json build script
  SETUP_CMD          Dependency install in the new worktree
                     (default: uv sync / npm ci / pnpm / yarn / bun)
  .orchestrate.env   Per-repo KEY=value file in the main checkout for the
                     settings above (environment variables win)
  AUTOCOMPACT_WINDOW Context size at which the Claude sessions auto-compact:
                     100k-1M or "auto"; "" keeps Claude's default (default: 150k)
  KEEP_TEST_DB       Set to 1 to keep the per-worktree test databases
                     instead of dropping them when the worktree is removed
  WORKTREE_DB_VARS   Variables whose database name is made per-worktree as
                     <main value>_<branch> (default: "DB_NAME B2C_DB_NAME";
                     set to "" to share the main databases)
  MR_ASSIGNEE        Default MR assignee, same as --assignee
  MR_REVIEWER        Default MR reviewer, same as --reviewer

Examples:
  $0 -i 123 card-testimonials-not-visible
  $0 -t deployment-v4.3.0 card-testimonials-not-visible
  $0 --mr -i 123
  $0 --model opus -i 123 card-testimonials-not-visible
  $0 --mcp project-mcp --mcp outside-in-tdd-mcp my-feature
  $0 --reap --dry-run
EOF
  exit 0
}

# getopts has no long options: pull --help / --mr / --mcp / --no-auto-exit / --no-ci-check / --model / --assignee / --reviewer out of the arguments first.
MODEL=""
AUTO_EXIT=1
ASSIGNEE="${MR_ASSIGNEE:-}"
REVIEWER="${MR_REVIEWER:-}"
MR_ONLY=0
REBASE=1
CI_CHECK=1
GATE=1
NOTIFY_TEST=0
REAP=0
REAP_DRY=0
REAP_YES=0
MCP_SELECTED=()
ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help) help ;;
    --mr) MR_ONLY=1; shift ;;
    --no-gate) GATE=0; shift ;;
    --notify-test) NOTIFY_TEST=1; shift ;;
    --reap) REAP=1; shift ;;
    --dry-run) REAP_DRY=1; shift ;;
    --yes) REAP_YES=1; shift ;;
    --no-auto-exit) AUTO_EXIT=0; shift ;;
    --no-rebase) REBASE=0; shift ;;
    --no-ci-check) CI_CHECK=0; shift ;;
    --mcp)
      [[ $# -ge 2 && -n "$2" ]] || { echo "--mcp needs a value" >&2; exit 64; }
      MCP_SELECTED+=("$2")
      shift 2
      ;;
    --assignee)
      [[ $# -ge 2 && -n "$2" ]] || { echo "--assignee needs a value" >&2; exit 64; }
      ASSIGNEE="$2"
      shift 2
      ;;
    --assignee=*)
      ASSIGNEE="${1#--assignee=}"
      [[ -n "$ASSIGNEE" ]] || { echo "--assignee needs a value" >&2; exit 64; }
      shift
      ;;
    --reviewer)
      [[ $# -ge 2 && -n "$2" ]] || { echo "--reviewer needs a value" >&2; exit 64; }
      REVIEWER="$2"
      shift 2
      ;;
    --reviewer=*)
      REVIEWER="${1#--reviewer=}"
      [[ -n "$REVIEWER" ]] || { echo "--reviewer needs a value" >&2; exit 64; }
      shift
      ;;
    --mcp=*)
      [[ -n "${1#--mcp=}" ]] || { echo "--mcp needs a value" >&2; exit 64; }
      MCP_SELECTED+=("${1#--mcp=}")
      shift
      ;;
    --model)
      [[ $# -ge 2 && -n "$2" ]] || { echo "--model needs a value" >&2; exit 64; }
      MODEL="$2"
      shift 2
      ;;
    --model=*)
      MODEL="${1#--model=}"
      [[ -n "$MODEL" ]] || { echo "--model needs a value" >&2; exit 64; }
      shift
      ;;
    *) ARGS+=("$1"); shift ;;
  esac
done
set -- ${ARGS[@]+"${ARGS[@]}"}
MODEL_ARGS=()
AUTOCOMPACT_WINDOW="${AUTOCOMPACT_WINDOW-150k}"
COMPACT_ARGS=()
[[ -z "$AUTOCOMPACT_WINDOW" ]] || COMPACT_ARGS=(--autocompact "$AUTOCOMPACT_WINDOW")
# Interactive sessions start with Remote Control on (visible and answerable
# from the Claude mobile app); REMOTE_CONTROL=0 turns that off.
RC_ON=""
[[ "${REMOTE_CONTROL:-1}" != "1" ]] || RC_ON=1

ISSUE=""
TARGET=""
while getopts ":i:t:h" opt; do
  case "$opt" in
    i) ISSUE="${OPTARG#\#}" ;;
    t) TARGET="$OPTARG" ;;
    h) help ;;
    *) usage ;;
  esac
done
shift $((OPTIND - 1))

[[ -z "$MODEL" ]] || MODEL_ARGS=(--model "$MODEL")

if [[ -n "$ISSUE" && ! "$ISSUE" =~ ^[0-9]+$ ]]; then
  echo "issue-id must be numeric: '$ISSUE'" >&2
  exit 64
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "not inside a git repository: $PWD (run this from your project directory)" >&2
  exit 64
fi

# Resolve the main checkout even when invoked from inside a worktree.
REPO_ROOT="$(cd "$(git rev-parse --git-common-dir)/.." && pwd)"

log() { printf '\n==> %s\n' "$*"; }

# Per-repo defaults: <main checkout>/.orchestrate.env holds plain KEY=value
# lines (optionally quoted, no inline comments, never evaluated by the shell)
# for the settings below, so each project can say how it is built and tested.
# A variable already set in the environment wins over the file.
CONFIG_KEYS="FORGE BASE_BRANCH BRANCH_PREFIX GATE_CMD GATE_SCOPE GATE_LINT GATE_RUFF GATE_BUILD GATE_FIX_ATTEMPTS SETUP_CMD CI_FIX_ATTEMPTS CI_WAIT_TIMEOUT WORKTREE_DB_VARS KEEP_TEST_DB NOTIFY_CMD NOTIFY_ON"
# Two locations, the first one that sets a variable wins: <git-dir>/orchestrate.env
# (private, never shows in git status, nothing to commit) and the repo file.
for CONFIG_FILE in "$(git -C "$REPO_ROOT" rev-parse --absolute-git-dir)/orchestrate.env" "$REPO_ROOT/.orchestrate.env"; do
  [[ -f "$CONFIG_FILE" ]] || continue
  while IFS= read -r cfg_line || [[ -n "$cfg_line" ]]; do
    [[ "$cfg_line" =~ ^[[:space:]]*(export[[:space:]]+)?([A-Z][A-Z0-9_]*)[[:space:]]*=[[:space:]]*(.*)$ ]] || continue
    cfg_key="${BASH_REMATCH[2]}"
    cfg_value="${BASH_REMATCH[3]}"
    cfg_value="${cfg_value%"${cfg_value##*[![:space:]]}"}"
    case "$cfg_value" in
      \"*\" | \'*\') cfg_value="${cfg_value:1:${#cfg_value}-2}" ;;
    esac
    case " $CONFIG_KEYS " in
      *" $cfg_key "*) ;;
      *) echo "warning: $CONFIG_FILE: ignoring unknown setting $cfg_key" >&2; continue ;;
    esac
    [[ -z "${!cfg_key+x}" ]] || continue
    printf -v "$cfg_key" '%s' "$cfg_value"
  done < "$CONFIG_FILE"
done

BASE_BRANCH="${TARGET:-${BASE_BRANCH:-}}"
BRANCH_PREFIX="${BRANCH_PREFIX:-fix--}"
CI_FIX_ATTEMPTS="${CI_FIX_ATTEMPTS:-2}"
CI_WAIT_TIMEOUT="${CI_WAIT_TIMEOUT:-3600}"
GATE_FIX_ATTEMPTS="${GATE_FIX_ATTEMPTS:-2}"
GATE_LINT="${GATE_LINT:-${GATE_RUFF:-0}}"

# Single EXIT trap: temp MR description file and the session watcher.
DESC_FILE=""
WATCHER=""
cleanup() {
  local rc=$? wt
  [[ -z "$DESC_FILE" ]] || rm -f "$DESC_FILE"
  [[ -z "$WATCHER" ]] || kill "$WATCHER" 2>/dev/null || true
  # A run that stops early leaves its state file behind, with the exit code,
  # and tells you about it (Ctrl-C / SIGTERM are your own doing: no message).
  if [[ $rc -ne 0 && -n "$STATE_FILE" && -f "$STATE_FILE" ]]; then
    state_set "last_exit=$rc"
    case "$rc" in
      130|143) ;;
      *)
        wt="$(state_get worktree)"
        notify "${BRANCH:-run}: stopped at stage '$(state_get stage)' (exit $rc).${wt:+ Worktree kept: $wt.} Re-run the same command to resume."
        failure_session "$rc"
        ;;
    esac
  fi
}
trap cleanup EXIT

# Per-run state: <git-dir>/orchestrate/<branch>.json. Lives in the main git dir,
# so it survives the worktree and never shows up in git status. Keys: stage,
# session_id, gate_attempt, ci_attempt, pipeline_id, mr_url, worktree, last_exit,
# history. Removed when the run completes. (The review round is internal to the
# implement skill and not visible here.)
STATE_FILE=""
state_init() { # state_init <branch>
  local dir
  dir="$(git -C "$REPO_ROOT" rev-parse --absolute-git-dir)/orchestrate"
  mkdir -p "$dir"
  STATE_FILE="$dir/${1//\//_}.json"
}
state_set() { # state_set key=value...
  [[ -n "$STATE_FILE" ]] || return 0
  python3 - "$STATE_FILE" "$@" <<'PYSTATE' || echo "warning: could not update $STATE_FILE" >&2
import json, os, sys, time

path, pairs = sys.argv[1], sys.argv[2:]
try:
    with open(path) as f:
        st = json.load(f)
except (OSError, ValueError):
    st = {}
for pair in pairs:
    key, value = pair.split("=", 1)
    st[key] = int(value) if key.endswith("_attempt") or key == "last_exit" else value
now = time.strftime("%Y-%m-%dT%H:%M:%S%z")
st["updated_at"] = now
if "stage" in st:
    hist = st.setdefault("history", [])
    if not hist or hist[-1]["stage"] != st["stage"]:
        hist.append({"stage": st["stage"], "at": now})
    del hist[:-40]
if "stage" in [p.split("=", 1)[0] for p in pairs]:
    st.pop("last_exit", None)
tmp = path + ".tmp"
with open(tmp, "w") as f:
    json.dump(st, f, indent=2)
    f.write("\n")
os.replace(tmp, path)
PYSTATE
}
state_get() { # state_get key -> value or empty
  [[ -n "$STATE_FILE" && -f "$STATE_FILE" ]] || return 0
  python3 - "$STATE_FILE" "$1" <<'PYGET' 2>/dev/null || true
import json, sys

try:
    with open(sys.argv[1]) as f:
        print(json.load(f).get(sys.argv[2], ""))
except (OSError, ValueError):
    pass
PYGET
}
session_exists() { # does a Claude transcript for session id $1 exist?
  compgen -G "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/*/$1.jsonl" >/dev/null
}

# Tell the user a run needs them. Two independent routes, both best effort:
#   NOTIFY_CMD     your own command, run with the text in $NOTIFY_MESSAGE
#                  (e.g. 'curl -s -d "$NOTIFY_MESSAGE" ntfy.sh/<topic>')
#   NOTIFY_CLAUDE  1 (default) = a one-shot `claude -p` call that uses the
#                  PushNotification tool. It reaches the Claude mobile app
#                  only when Remote Control is connected for that call.
#                  Check with --notify-test.
NOTIFY_CMD="${NOTIFY_CMD:-}"
NOTIFY_CLAUDE="${NOTIFY_CLAUDE:-1}"
#   NOTIFY_ON      all (default) = also tell when a run finishes with an MR/PR
#                  to review; fail = only when a run stops with an error
NOTIFY_ON="${NOTIFY_ON:-all}"
case "$NOTIFY_ON" in
  all|fail) ;;
  *) echo "NOTIFY_ON must be all or fail: '$NOTIFY_ON'" >&2; exit 64 ;;
esac
FAILURE_SESSION="${FAILURE_SESSION:-1}"

# After a failed run: open an interactive Claude session in the worktree with
# Remote Control on (resuming the implement session when its transcript exists)
# and the failure as its first prompt, so the failure shows up in the mobile
# app as a session you can answer. Blocks until that session ends. Skipped
# without a terminal, without Remote Control, or when no worktree exists.
failure_session() { # failure_session <exit-code>
  local rc="$1" dir gitdir sid f logs="" resume_args=() prompt
  [[ "$FAILURE_SESSION" == "1" && -n "$RC_ON" && -t 0 && -t 1 ]] || return 0
  dir="$(state_get worktree)"
  [[ -n "$dir" && -d "$dir" ]] || dir="$(wt_branch_at "${BRANCH:-}" 2>/dev/null || true)"
  [[ -n "$dir" && -d "$dir" ]] || return 0
  gitdir="$(git -C "$dir" rev-parse --absolute-git-dir 2>/dev/null)" || return 0
  for f in orchestrate-gate-log.txt orchestrate-ci-logs.txt; do
    [[ -s "$gitdir/$f" ]] && logs+="${logs:+, }$gitdir/$f"
  done
  sid="$(state_get session_id)"
  if [[ -n "$sid" ]] && session_exists "$sid"; then
    resume_args=(--resume "$sid")
  fi
  prompt="The orchestration script for branch $BRANCH stopped at stage '$(state_get stage)' (exit code $rc). The user may be reading this on their phone. Find out why: check \`git status\` and \`git log\` in this worktree (a rebase may be in progress)${logs:+, and read these logs: $logs}. Reply with a short diagnosis and the options (what you would fix, or what you need decided). Change only what the user tells you to. Never run \`git push\` or create a merge request: afterwards the user re-runs the same command, which rebases, runs the gate, pushes and opens the MR."
  log "Opening a Remote Control session in $dir to look at the failure"
  (cd "$dir" && claude ${MODEL_ARGS[@]+"${MODEL_ARGS[@]}"} ${COMPACT_ARGS[@]+"${COMPACT_ARGS[@]}"} \
    ${resume_args[@]+"${resume_args[@]}"} "--remote-control=$BRANCH-help" \
    --name "$BRANCH-help" "$prompt") || true
}
# Push <message> from a short interactive Claude session with Remote Control on,
# then close it. Returns non-zero when the session could not be run.
notify_via_session() { # notify_via_session <message>
  local msg="$1" name done_file watcher rc=0 prompt
  name="$(basename "$REPO_ROOT")-notify-$$"
  done_file="$(mktemp -u "${TMPDIR:-/tmp}/orchestrate-notify.XXXXXX")"
  prompt="Call the PushNotification tool exactly once with status \"proactive\" and exactly this message, nothing else: $msg
Then run \`touch $done_file\` and stop. Do nothing else."
  # The watcher closes the session once the marker exists, or after 60 seconds.
  # No `timeout` around claude: it would move claude into a background process
  # group, where an interactive session cannot read the terminal and just hangs.
  (
    n=0
    while [[ ! -e "$done_file" && $n -lt 60 ]]; do sleep 1; n=$((n + 1)); done
    sleep 3 # let the tool call finish
    pkill -TERM -f -- "--name ${name}( |\$)" || true
  ) &
  watcher=$!
  (cd "$REPO_ROOT" && claude --model haiku \
    --tools "PushNotification,Bash" --allowedTools "PushNotification,Bash(touch $done_file)" \
    --disable-slash-commands --strict-mcp-config \
    "--remote-control=$name" --name "$name" "$prompt") || rc=$?
  kill "$watcher" 2>/dev/null || true
  wait "$watcher" 2>/dev/null || true
  # 143 = closed by the watcher: fine if the marker exists.
  if [[ -e "$done_file" ]]; then
    rm -f "$done_file"
    return 0
  fi
  echo "warning: the notification session ended without sending (exit $rc)" >&2
  return 1
}

notify() { # notify <message>
  local msg="${1:0:190}" limit=()
  printf '\n!! %s\n' "$msg" >&2
  if [[ -n "$NOTIFY_CMD" ]]; then
    NOTIFY_MESSAGE="$msg" bash -c "$NOTIFY_CMD" >/dev/null 2>&1 \
      || echo "warning: NOTIFY_CMD failed" >&2
  fi
  if [[ "$NOTIFY_CLAUDE" == "1" ]] && command -v claude >/dev/null; then
    # With a terminal and Remote Control, push from a short interactive session
    # (like the implement session, it is connected to the Claude app): it sends
    # the notification, touches a marker file and is closed. A one-shot
    # `claude -p` call is not connected, so the push often never arrives.
    if [[ -n "$RC_ON" && -t 0 && -t 1 ]]; then
      notify_via_session "$msg" && return 0
    fi
    command -v timeout >/dev/null && limit=(timeout 90)
    (cd "$REPO_ROOT" && ${limit[@]+"${limit[@]}"} claude -p --model haiku \
      --tools PushNotification --allowedTools PushNotification \
      --no-session-persistence --disable-slash-commands --strict-mcp-config \
      "Call the PushNotification tool exactly once with status \"proactive\" and exactly this message, nothing else: $msg" \
      >/dev/null 2>&1) || echo "warning: could not send the Claude push notification" >&2
  fi
}

# How the CI check ended, for the completion message. Set by ci_check; stays
# "not-watched" with --no-ci-check and in --mr mode. Other values: passed,
# none (no pipeline found), unrelated (red, judged unrelated to the branch),
# ended:<status> (cancelled, action_required, ...).
CI_OUTCOME="not-watched"

# The run finished and an MR/PR is waiting for review: one message with its URL
# and the CI result. Skipped with NOTIFY_ON=fail. $1 = URL (or a view command).
notify_done() {
  local url="$1" what=MR ci
  [[ "$NOTIFY_ON" == "all" ]] || return 0
  [[ "${FORGE:-}" != "gh" ]] || what=PR
  case "$CI_OUTCOME" in
    passed)       ci="CI green" ;;
    none)         ci="no CI pipeline found" ;;
    unrelated)    ci="CI red, judged unrelated: check it" ;;
    ended:*)      ci="CI ended as '${CI_OUTCOME#ended:}': check it" ;;
    *)            ci="CI not watched" ;;
  esac
  # URL before the branch: notify() cuts long messages and the URL is the point.
  notify "$(basename "$REPO_ROOT"): $what ready for review, $ci. $url (${BRANCH:-run})"
}

if [[ $NOTIFY_TEST -eq 1 ]]; then
  notify "orchestrate-fix test notification from $(basename "$REPO_ROOT") at $(date +%H:%M)"
  echo "Sent. NOTIFY_CMD=${NOTIFY_CMD:-(not set)}, NOTIFY_CLAUDE=$NOTIFY_CLAUDE, NOTIFY_ON=$NOTIFY_ON."
  echo "If nothing arrived on your phone, the Claude route is not connected (Remote Control); set NOTIFY_CMD." >&2
  exit 0
fi

# Forge: where the repository is hosted decides which CLI opens the MR/PR and
# reads its CI. FORGE=gh|glab forces it; otherwise a github.com origin means
# gh and anything else glab (GitHub Enterprise: set FORGE=gh).
case "${FORGE:-}" in
  gh|glab) ;;
  "")
    case "$(git -C "$REPO_ROOT" remote get-url origin 2>/dev/null || true)" in
      *github.com[:/]*) FORGE=gh ;;
      *) FORGE=glab ;;
    esac
    ;;
  *) echo "FORGE must be gh or glab: '$FORGE'" >&2; exit 64 ;;
esac
# gh cannot always tell which remote is "the" repository (it prompts when there
# are several), so name it from origin: --repo owner/repo (HOST/owner/repo off github.com).
GH_REPO_ARGS=()
if [[ "$FORGE" == "gh" ]]; then
  origin_url="$(git -C "$REPO_ROOT" remote get-url origin 2>/dev/null || true)"
  if [[ "$origin_url" =~ ^([a-z+]+://)?([^@/]+@)?([^:/]+)(:[0-9]+)?[:/]+([^/]+/[^/]+)$ ]]; then
    gh_slug="${BASH_REMATCH[5]%.git}"
    [[ "${BASH_REMATCH[3]}" == "github.com" ]] || gh_slug="${BASH_REMATCH[3]}/$gh_slug"
    GH_REPO_ARGS=(--repo "$gh_slug")
  fi
  unset origin_url gh_slug
fi
if [[ "$FORGE" == "gh" ]]; then FORGE_NOUN="pull request"; else FORGE_NOUN="merge request"; fi

for bin in git "$FORGE" claude; do
  command -v "$bin" >/dev/null || { echo "missing required command: $bin" >&2; exit 69; }
done
# Fail now, not after the whole implement session, when gh is not logged in.
if [[ "$FORGE" == "gh" ]] && ! gh auth status >/dev/null 2>&1; then
  echo "gh is not authenticated; run: gh auth login" >&2
  exit 69
fi

forge_view_hint() { # forge_view_hint <branch>: command that opens the MR/PR
  if [[ "$FORGE" == "gh" ]]; then echo "gh pr view $1 --web"; else echo "glab mr view $1 --web"; fi
}

# Default target branch: origin's HEAD, else the first of main/master/develop.
if [[ -z "$BASE_BRANCH" ]]; then
  BASE_BRANCH="$(git -C "$REPO_ROOT" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null || true)"
  BASE_BRANCH="${BASE_BRANCH#origin/}"
  if [[ -z "$BASE_BRANCH" ]]; then
    for candidate in main master develop; do
      if git -C "$REPO_ROOT" show-ref --verify --quiet "refs/heads/$candidate"; then
        BASE_BRANCH="$candidate"
        break
      fi
    done
  fi
  if [[ -z "$BASE_BRANCH" ]]; then
    echo "could not detect the default branch; pass -t <target-branch>" >&2
    exit 64
  fi
fi

git -C "$REPO_ROOT" rev-parse --verify --quiet "refs/heads/$BASE_BRANCH" >/dev/null \
  || { echo "target branch not found locally: $BASE_BRANCH" >&2; exit 64; }

wt_branch_at() { # path of the worktree that has refs/heads/$1 checked out, if any
  git -C "$REPO_ROOT" worktree list --porcelain | awk -v b="refs/heads/$1" '
    /^worktree /{p=substr($0,10)} $0=="branch " b {print p; exit}'
}
wt_registered() { # is $1 a registered worktree path?
  git -C "$REPO_ROOT" worktree list --porcelain | grep -Fxq "worktree $1"
}

# After a Claude-resolved rebase, check that nothing was lost. Compared with the
# pre-rebase tip $3 (merge base with $2 = target):
#   - files outside the feature's own file set must be identical to the target
#   - lines removed relative to the target must have been removed by the
#     feature itself (otherwise target code was dropped)
#   - lines the feature added must still be present in the file
# Exits 1 with a report when something is off; the branch (rebased) and the
# backup ref are left for review.
verify_resolution() {
  local dir="$1" upstream="$2" oldtip="$3" backup="$4" report
  log "Verifying the conflict resolution against the old tip"
  report="$(python3 - "$dir" "$upstream" "$oldtip" "$BRANCH" <<'PYVERIFY'
import subprocess, sys

d, upstream, oldtip, branch = sys.argv[1:5]

def git(*a):
    return subprocess.run(["git", "-C", d, *a], capture_output=True, text=True, errors="replace").stdout

def names(a, b):
    return {n for n in git("diff", "--no-renames", "--name-only", a, b).splitlines() if n}

def lines(a, b, f, sign):
    out = set()
    for l in git("diff", "--no-renames", "-U0", a, b, "--", f).splitlines():
        if l.startswith(sign) and not l.startswith(sign * 3) and l[1:].strip():
            out.add(l[1:].strip())
    return out

mb = git("merge-base", upstream, oldtip).strip()
feature_files = names(mb, oldtip)
new_files = names(upstream, branch)
problems = []

for f in sorted(new_files - feature_files):
    problems.append(f"{f}: changed relative to {upstream} but not part of the feature")

for f in sorted(feature_files):
    removed_new = lines(upstream, branch, f, "-")
    removed_feat = lines(mb, oldtip, f, "-")
    lost_main = sorted(removed_new - removed_feat)
    if lost_main:
        problems.append(f"{f}: {len(lost_main)} line(s) from {upstream} are gone, e.g. {lost_main[0][:100]!r}")
    present = {l.strip() for l in git("show", f"{branch}:{f}").splitlines()}
    lost_feat = sorted(lines(mb, oldtip, f, "+") - present)
    if lost_feat:
        problems.append(f"{f}: {len(lost_feat)} line(s) added by the feature are gone, e.g. {lost_feat[0][:100]!r}")

print("\n".join(problems))
PYVERIFY
)"
  if [[ -n "$report" ]]; then
    log "The conflict resolution looks lossy; no MR created."
    sed 's/^/  - /' <<<"$report" >&2
    {
      echo "Review in $dir:  git diff $backup $BRANCH   (backup = the tip before the rebase)"
      echo "Restore it:      git -C $dir reset --hard $backup"
      echo "If it is fine:   $0 --mr $BRANCH"
    } >&2
    exit 1
  fi
  echo "Resolution verified: nothing from $upstream or from the feature was dropped."
}

# Rebase BRANCH onto the target branch so conflicts surface (and get fixed)
# before the MR exists. $1 = directory where BRANCH is checked out. Sets
# REBASED=1 when history was rewritten. On conflicts a Claude session resolves
# them; if that does not finish the rebase it is left in progress for you.
REBASED=0
IMPLEMENT_SESSION_ID="" # id of this run's implement session; rebase and CI fixes resume it
rebase_onto_base() {
  local dir="$1" upstream="$BASE_BRANCH" gitdir
  [[ $REBASE -eq 1 ]] || return 0
  state_set stage=rebase

  if git -C "$dir" remote get-url origin >/dev/null 2>&1; then
    git -C "$dir" fetch --quiet origin "$BASE_BRANCH" \
      || echo "warning: could not fetch origin/$BASE_BRANCH; rebasing onto what is local" >&2
    if git -C "$dir" rev-parse --verify --quiet "refs/remotes/origin/$BASE_BRANCH" >/dev/null; then
      upstream="origin/$BASE_BRANCH"
    fi
  fi

  if git -C "$dir" merge-base --is-ancestor "$upstream" "$BRANCH"; then
    echo "$BRANCH is already up to date with $upstream"
    return 0
  fi

  # Safety net: the pre-rebase tip stays reachable under a backup ref.
  local oldtip backup="refs/orchestrate-backup/$BRANCH"
  oldtip="$(git -C "$dir" rev-parse "$BRANCH")"
  git -C "$dir" update-ref "$backup" "$oldtip"

  log "Rebasing $BRANCH onto $upstream (backup of the old tip: $backup)"
  if git -C "$dir" rebase "$upstream"; then
    REBASED=1
    return 0
  fi

  log "Rebase conflicts; asking Claude to resolve them"
  gitdir="$(git -C "$dir" rev-parse --absolute-git-dir)"
  local done_file="$gitdir/orchestrate-rebase-done" prompt
  rm -f "$done_file"
  prompt="A rebase of branch $BRANCH onto $upstream stopped on conflicts in this worktree. Resolve them: inspect \`git status\` and the conflicting hunks, keep the intent of both sides, \`git add\` the resolved files and run \`git rebase --continue\` until the rebase has finished. Then run the tests that cover the files that were in conflict and fix any breakage caused by the merge (amend the relevant commit or add a fix commit). Do not drop anything: every line from main ($upstream) and every line of this feature must survive unless the two are truly contradictory, in which case combine them. Do not modify files that are not in conflict. Never run \`git rebase --abort\`, \`git reset\`, \`git checkout -- <file>\`, \`git push\` or create a merge request. The script verifies the result against the old tip afterwards."
  if [[ $AUTO_EXIT -eq 1 ]]; then
    prompt="$prompt Final step, only when the rebase is finished and the tests pass: run \`touch $done_file\`."
    (
      while [[ ! -e "$done_file" ]]; do sleep 2; done
      sleep 5
      pkill -TERM -f -- "--name ${BRANCH//./\\.}-rebase( |\$)" || true
    ) &
    WATCHER=$!
  fi
  # Resume the implement session so Claude knows the intent of the feature's
  # changes; fall back to a fresh session when there is none (--mr mode).
  local resume_args=()
  [[ -z "$IMPLEMENT_SESSION_ID" ]] || resume_args=(--resume "$IMPLEMENT_SESSION_ID")
  (cd "$dir" && claude ${MODEL_ARGS[@]+"${MODEL_ARGS[@]}"} ${COMPACT_ARGS[@]+"${COMPACT_ARGS[@]}"} \
    ${resume_args[@]+"${resume_args[@]}"} ${RC_ON:+"--remote-control=$BRANCH-rebase"} \
    --name "$BRANCH-rebase" "$prompt") || true
  if [[ -n "$WATCHER" ]]; then
    kill "$WATCHER" 2>/dev/null || true
    wait "$WATCHER" 2>/dev/null || true
    WATCHER=""
  fi

  if [[ -d "$gitdir/rebase-merge" || -d "$gitdir/rebase-apply" ]] \
     || ! git -C "$dir" merge-base --is-ancestor "$upstream" "$BRANCH" \
     || [[ -n "$(git -C "$dir" status --porcelain)" ]]; then
    log "Rebase onto $upstream is not finished; no MR created."
    echo "Resolve it in $dir (git status; git rebase --continue), then run: $0 --mr $BRANCH" >&2
    exit 1
  fi
  verify_resolution "$dir" "$upstream" "$oldtip" "$backup"
  REBASED=1
}

# Local gate: what must pass before the branch is pushed. Runs in the directory
# where BRANCH is checked out, with GATE_BASE = the ref the branch sits on.
# $GATE_CMD wins; otherwise every stack found at the repository root is gated
# with its own checks. All steps run, so one report lists every failure:
#   Python (uv)   Django (src/manage.py): makemigrations check + the full
#                 pytest suite, from src/ when pytest is configured there, as
#                 CI does (GATE_SCOPE=changed: only the changed apps).
#                 Other uv projects with pytest: pytest.
#   Node          package.json: typecheck (a `typecheck`/`type-check` script,
#                 else tsc --noEmit when tsconfig.json and a local tsc exist)
#                 and the `test` script, with the package manager the lockfile
#                 names and CI=1 set. GATE_BUILD=1 adds the `build` script.
#   Rust          Cargo.toml: cargo test --workspace
# GATE_LINT=1 adds the linters: ruff on changed files, the `lint` script,
# cargo fmt --check and clippy. They are opt-in because a project's lint rules
# are often not enforced by its CI and flag pre-existing code. A stack whose
# tool is missing is skipped with a note; nothing detected = nothing to gate,
# which passes.
GATE_RAN=0

node_pm() { # node_pm <dir>: the package manager the lockfile names (npm without one)
  local d="$1"
  if [[ -f "$d/pnpm-lock.yaml" ]]; then
    echo pnpm
  elif [[ -f "$d/yarn.lock" ]]; then
    echo yarn
  elif [[ -f "$d/bun.lockb" || -f "$d/bun.lock" ]]; then
    echo bun
  else
    echo npm
  fi
}

node_scripts() { # names of the real package.json scripts (not npm's "no test specified")
  python3 -I - "${1:-package.json}" <<'PYNODE' 2>/dev/null || true
import json, sys

try:
    with open(sys.argv[1]) as f:
        scripts = json.load(f).get("scripts") or {}
except (OSError, ValueError):
    scripts = {}
for name, cmd in scripts.items():
    if isinstance(cmd, str) and "no test specified" not in cmd:
        print(name)
PYNODE
}

gate_python() { # gate_python <changed files, one per line>
  local changed="$1" rc=0 f app pyfiles=() apps=()
  if ! command -v uv >/dev/null; then
    [[ ! -f src/manage.py && ! -f uv.lock ]] || echo "note: uv not found; skipping the Python checks" >&2
    return 0
  fi
  if [[ -f src/manage.py ]]; then
    GATE_RAN=1
    echo "+ uv run python src/manage.py makemigrations --check --dry-run"
    uv run python src/manage.py makemigrations --check --dry-run || rc=1
    if [[ "${GATE_SCOPE:-full}" == "changed" ]]; then
      while IFS= read -r f; do
        [[ "$f" =~ ^src/([^/]+)/ ]] || continue
        app="src/${BASH_REMATCH[1]}"
        [[ -d "$app/tests" ]] || continue
        [[ " ${apps[*]-} " == *" $app "* ]] || apps+=("$app")
      done <<<"$changed"
      if [[ ${#apps[@]} -gt 0 ]]; then
        echo "+ uv run pytest ${apps[*]}"
        uv run pytest "${apps[@]}" || rc=1
      fi
    elif ! grep -qs '^\[tool\.pytest' pyproject.toml \
         && [[ -f src/pytest.ini || -f src/tox.ini || -f src/setup.cfg || -f src/conftest.py ]]; then
      # pytest is configured under src/ (CI runs it from there).
      echo "+ (cd src && uv run pytest)"
      (cd src && uv run pytest) || rc=1
    else
      echo "+ uv run pytest"
      uv run pytest || rc=1
    fi
  elif [[ -f uv.lock ]] && { grep -qs '^\[tool\.pytest' pyproject.toml || [[ -f pytest.ini || -d tests ]]; }; then
    GATE_RAN=1
    echo "+ uv run pytest"
    uv run pytest || rc=1
  fi
  if [[ "${GATE_LINT:-0}" == "1" ]] \
     && { grep -qsE '^\[tool\.ruff' pyproject.toml || [[ -f ruff.toml || -f .ruff.toml ]]; }; then
    while IFS= read -r f; do
      [[ "$f" == *.py ]] && pyfiles+=("$f")
    done <<<"$changed"
    if [[ ${#pyfiles[@]} -gt 0 ]]; then
      GATE_RAN=1
      echo "+ uv run ruff check <${#pyfiles[@]} changed file(s)>"
      uv run ruff check "${pyfiles[@]}" || rc=1
    fi
  fi
  return $rc
}

gate_node() {
  local rc=0 pm scripts name steps=()
  [[ -f package.json ]] || return 0
  pm="$(node_pm .)"
  if ! command -v "$pm" >/dev/null; then
    echo "note: $pm not found; skipping the Node checks" >&2
    return 0
  fi
  if [[ ! -d node_modules && ! -f .pnp.cjs ]]; then
    echo "note: no node_modules in $PWD; skipping the Node checks (install the dependencies first)" >&2
    return 0
  fi
  scripts=" $(node_scripts package.json | tr '\n' ' ') "
  export CI=1 # test runners (vitest, jest, ...) must not start in watch mode
  if [[ "$scripts" == *" typecheck "* ]]; then
    steps+=(typecheck)
  elif [[ "$scripts" == *" type-check "* ]]; then
    steps+=(type-check)
  elif [[ -f tsconfig.json && -x node_modules/.bin/tsc ]]; then
    GATE_RAN=1
    echo "+ node_modules/.bin/tsc --noEmit"
    node_modules/.bin/tsc --noEmit || rc=1
  fi
  if [[ "${GATE_LINT:-0}" == "1" && "$scripts" == *" lint "* ]]; then steps+=(lint); fi
  if [[ "$scripts" == *" test "* ]]; then steps+=(test); fi
  if [[ "${GATE_BUILD:-0}" == "1" && "$scripts" == *" build "* ]]; then steps+=(build); fi
  for name in ${steps[@]+"${steps[@]}"}; do
    GATE_RAN=1
    echo "+ $pm run $name"
    "$pm" run "$name" || rc=1
  done
  return $rc
}

gate_rust() {
  local rc=0
  [[ -f Cargo.toml ]] || return 0
  if ! command -v cargo >/dev/null; then
    echo "note: cargo not found; skipping the Rust checks" >&2
    return 0
  fi
  GATE_RAN=1
  echo "+ cargo test --workspace"
  cargo test --workspace || rc=1
  if [[ "${GATE_LINT:-0}" == "1" ]]; then
    echo "+ cargo fmt --all -- --check"
    cargo fmt --all -- --check || rc=1
    echo "+ cargo clippy --workspace --all-targets -- -D warnings"
    cargo clippy --workspace --all-targets -- -D warnings || rc=1
  fi
  return $rc
}

gate_default() {
  local rc=0 changed
  GATE_RAN=0
  changed="$(git diff --name-only --diff-filter=d "$GATE_BASE"...HEAD)"
  gate_python "$changed" || rc=1
  gate_node || rc=1
  gate_rust || rc=1
  [[ $GATE_RAN -eq 1 ]] || echo "no local gate detected (set GATE_CMD to define one)"
  return $rc
}

# Run the gate once, output also saved to $3. $1 = directory, $2 = base ref.
gate_run() {
  local dir="$1" base="$2" out="$3" rc
  set +e
  (
    cd "$dir" || exit 1
    export GATE_BASE="$base"
    if [[ -n "${GATE_CMD:-}" ]]; then
      bash -c "$GATE_CMD"
    else
      gate_default
    fi
  ) 2>&1 | tee "$out"
  rc=${PIPESTATUS[0]}
  set -e
  return "$rc"
}

# Resume the implement session (fresh one when there is none) with $3 as the
# prompt and wait for it. $1 = directory, $2 = session name suffix. Closes the
# session by itself when AUTO_EXIT is on: the prompt must end in a `touch $4`.
run_fix_session() {
  local dir="$1" suffix="$2" prompt="$3" done_file="$4" resume_args=()
  rm -f "$done_file"
  if [[ $AUTO_EXIT -eq 1 ]]; then
    prompt="$prompt Final step, once everything is committed (or nothing needs changing): run \`touch $done_file\`."
    (
      while [[ ! -e "$done_file" ]]; do sleep 2; done
      sleep 5
      pkill -TERM -f -- "--name ${BRANCH//./\\.}-$suffix( |\$)" || true
    ) &
    WATCHER=$!
  fi
  [[ -z "$IMPLEMENT_SESSION_ID" ]] || resume_args=(--resume "$IMPLEMENT_SESSION_ID")
  (cd "$dir" && claude ${MODEL_ARGS[@]+"${MODEL_ARGS[@]}"} ${COMPACT_ARGS[@]+"${COMPACT_ARGS[@]}"} \
    ${resume_args[@]+"${resume_args[@]}"} ${RC_ON:+"--remote-control=$BRANCH-$suffix"} \
    --name "$BRANCH-$suffix" "$prompt") || true
  if [[ -n "$WATCHER" ]]; then
    kill "$WATCHER" 2>/dev/null || true
    wait "$WATCHER" 2>/dev/null || true
    WATCHER=""
  fi
}

# Gate $1 (directory with BRANCH checked out) before pushing. On failure a
# Claude session fixes it, up to GATE_FIX_ATTEMPTS times (never in --mr mode,
# where the branch may not be the one being worked on). Returns 1 when the gate
# is still red; the caller keeps the worktree.
local_gate() {
  local dir="$1" attempt=0 base="$BASE_BRANCH" gitdir logs done_file prompt before
  [[ $GATE -eq 1 ]] || return 0
  if git -C "$dir" rev-parse --verify --quiet "refs/remotes/origin/$BASE_BRANCH" >/dev/null; then
    base="origin/$BASE_BRANCH"
  fi
  gitdir="$(git -C "$dir" rev-parse --absolute-git-dir)"
  logs="$gitdir/orchestrate-gate-log.txt"
  done_file="$gitdir/orchestrate-gate-done"

  while :; do
    state_set stage=gate "gate_attempt=$attempt"
    log "Local gate (fix attempts used: $attempt/$GATE_FIX_ATTEMPTS)"
    if gate_run "$dir" "$base" "$logs"; then
      echo "Local gate passed."
      return 0
    fi
    if [[ $MR_ONLY -eq 1 || $attempt -ge $GATE_FIX_ATTEMPTS ]]; then
      echo "Local gate output: $logs" >&2
      return 1
    fi
    attempt=$((attempt + 1))
    log "Local gate failed; asking Claude to fix it (attempt $attempt/$GATE_FIX_ATTEMPTS)"
    before="$(git -C "$dir" rev-parse HEAD)"
    prompt="The local pre-push gate for branch $BRANCH failed. Its full output is in $logs. Find the cause of each failure in this worktree, reproduce it where you can, and fix it with the smallest change that fits the existing code; commit the fix. Never run \`git push\`, \`git rebase\`, \`git reset\` or create a merge request; the orchestration script pushes."
    run_fix_session "$dir" "gate" "$prompt" "$done_file"
    if [[ -n "$(git -C "$dir" status --porcelain)" ]]; then
      log "Uncommitted changes after the gate fix session; not pushing."
      return 1
    fi
    if [[ "$(git -C "$dir" rev-parse HEAD)" == "$before" ]]; then
      log "Claude made no changes for the failed gate."
      return 1
    fi
  done
}

# Push BRANCH, have Claude draft the MR message, create the MR with glab.
# $1 = directory to run in, $2 = text for the "kept" hint (may be empty).
# Sets MR_URL. Exits 1 (via keep) on any failure.
create_mr() {
  local dir="$1" kept_hint="$2"
  local range="$BASE_BRANCH..$BRANCH"

  keep() {
    log "$1"
    [[ -n "$kept_hint" ]] && echo "$kept_hint" >&2
    exit 1
  }

  if [[ -n "$(git -C "$dir" branch --show-current)" && "$(git -C "$dir" branch --show-current)" == "$BRANCH" ]]; then
    rebase_onto_base "$dir"
  else
    # BRANCH is not what $dir has checked out; rebase where it is, if anywhere.
    local rdir
    rdir="$(wt_branch_at "$BRANCH")"
    if [[ -n "$rdir" ]]; then
      rebase_onto_base "$rdir"
    elif [[ $REBASE -eq 1 ]]; then
      echo "note: $BRANCH is not checked out anywhere; skipping rebase onto $BASE_BRANCH" >&2
    fi
  fi

  local commits
  commits="$(git -C "$dir" rev-list --count "$range")"
  if [[ "$commits" -eq 0 ]]; then
    keep "No commits on $BRANCH beyond $BASE_BRANCH; not creating an MR."
  fi

  # Local gate in the directory where BRANCH is checked out, after the rebase.
  local gdir=""
  if [[ "$(git -C "$dir" branch --show-current)" == "$BRANCH" ]]; then
    gdir="$dir"
  else
    gdir="$(wt_branch_at "$BRANCH")"
  fi
  if [[ -n "$gdir" ]]; then
    local_gate "$gdir" || keep "Local gate failed; not pushing."
    commits="$(git -C "$dir" rev-list --count "$range")"
  elif [[ $GATE -eq 1 ]]; then
    echo "note: $BRANCH is not checked out anywhere; skipping the local gate" >&2
  fi

  state_set stage=push
  log "Pushing $BRANCH"
  if [[ $REBASED -eq 1 ]]; then
    # History was rewritten; an earlier push of this branch must be replaced.
    git -C "$dir" push -u --force-with-lease origin "$BRANCH" || keep "Push failed."
  else
    git -C "$dir" push -u origin "$BRANCH" || keep "Push failed."
  fi

  local title
  title="$(git -C "$dir" log --reverse --format=%s "$range" | head -n 1)"
  DESC_FILE="$(mktemp)"

  # Ask Claude (print mode, no tools) to write the MR title/description from
  # the commits. Falls back to the plain commit list if that fails.
  log "Drafting MR message from $commits commit(s)"
  local mr_prompt
  mr_prompt="$(
    cat <<'TXT'
Write a pull/merge request message from the commits and diffstat below.
Output format, nothing else (no code fences, no preamble):
line 1: a concise title, imperative mood, max 72 characters
line 2: blank
line 3+: a short markdown description - a summary paragraph, then bullet
points for notable changes. Do not list commits one by one and do not add
an issue-closing line.

TXT
    printf '## Commits\n'
    git -C "$dir" log --reverse --format='### %s%n%b' "$range"
    printf '\n## Diffstat\n'
    git -C "$dir" diff --stat "$BASE_BRANCH...$BRANCH"
  )"
  local generated gen_title gen_body
  generated="$(cd "$dir" && claude -p ${MODEL_ARGS[@]+"${MODEL_ARGS[@]}"} --tools "" --no-session-persistence \
    --disable-slash-commands --strict-mcp-config <<<"$mr_prompt" 2>/dev/null || true)"
  gen_title="$(head -n 1 <<<"$generated" | sed -e 's/^#* *//' -e 's/[[:space:]]*$//')"
  gen_body="$(tail -n +2 <<<"$generated" | sed '1{/^[[:space:]]*$/d;}')"

  if [[ -n "$gen_title" && ${#gen_title} -le 100 && -n "$gen_body" ]]; then
    title="$gen_title"
    printf '%s\n' "$gen_body" > "$DESC_FILE"
  else
    echo "warning: could not generate MR message, using commit list" >&2
    git -C "$dir" log --reverse --format='- %s%n%n%b' "$range" > "$DESC_FILE"
  fi
  if [[ -n "$ISSUE" ]]; then
    printf '\nCloses #%s\n' "$ISSUE" >> "$DESC_FILE"
  fi

  # Assignee / reviewers are set with GitLab quick actions in the description
  # (/assign, /assign_reviewer): the server applies them, so no user lookup is
  # needed and a token without the "User: Read" scope still works. The flags
  # --assignee/--reviewer of glab resolve usernames through /users and fail
  # with 403 on such tokens. Default assignee is yourself ("/assign me").
  local assignee="${ASSIGNEE:-me}"
  local reviewer="$REVIEWER"
  quick_users() { # "a,@b" -> "@a @b"; "me" stays as is
    local out="" u
    for u in ${1//,/ }; do
      u="${u#@}"
      [[ -n "$u" ]] || continue
      if [[ "$u" == "me" ]]; then out+="me "; else out+="@$u "; fi
    done
    printf '%s' "${out% }"
  }
  local actions=""
  if [[ "$FORGE" == "glab" ]]; then
    if [[ "$assignee" != "none" ]]; then
      actions+="/assign $(quick_users "$assignee")"$'\n'
    fi
    if [[ -n "$reviewer" && "$reviewer" != "none" ]]; then
      actions+="/assign_reviewer $(quick_users "$reviewer")"$'\n'
    fi
    if [[ -n "$actions" ]]; then
      printf '\n%s' "$actions" >> "$DESC_FILE"
    fi
  fi

  log "Creating $FORGE_NOUN"
  local mr_out
  if [[ "$FORGE" == "gh" ]]; then
    # GitHub has no quick actions: assignee and reviewer are flags. "me" is
    # "@me"; a login that cannot be assigned makes the create fail, so it is
    # retried once without people, and an already open PR is reused (re-runs).
    gh_users() { # "me,@b" -> "@me,b"
      local out="" u
      for u in ${1//,/ }; do
        u="${u#@}"
        [[ -n "$u" ]] || continue
        if [[ "$u" == "me" ]]; then out+="@me,"; else out+="$u,"; fi
      done
      printf '%s' "${out%,}"
    }
    local people=() list
    if [[ "$assignee" != "none" ]]; then
      list="$(gh_users "$assignee")"
      [[ -z "$list" ]] || people+=(--assignee "$list")
    fi
    if [[ -n "$reviewer" && "$reviewer" != "none" ]]; then
      list="$(gh_users "$reviewer")"
      [[ -z "$list" ]] || people+=(--reviewer "$list")
    fi
    local gh_base=(--head "$BRANCH" --base "$BASE_BRANCH" --title "$title" --body-file "$DESC_FILE")
    if ! mr_out="$(cd "$dir" && gh pr create ${GH_REPO_ARGS[@]+"${GH_REPO_ARGS[@]}"} "${gh_base[@]}" \
         ${people[@]+"${people[@]}"} 2>&1)"; then
      local existing
      existing="$(cd "$dir" && gh pr list ${GH_REPO_ARGS[@]+"${GH_REPO_ARGS[@]}"} --head "$BRANCH" --state open \
        --json url --jq '.[0].url // empty' 2>/dev/null || true)"
      if [[ -n "$existing" ]]; then
        echo "a pull request for $BRANCH is already open"
        mr_out="$existing"
      elif [[ ${#people[@]} -gt 0 ]]; then
        echo "$mr_out" >&2
        echo "warning: gh pr create failed; retrying without assignee/reviewer" >&2
        mr_out="$(cd "$dir" && gh pr create ${GH_REPO_ARGS[@]+"${GH_REPO_ARGS[@]}"} "${gh_base[@]}" 2>&1)" \
          || { echo "$mr_out" >&2; keep "gh pr create failed."; }
      else
        echo "$mr_out" >&2
        keep "gh pr create failed."
      fi
    fi
    echo "$mr_out"
    MR_URL="$(grep -Eo 'https?://[^ ]+/pull/[0-9]+' <<<"$mr_out" | head -n 1 || true)"
  else
    mr_out="$(cd "$dir" && glab mr create \
      --source-branch "$BRANCH" --target-branch "$BASE_BRANCH" \
      --title "$title" --description-file "$DESC_FILE" --yes 2>&1)" \
      || { echo "$mr_out" >&2; keep "glab mr create failed."; }
    echo "$mr_out"
    MR_URL="$(grep -Eo 'https?://[^ ]+/merge_requests/[0-9]+' <<<"$mr_out" | head -n 1 || true)"
  fi
  state_set stage=mr "mr_url=$MR_URL"
}

# Wait for the CI pipeline of commit $2 (run from $1). Sets PIPELINE_ID and
# PIPELINE_STATUS (success, failed, canceled, skipped, ... or "none" when no
# pipeline showed up, "timeout" when it did not finish in time).
PIPELINE_ID=""
PIPELINE_STATUS=""
wait_for_pipeline() {
  local dir="$1" sha="$2" start now line last=""
  start="$(date +%s)"
  PIPELINE_ID=""
  PIPELINE_STATUS=""
  # No workflow at the pushed commit can run for a push to a branch or a pull
  # request (no files, or only tag / manual / scheduled triggers): nothing will
  # ever appear, so do not sit out the two-minute "no pipeline" wait below.
  # Anything unclear counts as "may run" and is waited for.
  if [[ "$FORGE" == "gh" ]] && ! python3 - "$dir" "$sha" <<'PYWF' 2>/dev/null
import re, subprocess, sys

d, sha = sys.argv[1:3]

def git(*a):
    return subprocess.run(["git", "-C", d, *a], capture_output=True, text=True, errors="replace").stdout

try:
    import yaml
except ImportError:
    yaml = None

def may_run(text):
    if yaml is not None:
        try:
            doc = yaml.safe_load(text)
            on = doc.get("on", doc.get(True)) if isinstance(doc, dict) else None
        except Exception:
            return True
        if on is None:
            return True
        if isinstance(on, str):
            on = [on]
        events = on if isinstance(on, dict) else {e: None for e in on}
        if "pull_request" in events or "pull_request_target" in events:
            return True
        if "push" in events:
            cfg = events["push"]
            if isinstance(cfg, dict) and ("tags" in cfg or "tags-ignore" in cfg) \
               and not any(k in cfg for k in ("branches", "branches-ignore", "paths", "paths-ignore")):
                return False
            return True
        return "workflow_run" in events or "merge_group" in events
    # No PyYAML: a text scan with the same rules.
    if re.search(r"pull_request", text):
        return True
    if re.search(r"(?m)^[^#\n]*\bpush\b", text):  # may over-match (a step running git push): then it just waits
        return not (re.search(r"(?m)^\s*tags(-ignore)?\s*:", text)
                    and not re.search(r"(?m)^\s*(branches(-ignore)?|paths(-ignore)?)\s*:", text))
    return bool(re.search(r"workflow_run|merge_group", text))

try:
    files = [f for f in git("ls-tree", "-r", "--name-only", sha, ".github/workflows").splitlines()
             if f.endswith((".yml", ".yaml"))]
    sys.exit(0 if any(may_run(git("show", f"{sha}:{f}")) for f in files) else 1)
except Exception:
    sys.exit(0)  # unclear: wait for the run as before
PYWF
  then
    echo "No workflow in this commit runs on a branch push or pull request; skipping the CI wait."
    PIPELINE_STATUS="none"
    return 0
  fi
  while :; do
    if [[ "$FORGE" == "gh" ]]; then
      # A commit can have several workflow runs. One "pipeline" = all of them:
      # running while any is unfinished, then failed (ids of the failed runs,
      # comma-separated) / canceled / success.
      line="$(cd "$dir" && gh run list ${GH_REPO_ARGS[@]+"${GH_REPO_ARGS[@]}"} --commit "$sha" --limit 30 \
        --json databaseId,status,conclusion 2>/dev/null \
        | python3 -c 'import json,sys
runs = json.load(sys.stdin)
if not runs:
    print("")
else:
    ids = lambda rs: ",".join(str(r["databaseId"]) for r in rs)
    if any(r["status"] != "completed" for r in runs):
        print("%s running" % runs[0]["databaseId"])
    else:
        bad = [r for r in runs if r["conclusion"] in ("failure", "timed_out", "startup_failure")]
        if bad:
            print("%s failed" % ids(bad))
        elif any(r["conclusion"] == "action_required" for r in runs):
            print("%s action_required" % ids(runs))
        elif any(r["conclusion"] == "cancelled" for r in runs):
            print("%s canceled" % ids(runs))
        else:
            print("%s success" % runs[0]["databaseId"])' 2>/dev/null || true)"
    else
      line="$(cd "$dir" && glab api "projects/:id/pipelines?sha=$sha&order_by=id&sort=desc&per_page=1" 2>/dev/null \
        | python3 -c 'import json,sys
d=json.load(sys.stdin)
print("%s %s" % (d[0]["id"], d[0]["status"]) if d else "")' 2>/dev/null || true)"
    fi
    now="$(date +%s)"
    if [[ -z "$line" ]]; then
      # The pipeline is created a moment after the push; no pipeline at all
      # means the project (or this branch) has no CI.
      if (( now - start > 120 )); then PIPELINE_STATUS="none"; return 0; fi
    else
      PIPELINE_ID="${line%% *}"
      PIPELINE_STATUS="${line#* }"
      case "$PIPELINE_STATUS" in
        created|waiting_for_resource|preparing|pending|running|scheduled) ;;
        *) return 0 ;;
      esac
      if [[ "$line" != "$last" ]]; then
        echo "pipeline #$PIPELINE_ID: $PIPELINE_STATUS"
        last="$line"
      fi
    fi
    if (( now - start > CI_WAIT_TIMEOUT )); then PIPELINE_STATUS="timeout"; return 0; fi
    sleep 15
  done
}

# Write the failed jobs of pipeline $2 to file $3: the tail of each job log, and
# the failed tests of any JUnit report among the job's artifacts. Artifacts are
# extracted under $4. Needs the token permissions "Job: Read" (logs) and "Job
# Artifact: Read" (artifacts); without them a warning is printed and the part
# is skipped.
write_failed_job_logs() {
  local dir="$1" pipeline="$2" out="$3" art_root="$4" jobs tmp
  if [[ "$FORGE" == "gh" ]]; then
    # $pipeline = comma-separated ids of the failed workflow runs. Only the log
    # of the failed steps is fetched; artifacts (JUnit reports) are not.
    local run_id
    : > "$out"
    for run_id in ${pipeline//,/ }; do
      printf '===== failed workflow run #%s =====\n' "$run_id" >> "$out"
      if (cd "$dir" && gh run view ${GH_REPO_ARGS[@]+"${GH_REPO_ARGS[@]}"} "$run_id" --log-failed >"${out}.tmp" 2>&1); then
        printf -- '--- last 300 log lines of the failed steps ---\n' >> "$out"
        sed -E 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\r//g' "${out}.tmp" | tail -n 300 >> "$out"
      else
        printf -- '(log not available: %s)\n' "$(head -c 200 "${out}.tmp")" >> "$out"
        echo "warning: could not read the log of run #$run_id (gh auth needs the repo scope)" >&2
      fi
      printf '\n' >> "$out"
    done
    rm -f "${out}.tmp"
    return 0
  fi
  jobs="$(cd "$dir" && glab api "projects/:id/pipelines/$pipeline/jobs?scope[]=failed&per_page=50" 2>/dev/null || echo '[]')"
  : > "$out"
  [[ -n "$art_root" ]] || { echo "write_failed_job_logs: no artifacts directory given" >&2; return 1; }
  rm -rf "$art_root"
  tmp="$(mktemp)"
  local id name has_art warned_log=0 warned_art=0
  while IFS=$'\t' read -r id name has_art; do
    [[ -n "$id" ]] || continue
    printf '===== failed job: %s (#%s) =====\n' "$name" "$id" >> "$out"

    if (cd "$dir" && glab api "projects/:id/jobs/$id/trace" >"$tmp" 2>&1); then
      printf -- '--- last 200 log lines ---\n' >> "$out"
      sed -E 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\r//g' "$tmp" | tail -n 200 >> "$out"
    else
      printf -- '(log not available: %s)\n' "$(head -c 200 "$tmp")" >> "$out"
      if [[ $warned_log -eq 0 ]]; then
        warned_log=1
        echo "warning: could not read the job log of #$id; the token likely lacks the 'Job: Read' permission. The fix session will have no log output." >&2
      fi
    fi

    if [[ "$has_art" == "1" ]]; then
      mkdir -p "$art_root/$id"
      if (cd "$dir" && glab api "projects/:id/jobs/$id/artifacts" >"$art_root/$id.zip" 2>"$tmp"); then
        python3 -I - "$art_root/$id.zip" "$art_root/$id" >> "$out" <<'PYART' || echo "warning: could not unpack the artifacts of job #$id" >&2
import sys, zipfile, os
import xml.etree.ElementTree as ET

zpath, dest = sys.argv[1:3]
with zipfile.ZipFile(zpath) as z:
    root = os.path.realpath(dest)
    for m in z.infolist():
        if not os.path.realpath(os.path.join(root, m.filename)).startswith(root + os.sep):
            continue  # path traversal guard
        z.extract(m, dest)
names = [os.path.relpath(os.path.join(d, f), dest) for d, _, fs in os.walk(dest) for f in fs]
print("--- artifacts extracted to %s (%d files) ---" % (dest, len(names)))
for n in sorted(names)[:30]:
    print("  " + n)
shown = 0
for n in sorted(names):
    if not n.endswith(".xml"):
        continue
    try:
        tree = ET.parse(os.path.join(dest, n))
    except ET.ParseError:
        continue
    for case in tree.iter("testcase"):
        bad = [c for c in case if c.tag in ("failure", "error")]
        if not bad or shown >= 40:
            continue
        shown += 1
        msg = (bad[0].get("message") or bad[0].text or "").strip().splitlines()
        print("FAILED %s::%s - %s" % (case.get("classname", ""), case.get("name", ""), (msg[0] if msg else "")[:300]))
if shown:
    print("--- %d failed test(s) from JUnit reports (max 40 shown) ---" % shown)
PYART
      else
        rm -f "$art_root/$id.zip"
        rmdir "$art_root/$id" 2>/dev/null || true
        printf -- '(artifacts not available: %s)\n' "$(head -c 200 "$tmp")" >> "$out"
        if [[ $warned_art -eq 0 ]]; then
          warned_art=1
          echo "note: job #$id has artifacts but they could not be downloaded (the token likely lacks 'Job Artifact: Read'); skipping artifacts." >&2
        fi
      fi
    fi
    printf '\n' >> "$out"
  done < <(python3 -c 'import json,sys
for j in json.load(sys.stdin):
    arts = [a for a in (j.get("artifacts") or []) if a.get("file_type") != "trace"]
    print("%s\t%s\t%d" % (j["id"], j["name"], 1 if arts else 0))' <<<"$jobs" 2>/dev/null || true)
  rm -f "$tmp"
}

# Watch the MR's full CI pipeline. On failure a Claude session reads the failed
# job logs, fixes the issues, commits; the fix is pushed and the new pipeline
# watched again, at most CI_FIX_ATTEMPTS times. The fix runs in the resumed
# implement session, so Claude keeps the context of the change. Exits 1 (worktree kept) when the
# pipeline is still not green. Returns 0 when Claude finds the failures
# unrelated to the branch (it commits nothing and records the reason): the MR
# is left with its red pipeline and the worktree is cleaned up. $1 = worktree directory.
ci_check() {
  local dir="$1" attempt=0 sha gitdir logs artifacts done_file unrelated_file prompt
  [[ $CI_CHECK -eq 1 ]] || return 0
  gitdir="$(git -C "$dir" rev-parse --absolute-git-dir)"
  logs="$gitdir/orchestrate-ci-logs.txt"
  artifacts="$gitdir/orchestrate-ci-artifacts"
  done_file="$gitdir/orchestrate-ci-done"
  unrelated_file="$gitdir/orchestrate-ci-unrelated"

  while :; do
    sha="$(git -C "$dir" rev-parse HEAD)"
    state_set stage=ci "ci_attempt=$attempt"
    log "Waiting for the CI pipeline of ${sha:0:8}"
    wait_for_pipeline "$dir" "$sha"
    case "$PIPELINE_STATUS" in
      success)
        echo "CI pipeline #$PIPELINE_ID passed."
        CI_OUTCOME="passed"
        return 0
        ;;
      none)
        echo "warning: no CI pipeline found for ${sha:0:8}; skipping the CI check" >&2
        CI_OUTCOME="none"
        return 0
        ;;
      timeout)
        log "CI pipeline #$PIPELINE_ID did not finish within ${CI_WAIT_TIMEOUT}s."
        echo "Worktree kept: $dir (branch $BRANCH)" >&2
        exit 1
        ;;
      failed) ;;
      *)
        echo "CI pipeline #$PIPELINE_ID ended as '$PIPELINE_STATUS'; not fixing that." >&2
        CI_OUTCOME="ended:$PIPELINE_STATUS"
        return 0
        ;;
    esac

    if (( attempt >= CI_FIX_ATTEMPTS )); then
      log "CI pipeline #$PIPELINE_ID still fails after $attempt fix attempt(s)."
      echo "Worktree kept: $dir (branch $BRANCH)" >&2
      exit 1
    fi
    attempt=$((attempt + 1))
    state_set stage=ci "ci_attempt=$attempt" "pipeline_id=$PIPELINE_ID"

    log "CI pipeline #$PIPELINE_ID failed; asking Claude to fix it (attempt $attempt/$CI_FIX_ATTEMPTS)"
    write_failed_job_logs "$dir" "$PIPELINE_ID" "$logs" "$artifacts"
    rm -f "$done_file" "$unrelated_file"
    prompt="The full CI pipeline (#$PIPELINE_ID) of the merge request for branch $BRANCH failed. The tail of every failed job's log, plus the failed tests of any JUnit report, is in $logs; job artifacts, where available, are extracted under $artifacts (the logs file lists them). Read it, find the cause of each failure in this worktree, reproduce it locally where you can (run the failing tests or checks) and fix it with the smallest change that fits the existing code. If a failure is unrelated to this branch (infrastructure, a flaky job, a test that also fails on $BASE_BRANCH), do not change code for it. When every failure is unrelated, commit nothing and write one line per failure to $unrelated_file saying why it is unrelated (and the evidence, e.g. the same test fails on $BASE_BRANCH). Otherwise commit the fixes. Never run \`git push\`, \`git rebase\`, \`git reset\` or create a merge request; the orchestration script pushes."
    if [[ $AUTO_EXIT -eq 1 ]]; then
      prompt="$prompt Final step, once everything is committed (or nothing needs changing): run \`touch $done_file\`."
      (
        while [[ ! -e "$done_file" ]]; do sleep 2; done
        sleep 5
        pkill -TERM -f -- "--name ${BRANCH//./\\.}-ci( |\$)" || true
      ) &
      WATCHER=$!
    fi
    # Resume the implement session so Claude still has the context of what
    # was built and why; fall back to a fresh session when there is none.
    local resume_args=()
    [[ -z "$IMPLEMENT_SESSION_ID" ]] || resume_args=(--resume "$IMPLEMENT_SESSION_ID")
    (cd "$dir" && claude ${MODEL_ARGS[@]+"${MODEL_ARGS[@]}"} ${COMPACT_ARGS[@]+"${COMPACT_ARGS[@]}"} \
      ${resume_args[@]+"${resume_args[@]}"} ${RC_ON:+"--remote-control=$BRANCH-ci"} \
      --name "$BRANCH-ci" "$prompt") || true
    if [[ -n "$WATCHER" ]]; then
      kill "$WATCHER" 2>/dev/null || true
      wait "$WATCHER" 2>/dev/null || true
      WATCHER=""
    fi

    if [[ -n "$(git -C "$dir" status --porcelain)" ]]; then
      log "Uncommitted changes after the CI fix session; not pushing."
      echo "Worktree kept: $dir (branch $BRANCH)" >&2
      exit 1
    fi
    if [[ "$(git -C "$dir" rev-parse HEAD)" == "$sha" && -s "$unrelated_file" ]]; then
      log "CI pipeline #$PIPELINE_ID failed for reasons unrelated to this branch; leaving the MR as it is."
      sed 's/^/  - /' "$unrelated_file"
      # Tell the reviewer why the pipeline is red. Not fatal: a token without
      # permission to comment only costs the note.
      local note
      note="$(printf 'The CI pipeline #%s failed for reasons that look unrelated to this change (assessed automatically after the pipeline ran):\n\n' "$PIPELINE_ID"; sed 's/^/- /' "$unrelated_file")"
      if [[ "$FORGE" == "gh" ]]; then
        (cd "$dir" && gh pr comment ${GH_REPO_ARGS[@]+"${GH_REPO_ARGS[@]}"} "$BRANCH" --body "$note" >/dev/null) \
          || echo "warning: could not post the note on the PR (token needs permission to comment)" >&2
      else
        (cd "$dir" && glab mr note create "$BRANCH" --resolvable=false -m "$note" >/dev/null) \
          || echo "warning: could not post the note on the MR (token needs permission to comment)" >&2
      fi
      CI_OUTCOME="unrelated"
      return 0
    fi
    if [[ "$(git -C "$dir" rev-parse HEAD)" == "$sha" ]]; then
      log "Claude made no changes for the failed pipeline #$PIPELINE_ID."
      echo "Worktree kept: $dir (branch $BRANCH)" >&2
      exit 1
    fi
    log "Pushing the CI fix"
    git -C "$dir" push origin "$BRANCH" \
      || { echo "Push failed. Worktree kept: $dir (branch $BRANCH)" >&2; exit 1; }
  done
}

# Per-worktree databases -----------------------------------------------------
WORKTREE_DB_VARS_EXPLICIT="${WORKTREE_DB_VARS+1}"
WORKTREE_DB_VARS="${WORKTREE_DB_VARS-DB_NAME B2C_DB_NAME}"

main_dotenv_value() { # main_dotenv_value VAR: value from the main .env, if any
  [[ -f "$REPO_ROOT/.env" ]] || return 0
  sed -n -E "s/^[[:space:]]*(export[[:space:]]+)?$1[[:space:]]*=[[:space:]]*//p" "$REPO_ROOT/.env" \
    | tail -n 1 \
    | sed -E "s/[[:space:]]+#.*$//; s/^[\"']//; s/[\"']$//"
}

branch_slug() { # lowercase, [a-z0-9_] only
  printf '%s' "$1" | tr 'A-Z' 'a-z' | tr -c 'a-z0-9' '_' | sed 's/__*/_/g; s/^_//; s/_$//'
}

# derive_db_names <branch> [quiet]: fills DERIVED_ENV (VAR=<main>_<part>),
# DERIVED_TEST (test_<main>_<part>, what Django creates) and DERIVED_GRANT
# (MySQL grant patterns) for the branch. Variables without a value in the main
# .env are skipped.
DERIVED_ENV=()
DERIVED_TEST=()
DERIVED_GRANT=()
derive_db_names() {
  local branch="$1" quiet="${2:-0}" slug hash var main_value room part keep_len
  DERIVED_ENV=()
  DERIVED_TEST=()
  DERIVED_GRANT=()
  slug="$(branch_slug "$branch")"
  hash="$(printf '%s' "$branch" | cksum | cut -d' ' -f1)"
  for var in $WORKTREE_DB_VARS; do
    main_value="$(main_dotenv_value "$var")"
    if [[ -z "$main_value" ]]; then
      # Silent for the built-in variable names: a project without a database
      # (most JS/Rust ones) has none of them and needs no isolation.
      [[ $quiet -eq 1 || -z "${WORKTREE_DB_VARS_EXPLICIT:-}" ]] \
        || echo "note: $var not found in the main .env; its database is not isolated" >&2
      continue
    fi
    room=$((64 - 5 - ${#main_value} - 1)) # "test_" + <main> + "_" + <branch part>
    part="$slug"
    if [[ ${#part} -gt $room ]]; then
      keep_len=$((room - 1 - ${#hash}))
      if [[ $keep_len -lt 4 ]]; then
        [[ $quiet -eq 1 ]] || echo "note: $var value '$main_value' is too long to add a branch suffix; not isolated" >&2
        continue
      fi
      part="${part:0:$keep_len}_${hash}"
    fi
    DERIVED_ENV+=("${var}=${main_value}_${part}")
    DERIVED_TEST+=("test_${main_value}_${part}")
    DERIVED_GRANT+=("test\\_${main_value//_/\\_}\\_%")
  done
}

# Python with mysqlclient (the project's venv first); prints its path or nothing.
db_python() {
  local candidate
  for candidate in "$REPO_ROOT/.venv/bin/python" "$(command -v python3 || true)"; do
    if [[ -x "$candidate" ]] && "$candidate" -c 'import MySQLdb' 2>/dev/null; then
      printf '%s' "$candidate"
      return 0
    fi
  done
}

# Run python code $2 (stdin script) with $1 = interpreter and the MySQL
# credentials of the main .env in DROP_* variables (not on the command line).
# Remaining arguments are passed to the script.
db_script() {
  local py="$1" code="$2"
  shift 2
  DROP_HOST="$(main_dotenv_value DB_HOST)" DROP_USER="$(main_dotenv_value DB_USER)" \
    DROP_PWD="$(main_dotenv_value DB_PWD)" DROP_PORT="$(main_dotenv_value DB_PORT)" \
    "$py" -c "$code" "$@"
}

DB_PY_COMMON='
import os, sys
import MySQLdb

try:
    conn = MySQLdb.connect(
        host=os.environ["DROP_HOST"],
        user=os.environ["DROP_USER"],
        passwd=os.environ["DROP_PWD"],
        port=int(os.environ.get("DROP_PORT") or 3306),
        connect_timeout=10,
    )
except MySQLdb.Error as exc:
    print("could not connect to MySQL: %s" % exc, file=sys.stderr)
    sys.exit(1)
cur = conn.cursor()
'

# Print the databases whose name starts with one of the prefixes $@.
list_databases() {
  local py
  py="$(db_python)"
  [[ -n "$py" ]] || return 1
  db_script "$py" "$DB_PY_COMMON"'
cur.execute("SHOW DATABASES")
for (name,) in cur.fetchall():
    if any(name.startswith(p) for p in sys.argv[1:]):
        print(name)
' "$@"
}

# Drop the test_* databases $@ (anything else is refused).
drop_databases() {
  local py
  py="$(db_python)"
  [[ -n "$py" ]] || return 1
  db_script "$py" "$DB_PY_COMMON"'
failed = False
for name in sys.argv[1:]:
    if "`" in name or not name.startswith("test_"):
        print("skipped unsafe name: %r" % name)
        continue
    try:
        cur.execute("DROP DATABASE IF EXISTS `%s`" % name)
        print("dropped (if it existed): %s" % name)
    except MySQLdb.Error as exc:
        failed = True
        print("could not drop %s: %s" % (name, exc))
sys.exit(1 if failed else 0)
' "$@"
}

# --reap ---------------------------------------------------------------------
# State of the newest MR whose source branch is $1: opened/merged/closed/...
mr_state() {
  if [[ "$FORGE" == "gh" ]]; then
    (cd "$REPO_ROOT" && gh pr list ${GH_REPO_ARGS[@]+"${GH_REPO_ARGS[@]}"} --head "$1" --state all --limit 1 \
      --json state --jq '.[0].state // empty' 2>/dev/null | tr 'A-Z' 'a-z') || true
    return 0
  fi
  (cd "$REPO_ROOT" && glab api "projects/:id/merge_requests?source_branch=$1&state=all&order_by=updated_at&per_page=1" 2>/dev/null \
    | python3 -c 'import json,sys
d=json.load(sys.stdin)
print(d[0]["state"] if d else "")' 2>/dev/null) || true
}

reap() {
  local wt_root="$REPO_ROOT/.claude/worktrees" upstream="$BASE_BRANCH"
  local state_dir path branch why gitdir sf pid i mr ahead
  local reap_paths=() reap_branches=() reap_why=() kept=() stale_states=() orphan_dbs=()
  state_dir="$(git -C "$REPO_ROOT" rev-parse --absolute-git-dir)/orchestrate"

  git -C "$REPO_ROOT" worktree prune
  git -C "$REPO_ROOT" fetch --quiet origin "$BASE_BRANCH" 2>/dev/null \
    || echo "warning: could not fetch origin/$BASE_BRANCH; using what is local" >&2
  if git -C "$REPO_ROOT" rev-parse --verify --quiet "refs/remotes/origin/$BASE_BRANCH" >/dev/null; then
    upstream="origin/$BASE_BRANCH"
  fi

  log "Worktrees under $wt_root"
  while IFS=$'\t' read -r path branch; do
    [[ -n "$path" && -n "$branch" ]] || continue
    sf="$state_dir/${branch//\//_}.json"
    # A live Claude session or orchestrator run for the branch: hands off.
    if pgrep -f -- "--name ${branch//./\\.}(-rebase|-ci|-gate)?( |\$)" >/dev/null 2>&1; then
      kept+=("$branch"); echo "keep   $branch: a Claude session is running"; continue
    fi
    pid=""
    if [[ -f "$sf" ]]; then
      STATE_FILE="$sf"; pid="$(state_get pid)"; STATE_FILE=""
    fi
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kept+=("$branch"); echo "keep   $branch: orchestrator run $pid is active"; continue
    fi
    gitdir="$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null || true)"
    if [[ -z "$gitdir" || -d "$gitdir/rebase-merge" || -d "$gitdir/rebase-apply" ]]; then
      kept+=("$branch"); echo "keep   $branch: rebase in progress or unreadable"; continue
    fi
    if [[ -n "$(git -C "$path" status --porcelain)" ]]; then
      kept+=("$branch"); echo "keep   $branch: uncommitted changes"; continue
    fi

    why=""
    ahead="$(git -C "$REPO_ROOT" rev-list --count "$upstream..$branch" 2>/dev/null || echo 1)"
    if [[ "$ahead" -eq 0 ]]; then
      # Nothing beyond the target: a run that never produced commits. Without a
      # state file we cannot tell it from a run that is just starting, so it
      # must also be a day old.
      if [[ -f "$sf" || -n "$(find "$path" -maxdepth 0 -mmin +1440 2>/dev/null)" ]]; then
        why="no commits beyond $upstream and no active run"
      else
        kept+=("$branch"); echo "keep   $branch: nothing beyond the target (merged or not started) and younger than a day"; continue
      fi
    elif git -C "$REPO_ROOT" merge-base --is-ancestor "$branch" "$upstream"; then
      why="merged into $upstream"
    else
      mr="$(mr_state "$branch")"
      case "$mr" in
        merged|closed)
          if git -C "$REPO_ROOT" rev-parse --verify --quiet "refs/remotes/origin/$branch" >/dev/null \
             && [[ "$(git -C "$REPO_ROOT" rev-list --count "origin/$branch..$branch")" -eq 0 ]]; then
            why="MR $mr, branch fully pushed"
          else
            kept+=("$branch"); echo "keep   $branch: MR $mr but commits are not on origin"; continue
          fi
          ;;
        *)
          kept+=("$branch"); echo "keep   $branch: MR ${mr:-not found}"; continue
          ;;
      esac
    fi
    reap_paths+=("$path"); reap_branches+=("$branch"); reap_why+=("$why")
    echo "reap   $branch: $why"
  done < <(git -C "$REPO_ROOT" worktree list --porcelain | awk -v root="$wt_root/" '
    /^worktree /{p=substr($0,10)}
    /^branch refs\/heads\//{ if (index(p, root) == 1) print p "\t" substr($0, 19) }')

  # State files of runs whose worktree is gone and whose process is dead.
  if [[ -d "$state_dir" ]]; then
    for sf in "$state_dir"/*.json; do
      [[ -f "$sf" ]] || continue
      STATE_FILE="$sf"; path="$(state_get worktree)"; pid="$(state_get pid)"; STATE_FILE=""
      [[ -z "$pid" ]] || ! kill -0 "$pid" 2>/dev/null || continue
      if [[ -z "$path" || ! -e "$path" ]]; then
        stale_states+=("$sf"); echo "state  $(basename "$sf"): worktree gone"
      fi
    done
  fi

  # Databases: <main>_<slug of BRANCH_PREFIX>_* that no remaining worktree owns.
  if [[ "${KEEP_TEST_DB:-0}" != "1" && -n "$WORKTREE_DB_VARS" ]]; then
    local prefixes=() owned=() var main_value db b
    for var in $WORKTREE_DB_VARS; do
      main_value="$(main_dotenv_value "$var")"
      [[ -z "$main_value" ]] || prefixes+=("test_${main_value}_$(branch_slug "$BRANCH_PREFIX")")
    done
    for b in ${kept[@]+"${kept[@]}"}; do
      derive_db_names "$b" 1
      owned+=(${DERIVED_TEST[@]+"${DERIVED_TEST[@]}"})
    done
    if [[ ${#prefixes[@]} -gt 0 ]]; then
      if dbs="$(list_databases "${prefixes[@]}" 2>/dev/null)"; then
        while IFS= read -r db; do
          [[ -n "$db" ]] || continue
          [[ " ${owned[*]-} " != *" $db "* ]] || continue
          orphan_dbs+=("$db"); echo "db     $db: no worktree uses it"
        done <<<"$dbs"
      else
        echo "warning: could not list databases (no MySQL access); skipping the database sweep" >&2
      fi
    fi
  fi

  if [[ ${#reap_paths[@]} -eq 0 && ${#stale_states[@]} -eq 0 && ${#orphan_dbs[@]} -eq 0 ]]; then
    echo "Nothing to reap."
    return 0
  fi
  [[ $REAP_DRY -eq 0 ]] || { echo "(dry run: nothing changed)"; return 0; }
  if [[ $REAP_YES -eq 0 ]]; then
    [[ -t 0 ]] || { echo "not a terminal; pass --yes to reap without asking" >&2; exit 64; }
    read -r -p "Remove the above? [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]] || { echo "aborted"; return 0; }
  fi

  for ((i = 0; i < ${#reap_paths[@]}; i++)); do
    path="${reap_paths[$i]}"; branch="${reap_branches[$i]}"
    if git -C "$REPO_ROOT" worktree remove "$path"; then
      if git -C "$REPO_ROOT" branch -d "$branch" >/dev/null 2>&1; then
        git -C "$REPO_ROOT" update-ref -d "refs/orchestrate-backup/$branch" 2>/dev/null || true
        echo "removed worktree and branch $branch"
      else
        echo "removed worktree $path; kept branch $branch (not merged locally; git branch -D to force)"
      fi
      rm -f "$state_dir/${branch//\//_}.json"
    else
      echo "could not remove $path; left in place" >&2
    fi
  done
  for sf in ${stale_states[@]+"${stale_states[@]}"}; do rm -f "$sf"; done
  if [[ ${#orphan_dbs[@]} -gt 0 ]]; then
    drop_databases "${orphan_dbs[@]}" || echo "warning: some databases could not be dropped" >&2
  fi
  git -C "$REPO_ROOT" worktree prune
}

if [[ $REAP -eq 1 ]]; then
  [[ $# -eq 0 && $MR_ONLY -eq 0 ]] || usage
  command -v pgrep >/dev/null || { echo "missing required command: pgrep" >&2; exit 69; }
  reap
  exit 0
fi

# MR-only mode ---------------------------------------------------------------
if [[ $MR_ONLY -eq 1 ]]; then
  [[ $# -le 1 ]] || usage
  BRANCH="${1:-$(git symbolic-ref --quiet --short HEAD || true)}"
  if [[ -z "$BRANCH" ]]; then
    echo "no branch given and HEAD is detached" >&2
    exit 64
  fi
  if ! git -C "$REPO_ROOT" show-ref --verify --quiet "refs/heads/$BRANCH"; then
    echo "branch not found locally: $BRANCH" >&2
    exit 64
  fi
  if [[ "$BRANCH" == "$BASE_BRANCH" ]]; then
    echo "branch and target branch are the same: $BRANCH" >&2
    exit 64
  fi
  if [[ "$BRANCH" == "$(git symbolic-ref --quiet --short HEAD || true)" \
        && -n "$(git status --porcelain)" ]]; then
    echo "warning: uncommitted changes are not part of the MR" >&2
  fi

  state_init "$BRANCH"
  state_set "pid=$$" stage=mr-only
  # Reuse the implement session of an earlier run for rebase conflicts.
  IMPLEMENT_SESSION_ID="$(state_get session_id)"
  if [[ -n "$IMPLEMENT_SESSION_ID" ]] && ! session_exists "$IMPLEMENT_SESSION_ID"; then
    IMPLEMENT_SESSION_ID=""
  fi

  create_mr "$REPO_ROOT" ""
  printf '\nDone. Please check the merge request:\n  %s\n' \
    "${MR_URL:-run: $(forge_view_hint "$BRANCH")}"
  notify_done "${MR_URL:-run: $(forge_view_hint "$BRANCH")}"
  exit 0
fi

# Worktree mode --------------------------------------------------------------
[[ $# -ge 1 ]] || usage

FEATURE="$1"
shift
EXTRA="$*"

if [[ ! "$FEATURE" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  echo "feature-name must be a slug (letters, digits, . _ -): '$FEATURE'" >&2
  exit 64
fi

BRANCH="${BRANCH_PREFIX}${FEATURE}"
WORKTREE="$REPO_ROOT/.claude/worktrees/$BRANCH"
state_init "$BRANCH"

# MCP servers to initialise, chosen from `mcpctl catalog list`. Done up front so
# bad input fails before a worktree exists.
CATALOG=()
if command -v mcpctl >/dev/null; then
  while IFS= read -r line; do
    [[ "$line" =~ ^[^[:space:]] ]] || continue # skip "  source: ..." lines
    line="${line%% *}"
    CATALOG+=("${line%%@*}")
  done < <(mcpctl catalog list 2>/dev/null || true)
else
  echo "warning: mcpctl not found; skipping MCP setup" >&2
fi

in_catalog() {
  local n
  for n in ${CATALOG[@]+"${CATALOG[@]}"}; do
    [[ "$n" == "$1" ]] && return 0
  done
  return 1
}

if [[ ${#MCP_SELECTED[@]} -gt 0 ]]; then
  for name in "${MCP_SELECTED[@]}"; do
    in_catalog "$name" || {
      echo "unknown MCP '$name'; catalog has: ${CATALOG[*]:-(nothing)}" >&2
      exit 64
    }
  done
elif [[ ${#CATALOG[@]} -gt 0 ]]; then
  if [[ -t 0 ]]; then
    echo "MCP servers in the mcpctl catalog:"
    for i in "${!CATALOG[@]}"; do
      printf '  %d) %s\n' "$((i + 1))" "${CATALOG[$i]}"
    done
    read -r -p "Initialise which? numbers or names, space/comma separated (Enter = all, 0 = none): " reply
    reply="${reply//,/ }"
    if [[ -z "${reply// /}" ]]; then
      MCP_SELECTED=("${CATALOG[@]}")
    else
      for tok in $reply; do
        if [[ "$tok" == "0" || "$tok" == "none" ]]; then
          MCP_SELECTED=()
          break
        elif [[ "$tok" =~ ^[0-9]+$ && "$tok" -le ${#CATALOG[@]} ]]; then
          MCP_SELECTED+=("${CATALOG[$((tok - 1))]}")
        elif in_catalog "$tok"; then
          MCP_SELECTED+=("$tok")
        else
          echo "invalid selection: $tok" >&2
          exit 64
        fi
      done
    fi
  else
    echo "warning: no TTY and no --mcp given; skipping MCP setup" >&2
  fi
else
  echo "warning: mcpctl catalog is empty; skipping MCP setup" >&2
fi

# 1. worktree ---------------------------------------------------------------
# Re-running a feature reuses what is already there instead of failing: an
# existing worktree is picked up as is, an existing branch is checked out into
# a fresh worktree, so interrupted runs can simply be started again.
RESUMED=0
CHECKED_OUT_AT="$(wt_branch_at "$BRANCH")"
if [[ -e "$WORKTREE" ]]; then
  if ! wt_registered "$WORKTREE"; then
    echo "$WORKTREE exists but is not a git worktree (remove it or pick another name)" >&2
    exit 73
  fi
  if [[ "$CHECKED_OUT_AT" != "$WORKTREE" ]]; then
    echo "worktree $WORKTREE does not have $BRANCH checked out; fix or remove it first" >&2
    exit 73
  fi
  log "Reusing existing worktree $WORKTREE ($BRANCH)"
  RESUMED=1
elif git -C "$REPO_ROOT" show-ref --verify --quiet "refs/heads/$BRANCH"; then
  if [[ -n "$CHECKED_OUT_AT" ]]; then
    echo "branch $BRANCH is already checked out at $CHECKED_OUT_AT" >&2
    exit 73
  fi
  log "Branch $BRANCH exists; creating worktree $WORKTREE from it"
  git -C "$REPO_ROOT" worktree add "$WORKTREE" "$BRANCH"
  RESUMED=1
else
  log "Creating worktree $WORKTREE ($BRANCH from $BASE_BRANCH)"
  git -C "$REPO_ROOT" worktree add -b "$BRANCH" "$WORKTREE" "$BASE_BRANCH"
fi
state_set "pid=$$" stage=worktree "worktree=$WORKTREE" "base=$BASE_BRANCH"

# The worktree lives inside the repo; make sure git does not show it as
# untracked (local exclude only, nothing tracked changes).
if ! git -C "$REPO_ROOT" check-ignore -q "$WORKTREE"; then
  EXCLUDE="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-path info/exclude)"
  mkdir -p "$(dirname "$EXCLUDE")"
  echo ".claude/worktrees/" >> "$EXCLUDE"
  echo "added .claude/worktrees/ to $EXCLUDE"
fi

# Database isolation for parallel runs. The app reads its settings with
# python-decouple, which prefers real environment variables over the main
# checkout's env file that it finds by walking up the directory tree. So only
# the database names are overridden (as environment variables, see step 3b);
# every other value keeps coming from the main checkout. A partial env file in
# the worktree would not work: decouple loads only the first file it finds and
# would hide all the other values.
#
# Name scheme: <value in the main checkout's .env>_<branch>. Django creates
# test_<name>, so a least-privilege MySQL user needs only ONE wildcard grant to
# cover every worktree, e.g.
#   GRANT ALL ON `test\_myapp\_%`.* TO 'app_user'@'%';
# MySQL names are limited to 64 chars, "test_" included; long branch parts are
# cut and hash-suffixed. Variables missing from the main file are left alone.
# Set WORKTREE_DB_VARS="" to disable the isolation altogether.
derive_db_names "$BRANCH"
SESSION_ENV=(${DERIVED_ENV[@]+"${DERIVED_ENV[@]}"})
GRANT_HINTS=(${DERIVED_GRANT[@]+"${DERIVED_GRANT[@]}"})
DB_TEST_NAMES=(${DERIVED_TEST[@]+"${DERIVED_TEST[@]}"}) # test_<name> databases Django creates; dropped with the worktree
if [[ ${#SESSION_ENV[@]} -gt 0 ]]; then
  echo "database overrides: ${SESSION_ENV[*]} (all other settings come from the main checkout)"
fi
if [[ ${#GRANT_HINTS[@]} -gt 0 ]]; then
  echo "least-privilege MySQL user? it needs a grant matching: ${GRANT_HINTS[*]}"
fi

# Credentials: python-decouple finds the main checkout's .env by walking up the
# tree, but that only works while the worktree sits below it. Link it into the
# worktree (a symlink, not a copy: always current, nothing duplicated) so the
# lookup no longer depends on where the worktree lives. The database names
# above still win, as real environment variables take precedence.
if [[ -f "$REPO_ROOT/.env" && ! -e "$WORKTREE/.env" && ! -L "$WORKTREE/.env" ]]; then
  ln -s "$REPO_ROOT/.env" "$WORKTREE/.env"
  if ! git -C "$WORKTREE" check-ignore -q .env; then
    EXCLUDE="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-path info/exclude)"
    mkdir -p "$(dirname "$EXCLUDE")"
    echo "/.env" >> "$EXCLUDE"
  fi
  echo "linked .env -> $REPO_ROOT/.env"
elif [[ ! -e "$REPO_ROOT/.env" && ! -e "$WORKTREE/.env" ]]; then
  echo "warning: no .env in $REPO_ROOT; settings will have no credentials" >&2
fi

# TDD config and feature plan live in the main checkout and are git-ignored
# (.tdd-*.json), so a fresh worktree has neither. Point the outside-in-tdd
# server at the main checkout's files instead of copying them: the plan entry
# is found, and feature status updates land in one place. A file the worktree
# does have itself (e.g. tracked in another project) is left alone.
for pair in TDD_CONFIG_PATH:.tdd-config.json TDD_FEATURES_PATH:.tdd-features.json; do
  var="${pair%%:*}"
  file="${pair#*:}"
  if [[ -f "$REPO_ROOT/$file" && ! -e "$WORKTREE/$file" ]]; then
    SESSION_ENV+=("${var}=$REPO_ROOT/$file")
    echo "$file: using the main checkout's copy ($var)"
  elif [[ ! -e "$WORKTREE/$file" ]]; then
    echo "note: no $file in the main checkout; the session starts without it" >&2
  fi
done


# 2. mcpctl -----------------------------------------------------------------
# Carry the main checkout's .mcp.json settings (e.g. env such as
# TDD_ADAPTER_TIMEOUT_S on the outside-in-tdd server) into the worktree's
# .mcp.json. mcpctl generates that file without them, so they are merged in
# afterwards: command/args stay as mcpctl wrote them, every other key of a
# server that exists in both files comes from the main checkout (env is merged
# key by key). If the worktree has no .mcp.json at all, the main one is copied.
# Silent when the main checkout has none.
apply_main_mcp_settings() {
  local main_file="$REPO_ROOT/.mcp.json" wt_file="$WORKTREE/.mcp.json"
  [[ -f "$main_file" ]] || return 0
  [[ "$main_file" != "$wt_file" ]] || return 0
  python3 - "$main_file" "$wt_file" <<'PYMCP' || echo "warning: could not apply the main checkout's .mcp.json settings" >&2
import json, os, shutil, sys

main_path, wt_path = sys.argv[1:3]
if not os.path.exists(wt_path):
    shutil.copyfile(main_path, wt_path)
    print("copied .mcp.json from the main checkout")
    sys.exit(0)

main = json.load(open(main_path)).get("mcpServers", {})
wt = json.load(open(wt_path))
servers = wt.setdefault("mcpServers", {})
changed = []
for name, cfg in main.items():
    if name not in servers:
        continue
    for key, value in cfg.items():
        if key in ("command", "args"):
            continue
        if key == "env" and isinstance(value, dict) and isinstance(servers[name].get(key), dict):
            merged = {**servers[name][key], **value}
        else:
            merged = value
        if servers[name].get(key) != merged:
            servers[name][key] = merged
            changed.append(f"{name}.{key}")
if changed:
    with open(wt_path, "w") as f:
        json.dump(wt, f, indent=2)
        f.write("\n")
    print("applied from the main checkout's .mcp.json: " + ", ".join(changed))
PYMCP
}

if [[ ${#MCP_SELECTED[@]} -gt 0 ]]; then
  SETUP_ARGS=(setup)
  for name in "${MCP_SELECTED[@]}"; do
    SETUP_ARGS+=(--mcp "$name")
  done
  log "mcpctl ${SETUP_ARGS[*]}"
  # The worktree sits inside the main checkout, so without a .mcpctl.toml of
  # its own mcpctl walks up to the main one, takes the main checkout as the
  # project root and scaffolds nothing here (.agents/, .codex/, .pi/ ... are
  # git-ignored, so a fresh worktree has none). Give the worktree its own copy
  # and setup scaffolds the latest skills and hooks into it.
  if [[ -f "$REPO_ROOT/.mcpctl.toml" && ! -e "$WORKTREE/.mcpctl.toml" ]]; then
    cp "$REPO_ROOT/.mcpctl.toml" "$WORKTREE/.mcpctl.toml"
    if ! git -C "$WORKTREE" check-ignore -q .mcpctl.toml; then
      EXCLUDE="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-path info/exclude)"
      mkdir -p "$(dirname "$EXCLUDE")"
      printf '%s\n' "/.mcpctl.toml" "/.mcpctl.lock" >> "$EXCLUDE"
    fi
  fi
  (cd "$WORKTREE" && mcpctl "${SETUP_ARGS[@]}")
  apply_main_mcp_settings

  # 3. approve MCP servers --------------------------------------------------
  # Approve the .mcp.json entries whose command args name a selected MCP.
  # .claude/settings.local.json is git-ignored, so it is per-worktree.
  log "Approving MCP servers for the session"
  if [[ -f "$WORKTREE/.mcp.json" ]]; then
    APPROVED="$(python3 - "$WORKTREE/.mcp.json" "$WORKTREE/.claude/settings.local.json" \
      "${MCP_SELECTED[@]}" <<'PYJSON'
import json, os, sys

mcp_path, settings_path, selected = sys.argv[1], sys.argv[2], set(sys.argv[3:])
servers = json.load(open(mcp_path)).get("mcpServers", {})
approved = [k for k, v in servers.items() if selected & set(v.get("args", []))]

settings = {}
if os.path.exists(settings_path):
    settings = json.load(open(settings_path))
merged = list(dict.fromkeys(settings.get("enabledMcpjsonServers", []) + approved))
settings["enabledMcpjsonServers"] = merged
os.makedirs(os.path.dirname(settings_path), exist_ok=True)
with open(settings_path, "w") as f:
    json.dump(settings, f, indent=2)
    f.write("\n")
print(" ".join(approved))
PYJSON
    )"
    if [[ -n "$APPROVED" ]]; then
      echo "approved: $APPROVED"
    else
      echo "warning: no .mcp.json server matches the selected MCPs; nothing approved" >&2
    fi
  else
    echo "warning: no .mcp.json in the worktree after setup; nothing approved" >&2
  fi
else
  echo "no MCP servers selected; skipping mcpctl setup and approvals"
  apply_main_mcp_settings
fi

# Dependencies, so tests can run inside the session (a fresh worktree has no
# virtualenv and no node_modules). $SETUP_CMD wins; otherwise one install per
# stack found at the root: uv sync, the Node package manager named by the
# lockfile (frozen install), nothing for Rust (cargo fetches on first build).
# A failing install only warns: the session can still work and say what is off.
setup_steps() { # setup_steps <dir>: install commands for the stacks in <dir>, one per line
  local d="$1" pm
  if [[ -f "$d/uv.lock" || -f "$d/pyproject.toml" ]]; then
    if command -v uv >/dev/null; then
      echo "uv sync"
    else
      echo "warning: uv not found; skipping uv sync" >&2
    fi
  fi
  if [[ -f "$d/package.json" ]]; then
    pm="$(node_pm "$d")"
    if ! command -v "$pm" >/dev/null; then
      echo "warning: $pm not found; skipping the Node install" >&2
    else
      case "$pm" in
        pnpm) echo "pnpm install --frozen-lockfile" ;;
        bun) echo "bun install --frozen-lockfile" ;;
        yarn)
          if [[ -f "$d/.yarnrc.yml" ]]; then echo "yarn install --immutable"; else echo "yarn install --frozen-lockfile"; fi
          ;;
        *)
          if [[ -f "$d/package-lock.json" || -f "$d/npm-shrinkwrap.json" ]]; then echo "npm ci"; else echo "npm install"; fi
          ;;
      esac
    fi
  fi
}

if [[ -n "${SETUP_CMD:-}" ]]; then
  log "setup: $SETUP_CMD"
  (cd "$WORKTREE" && bash -c "$SETUP_CMD") || echo "warning: SETUP_CMD failed; tests may not run" >&2
else
  while IFS= read -r setup_cmd; do
    [[ -n "$setup_cmd" ]] || continue
    log "$setup_cmd"
    (cd "$WORKTREE" && bash -c "$setup_cmd") || echo "warning: '$setup_cmd' failed; tests may not run" >&2
  done < <(setup_steps "$WORKTREE")
fi

# 3b. session environment (database + TDD paths) ------------------------------------------------
# Apply the session environment (database names, TDD paths) to the session. They go into the worktree's
# local settings (env is inherited by the MCP servers and the test runs, also
# when you resume the session by hand) and are exported for this process too.
if [[ ${#SESSION_ENV[@]} -gt 0 ]]; then
  python3 - "$WORKTREE/.claude/settings.local.json" "${SESSION_ENV[@]}" <<'PYENV'
import json, os, sys

path, pairs = sys.argv[1], sys.argv[2:]
settings = json.load(open(path)) if os.path.exists(path) else {}
env = settings.setdefault("env", {})
for pair in pairs:
    key, value = pair.split("=", 1)
    env[key] = value
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w") as f:
    json.dump(settings, f, indent=2)
    f.write("\n")
PYENV
  for pair in "${SESSION_ENV[@]}"; do
    export "$pair"
  done
fi

# 4. claude session ---------------------------------------------------------
state_set stage=setup
PROMPT="/implement-and-review $FEATURE"
[[ -n "$EXTRA" ]] && PROMPT="$PROMPT $EXTRA"
if [[ $RESUMED -eq 1 ]]; then
  PROMPT="$PROMPT

This branch already exists from an earlier run. Check its commits ($BASE_BRANCH..$BRANCH), the uncommitted changes and the outside-in-tdd status first, and continue from there instead of starting over."
fi

# Hands-off: Claude is asked to create a marker file as its very last step; a
# watcher then closes the session so the push/MR steps below run by themselves.
# The marker lives in the worktree's git dir, so it never shows up in git status.
DONE_FILE="$(git -C "$WORKTREE" rev-parse --absolute-git-dir)/orchestrate-done"
rm -f "$DONE_FILE"
if [[ $AUTO_EXIT -eq 1 ]]; then
  PROMPT="$PROMPT

Final step, only after everything is implemented, reviewed, fixed and committed (nothing left to do or ask): run \`touch $DONE_FILE\`. Do not push or create a merge request; the orchestration script does that after this session ends."
  (
    while [[ ! -e "$DONE_FILE" ]]; do sleep 2; done
    sleep 5 # let the final message render
    # Anchored so "fix--card" never matches a parallel "fix--card-2" session.
    pkill -TERM -f -- "--name ${BRANCH//./\\.}( |\$)" || true
  ) &
  WATCHER=$!
fi

log "Starting Claude: ${PROMPT%%$'\n'*}"
if [[ $AUTO_EXIT -eq 1 ]]; then
  echo "The session closes by itself once Claude reports it is finished."
else
  echo "When the work is committed, exit Claude (/exit or Ctrl+D twice) to continue with push + MR."
fi
# A re-run picks the recorded implement session back up when its transcript
# exists (the prompt above already says to continue); else it gets a new id.
PRIOR_SESSION_ID="$(state_get session_id)"
if [[ $RESUMED -eq 1 && -n "$PRIOR_SESSION_ID" ]] && session_exists "$PRIOR_SESSION_ID"; then
  IMPLEMENT_SESSION_ID="$PRIOR_SESSION_ID"
  SESSION_ARGS=(--resume "$IMPLEMENT_SESSION_ID")
  echo "resuming implement session $IMPLEMENT_SESSION_ID"
else
  IMPLEMENT_SESSION_ID="$(python3 -c 'import uuid; print(uuid.uuid4())')"
  SESSION_ARGS=(--session-id "$IMPLEMENT_SESSION_ID")
fi
state_set stage=implement "session_id=$IMPLEMENT_SESSION_ID"
(cd "$WORKTREE" && claude ${MODEL_ARGS[@]+"${MODEL_ARGS[@]}"} ${COMPACT_ARGS[@]+"${COMPACT_ARGS[@]}"} \
  "${SESSION_ARGS[@]}" ${RC_ON:+"--remote-control=$BRANCH"} --name "$BRANCH" "$PROMPT") || {
  [[ -e "$DONE_FILE" ]] || echo "claude exited non-zero; continuing to post-session checks" >&2
}
if [[ -n "$WATCHER" ]]; then
  kill "$WATCHER" 2>/dev/null || true
  wait "$WATCHER" 2>/dev/null || true
  WATCHER=""
fi

# 5. merge request ----------------------------------------------------------
KEPT="Worktree kept: $WORKTREE (branch $BRANCH)"
if [[ -n "$(git -C "$WORKTREE" status --porcelain)" ]]; then
  log "Uncommitted changes in the worktree; not creating an MR."
  echo "$KEPT" >&2
  exit 1
fi

create_mr "$WORKTREE" "$KEPT"

# 6. CI pipeline ------------------------------------------------------------
ci_check "$WORKTREE"

# 7. cleanup ----------------------------------------------------------------
state_set stage=cleanup
log "Removing worktree $WORKTREE"
git -C "$REPO_ROOT" worktree remove "$WORKTREE" \
  || { echo "Could not remove worktree. $KEPT" >&2; exit 1; }

# Drop the per-worktree test databases (--reuse-db keeps them otherwise). Only
# names this run derived from <main value>_<branch> are touched, never the
# shared test_<main value>. Credentials come from the main .env and are passed
# to the helper through its environment, not the command line.
drop_test_databases() {
  [[ ${#DB_TEST_NAMES[@]} -gt 0 ]] || return 0
  if [[ "${KEEP_TEST_DB:-0}" == "1" ]]; then
    echo "KEEP_TEST_DB=1: leaving test databases: ${DB_TEST_NAMES[*]}"
    return 0
  fi

  if [[ -z "$(db_python)" ]]; then
    echo "warning: no Python with mysqlclient found; drop these manually: ${DB_TEST_NAMES[*]}" >&2
    return 0
  fi

  log "Dropping test databases: ${DB_TEST_NAMES[*]}"
  drop_databases "${DB_TEST_NAMES[@]}" \
    || echo "warning: could not drop the test databases; drop them manually: ${DB_TEST_NAMES[*]}" >&2
}
drop_test_databases

# The run is complete: its state file is no longer needed.
rm -f "$STATE_FILE"
STATE_FILE=""

printf '\nDone. Please check the merge request:\n  %s\nBranch %s is still available locally and on origin.\n' \
  "${MR_URL:-run: $(forge_view_hint "$BRANCH")}" "$BRANCH"
notify_done "${MR_URL:-run: $(forge_view_hint "$BRANCH")}"

exit 0
}
