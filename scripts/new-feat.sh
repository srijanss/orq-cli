#!/usr/bin/env bash
# Start one feature in its own herdr tab with the usual setup: herdr-feats with
# the project-mcp and outside-in-tdd-mcp servers. Shorthand for
#
#   herdr-feats --model opus --mcp project-mcp --mcp outside-in-tdd-mcp -- <feature>
#
# Usage:
#   new-feat [--model <model>] <feature>[:<issue-id>] [extra context...]
#
#   <feature>        required; slug, optionally with ":<issue-id>"
#   --model <model>  Claude model (default: $NEW_FEAT_MODEL, else opus)
#   extra context    optional; the rest of the line is appended to the
#                    /implement-and-review request
#
# Examples:
#   new-feat member-timezone-preference
#   new-feat --model sonnet member-timezone-preference:123
#   new-feat member-timezone-preference store it per member, default to UTC
#
# Run it from inside herdr, in the project directory.
set -euo pipefail

MODEL="${NEW_FEAT_MODEL:-opus}"
MCPS=(project-mcp outside-in-tdd-mcp)

usage() {
  echo "usage: ${0##*/} [--model <model>] <feature>[:<issue-id>] [extra context...]" >&2
  exit 64
}

case "${1:-}" in
  -h|--help) sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'; exit 0 ;;
esac

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)
      [[ $# -ge 2 && -n "$2" ]] || { echo "--model needs a value" >&2; usage; }
      MODEL="$2"
      shift 2
      ;;
    --model=*)
      MODEL="${1#--model=}"
      [[ -n "$MODEL" ]] || { echo "--model needs a value" >&2; usage; }
      shift
      ;;
    -*) echo "unknown option: $1" >&2; usage ;;
    *) break ;;
  esac
done

[[ $# -ge 1 ]] || { echo "feature name is required" >&2; usage; }
FEATURE="$1"
shift
CONTEXT="$*"

ARGS=(--model "$MODEL")
for m in "${MCPS[@]}"; do
  ARGS+=(--mcp "$m")
done
[[ -z "$CONTEXT" ]] || ARGS+=(--context "$CONTEXT")

exec herdr-feats "${ARGS[@]}" -- "$FEATURE"
