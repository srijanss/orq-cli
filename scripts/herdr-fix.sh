#!/usr/bin/env bash
# Start one orchestrate run (orchestrate-feat) per feature, each in its own herdr pane.
#
# Usage:
#   herdr-feats [orchestrate-feat options...] -- <feature>[:<issue-id>] [<feature>[:<issue-id>]...]
#   herdr-feats <feature>[:<issue-id>] [...]            (no shared options)
#
# Everything before `--` is passed unchanged to every orchestrate-feat run
# (e.g. --mcp project-mcp --mcp outside-in-tdd-mcp --model sonnet -t main).
# A `:<issue-id>` suffix becomes `-i <issue-id>` for that feature only.
#
# By default every feature gets its own herdr tab (labelled with the feature
# name), opened in the current directory without stealing focus.
#
# With --grid all features share ONE new tab instead. The first feature runs in
# the tab's first pane; each further pane splits the newest one, alternating
# right / down with a 0.618 ratio (the existing pane keeps 61.8%), so the panes
# tile as a golden spiral:
#
#   +----------+-----+
#   |          |  2  |
#   |    1     +--+--+
#   |          |3 |4 |
#   +----------+--+--+
#
# Options (before the feature list, not forwarded):
#   --grid                    one shared tab with the panes in a golden spiral
#                             (default: one tab per feature)
#   --here                    (--grid) use the current tab instead of a new one;
#                             the first pane is then split off the calling pane
#   --tab-label <text>        (--grid) label for the new tab
#                             (default: feats: <names>)
#   --focus                   switch to the first new tab (default: stay put)
#   --context <text>          extra request text appended after the feature
#                             name of every run (orchestrate-feat's
#                             [extra request text...])
#   --direction <right|down>  (--grid) direction of the first split (default: right)
#   --ratio <0.1-0.9>         (--grid) share kept by the existing pane (default: 0.618)
#   -h, --help                show this help
#
# Env:
#   ORCHESTRATE_SCRIPT  script to run in each pane. Default: the first found of
#                       orchestrate-feat / orchestrate-fix.sh next to this file,
#                       then orchestrate-feat on PATH.
set -euo pipefail

help() {
  sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'
  exit 0
}

DIRECTION="right"
RATIO="0.618"
HERE=0
GRID=0
FOCUS=0
TAB_LABEL=""
CONTEXT=""
COMMON=()
FEATURES=()

# Split arguments at the first `--`; without one, everything is a feature.
args=("$@")
sep=-1
for i in "${!args[@]}"; do
  if [[ "${args[$i]}" == "--" ]]; then
    sep=$i
    break
  fi
done

if [[ $sep -ge 0 ]]; then
  COMMON=("${args[@]:0:$sep}")
  FEATURES=("${args[@]:$((sep + 1))}")
else
  FEATURES=("${args[@]}")
fi

# Own options live in COMMON (or, without `--`, at the front of FEATURES).
parse_own() {
  local -n src=$1
  local out=() i=0
  while [[ $i -lt ${#src[@]} ]]; do
    case "${src[$i]}" in
      -h|--help) help ;;
      --direction)
        DIRECTION="${src[$((i + 1))]:-}"
        i=$((i + 2))
        ;;
      --grid)
        GRID=1
        i=$((i + 1))
        ;;
      --here)
        HERE=1
        i=$((i + 1))
        ;;
      --focus)
        FOCUS=1
        i=$((i + 1))
        ;;
      --context)
        [[ $((i + 1)) -lt ${#src[@]} && -n "${src[$((i + 1))]}" ]] \
          || { echo "--context needs a value" >&2; exit 64; }
        CONTEXT="${src[$((i + 1))]}"
        i=$((i + 2))
        ;;
      --context=*)
        CONTEXT="${src[$i]#--context=}"
        i=$((i + 1))
        ;;
      --tab-label)
        TAB_LABEL="${src[$((i + 1))]:-}"
        i=$((i + 2))
        ;;
      --tab-label=*)
        TAB_LABEL="${src[$i]#--tab-label=}"
        i=$((i + 1))
        ;;
      --ratio)
        RATIO="${src[$((i + 1))]:-}"
        i=$((i + 2))
        ;;
      --ratio=*)
        RATIO="${src[$i]#--ratio=}"
        i=$((i + 1))
        ;;
      --direction=*)
        DIRECTION="${src[$i]#--direction=}"
        i=$((i + 1))
        ;;
      *)
        out+=("${src[$i]}")
        i=$((i + 1))
        ;;
    esac
  done
  src=(${out[@]+"${out[@]}"})
}
parse_own COMMON
[[ $sep -ge 0 ]] || parse_own FEATURES

if [[ "$DIRECTION" != "right" && "$DIRECTION" != "down" ]]; then
  echo "--direction must be right or down" >&2
  exit 64
fi
if [[ ! "$RATIO" =~ ^0?\.[1-9][0-9]*$ ]] || ! python3 -c "import sys; r=float('$RATIO'); sys.exit(0 if 0.1<=r<=0.9 else 1)"; then
  echo "--ratio must be a number between 0.1 and 0.9" >&2
  exit 64
fi
if [[ $GRID -eq 0 && $HERE -eq 1 ]]; then
  echo "--here only makes sense together with --grid" >&2
  exit 64
fi
if [[ ${#FEATURES[@]} -eq 0 ]]; then
  echo "usage: $0 [orchestrate-feat options...] -- <feature>[:<issue-id>]..." >&2
  echo "Run with -h for details." >&2
  exit 64
fi

if [[ "${HERDR_ENV:-}" != "1" ]]; then
  echo "not running inside a herdr pane (HERDR_ENV is not 1); start this from herdr" >&2
  exit 69
fi
for bin in herdr python3; do
  command -v "$bin" >/dev/null || { echo "missing required command: $bin" >&2; exit 69; }
done

SCRIPT="${ORCHESTRATE_SCRIPT:-}"
if [[ -z "$SCRIPT" ]]; then
  here="$(cd "$(dirname "$0")" && pwd)"
  for candidate in "$here/orchestrate-feat" "$here/orchestrate-fix.sh"; do
    if [[ -x "$candidate" ]]; then
      SCRIPT="$candidate"
      break
    fi
  done
  [[ -n "$SCRIPT" ]] || SCRIPT="$(command -v orchestrate-feat || true)"
fi
if [[ -z "$SCRIPT" || ! -x "$SCRIPT" ]]; then
  echo "orchestrate script not found (looked for orchestrate-feat / orchestrate-fix.sh); set ORCHESTRATE_SCRIPT" >&2
  exit 69
fi

# Validate every feature before opening any pane.
NAMES=()
ISSUES=()
for f in "${FEATURES[@]}"; do
  name="${f%%:*}"
  issue=""
  [[ "$f" == *:* ]] && issue="${f#*:}"
  if [[ ! "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
    echo "invalid feature name (slug expected): '$name'" >&2
    exit 64
  fi
  if [[ -n "$issue" && ! "$issue" =~ ^#?[0-9]+$ ]]; then
    echo "invalid issue id for $name: '$issue'" >&2
    exit 64
  fi
  NAMES+=("$name")
  ISSUES+=("${issue#\#}")
done

json_get() { # json_get <python expression on d>
  python3 -c "import json,sys; d=json.load(sys.stdin)['result']; print($1)"
}

quote_cmd() { # single-quote every word: the pane's shell may have zsh
  # spell-correction (CORRECT_ALL), which would otherwise stop on e.g.
  # "project-mcp" -> ".project-mcp" and wait for an answer.
  local word out=""
  for word in "$@"; do
    out+="'${word//\'/\'\\\'\'}' "
  done
  printf '%s' "$out"
}

feature_cmdline() { # feature_cmdline <index>
  local cmd=("$SCRIPT")
  [[ -n "${ISSUES[$1]}" ]] && cmd+=(-i "${ISSUES[$1]}")
  cmd+=(${COMMON[@]+"${COMMON[@]}"} "${NAMES[$1]}")
  [[ -z "$CONTEXT" ]] || cmd+=("$CONTEXT")
  quote_cmd "${cmd[@]}"
}

new_tab() { # new_tab <label>; sets TAB_ID and ROOT_PANE
  local args=(tab create --cwd "$PWD" --label "$1" --no-focus) json
  [[ -z "${HERDR_WORKSPACE_ID:-}" ]] || args+=(--workspace "$HERDR_WORKSPACE_ID")
  json="$(herdr "${args[@]}")" || { echo "failed to create a tab" >&2; exit 1; }
  TAB_ID="$(json_get "d['tab']['tab_id']" <<<"$json")"
  ROOT_PANE="$(json_get "d['root_pane']['pane_id']" <<<"$json")"
}

TAB_ID=""
ROOT_PANE=""
FIRST_TAB=""

if [[ $GRID -eq 0 ]]; then
  # One tab per feature.
  for i in "${!NAMES[@]}"; do
    name="${NAMES[$i]}"
    new_tab "$name"
    [[ -n "$FIRST_TAB" ]] || FIRST_TAB="$TAB_ID"
    herdr pane rename "$ROOT_PANE" "$name" >/dev/null 2>&1 || true
    herdr pane run "$ROOT_PANE" "$(feature_cmdline "$i")" >/dev/null
    echo "$name -> tab $TAB_ID (pane $ROOT_PANE)"
  done
  TAB_ID="$FIRST_TAB"
else
  # All features in one tab, golden spiral.
  if [[ $HERE -eq 0 ]]; then
    [[ -n "$TAB_LABEL" ]] || TAB_LABEL="feats: $(IFS=,; echo "${NAMES[*]}" | sed 's/,/, /g' | cut -c1-40)"
    new_tab "$TAB_LABEL"
    echo "tab $TAB_ID ($TAB_LABEL)"
  fi

  TARGET="$ROOT_PANE"
  [[ $HERE -eq 1 ]] && TARGET="${HERDR_PANE_ID:-}"
  DIR="$DIRECTION"
  for i in "${!NAMES[@]}"; do
    name="${NAMES[$i]}"

    if [[ $i -eq 0 && $HERE -eq 0 ]]; then
      PANE="$ROOT_PANE" # the new tab's first pane
    else
      split_args=(pane split --direction "$DIR" --ratio "$RATIO" --cwd "$PWD" --no-focus)
      if [[ -n "$TARGET" ]]; then
        split_args+=(--pane "$TARGET")
      else
        split_args+=(--current)
      fi
      PANE="$(herdr "${split_args[@]}" | json_get "d['pane']['pane_id']")" \
        || { echo "failed to create a pane for $name" >&2; exit 1; }
      # Spiral: next split goes the other way, off the pane just created.
      if [[ "$DIR" == "right" ]]; then DIR="down"; else DIR="right"; fi
    fi

    herdr pane rename "$PANE" "$name" >/dev/null 2>&1 || true
    herdr pane run "$PANE" "$(feature_cmdline "$i")" >/dev/null
    echo "$name -> pane $PANE"

    TARGET="$PANE"
  done
fi

if [[ -n "$TAB_ID" && $FOCUS -eq 1 ]]; then
  herdr tab focus "$TAB_ID" >/dev/null 2>&1 || true
fi
