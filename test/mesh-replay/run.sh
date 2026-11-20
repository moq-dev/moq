#!/usr/bin/env bash
# Replay a mesh of real relays through a script of faults. See README.md.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"
# shellcheck source-path=SCRIPTDIR source=../lib/release.sh
source "$DIR/../lib/release.sh"

RERUN="$(harness_env MESH_REPLAY_LOG)just test mesh-replay$(harness_argv "$@")"

usage() {
    cat >&2 <<'USAGE'
usage: just test mesh-replay [--mesh FILE]... [--script FILE] [--broadcasts N[,N...]]
                             [--new RELAY] [--old RELAY | --old-release VERSION] [--report DIR]

  --mesh FILE         relays and links (default: test/mesh-replay/ring6.toml); repeat to sweep
  --script FILE       the faults to play (default: test/mesh-replay/script.toml)
  --broadcasts LIST   broadcasts each relay publishes, comma-separated to sweep (default: 4)
  --new RELAY         the relay rolled onto (default: this checkout's build)
  --old RELAY         the relay rolled off (default: the newest moq-relay release)
  --old-release V     that release's version instead of the newest
  --old-versions L    versions the old relay is configured with (default: its own default)
  --report DIR        also write each cell's report.json and report.md here
USAGE
    exit 2
}

MESHES=()
SCRIPT="$DIR/script.toml"
BROADCASTS=4
NEW=""
OLD=""
OLD_RELEASE=""
OLD_VERSIONS=""
REPORT=""
while [[ $# -gt 0 ]]; do
    [[ $# -ge 2 && -n "$2" ]] || usage
    case "$1" in
        --mesh) MESHES+=("$(realpath "$2")") ;;
        --script) SCRIPT=$(realpath "$2") ;;
        --broadcasts) BROADCASTS="$2" ;;
        --new) NEW=$(realpath "$2") ;;
        --old) OLD=$(realpath "$2") ;;
        --old-release) OLD_RELEASE="$2" ;;
        --old-versions) OLD_VERSIONS="$2" ;;
        --report) REPORT=$(realpath -m "$2") ;;
        *) usage ;;
    esac
    shift 2
done
((${#MESHES[@]})) || MESHES=("$DIR/ring6.toml")
# A cell is named for its mesh file, so two meshes of one name would share logs and a report.
dupe=$(for mesh in "${MESHES[@]}"; do basename "$mesh" .toml; done | sort | uniq -d | head -1)
[[ -z "$dupe" ]] || {
    echo "error: two --mesh files are named $dupe.toml; rename one" >&2
    exit 2
}
[[ "$BROADCASTS" =~ ^[1-9][0-9]*(,[1-9][0-9]*)*$ ]] || {
    echo "error: --broadcasts takes whole numbers, comma-separated (got '$BROADCASTS')" >&2
    exit 2
}
[[ -z "$OLD" || -z "$OLD_RELEASE" ]] || {
    echo "error: --old and --old-release name the same relay; give one" >&2
    exit 2
}

harness_begin mesh-replay "$RERUN"
REPORT="${REPORT:-$HARNESS_RUN/report}"

# ── binaries ────────────────────────────────────────────────────────────────
cd "$WORKSPACE"
cargo build --locked -p moq-relay -p moq-bench -p moq-shaper
TARGET=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')
for binary in moq-relay moq-bench moq-shaper; do
    harness_stage "new/$binary" "$TARGET/debug/$binary"
done
if [[ -n "$NEW" ]]; then
    harness_stage new/relay "$NEW"
    NEW_LABEL="$NEW"
else
    harness_stage new/relay "$HARNESS_RUN/new/moq-relay"
    NEW_LABEL="$(git rev-parse --short HEAD)$(git diff --quiet HEAD || echo +dirty)"
fi
if [[ -n "$OLD" ]]; then
    harness_stage old/bin/moq-relay "$OLD"
else
    if [[ -z "$OLD_RELEASE" ]]; then
        OLD_RELEASE=$(bash "$WORKSPACE/test/smoke/release.sh" moq-relay-v | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"].removeprefix("moq-relay-v"))')
    fi
    release_binary moq-relay moq-relay "$OLD_RELEASE" "$HARNESS_RUN/old"
fi

# ── cells ───────────────────────────────────────────────────────────────────
needed=0
for mesh in "${MESHES[@]}"; do
    n=$(python3 "$DIR/replay.py" ports "$mesh")
    ((n > needed)) && needed=$n
done
reserved=()
for ((i = 0; i < needed; i++)); do
    harness_port "mesh-$i"
    reserved+=("$HARNESS_PORT")
done
ports=$(
    IFS=,
    echo "${reserved[*]}"
)

status=0
summary="$HARNESS_RUN/summary.md"
: >"$summary"
IFS=, read -ra counts <<<"$BROADCASTS"
for mesh in "${MESHES[@]}"; do
    for count in "${counts[@]}"; do
        cell="$(basename "$mesh" .toml)-$count"
        mkdir -p "$HARNESS_RUN/$cell"
        args=(
            run --mesh "$mesh" --script "$SCRIPT" --broadcasts "$count"
            --dir "$HARNESS_RUN/$cell" --ports "$ports" --report "$REPORT/$cell"
            --log "${MESH_REPLAY_LOG:-info}" --bench "$HARNESS_RUN/new/moq-bench" --shaper "$HARNESS_RUN/new/moq-shaper"
            --new "$HARNESS_RUN/new/relay" --new-label "$NEW_LABEL" --old "$HARNESS_RUN/old/bin/moq-relay" --old-versions "$OLD_VERSIONS"
        )
        harness_spawn "replay $cell" - python3 "$DIR/replay.py" "${args[@]}"
        if harness_wait "$HARNESS_PID"; then
            echo "pass: $cell"
        else
            echo "FAIL: $cell (logs in $HARNESS_RUN/$cell)" >&2
            status=1
        fi
        [[ ! -f "$REPORT/$cell/report.md" ]] || cat "$REPORT/$cell/report.md" >>"$summary"
    done
done
[[ -z "${GITHUB_STEP_SUMMARY:-}" ]] || cat "$summary" >>"$GITHUB_STEP_SUMMARY"
echo "reports: $REPORT"
exit "$status"
