#!/usr/bin/env bash
# One steady broadcast through the passthrough chain, judged for memory growth after warm-up.
#
#   ffmpeg (live, every frame a keyframe) | moq import ts --passthrough -> moq-relay
#       -> moq export ts --passthrough > /dev/null
#
# Each frame starts a group, so a minute of the run is an hour of a broadcast cutting a group a
# second. See README.md.
set -euo pipefail

STEADY_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$STEADY_DIR/../.." && pwd)

# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$STEADY_DIR/../lib/harness.sh"

RERUN="$(harness_env STEADY_PROFILE RELAY_BIN MOQ_BIN)just test steady$(harness_argv "$@")"

# lite-07, which carries each group's skips. lite-06 still grows: /quest/m1/lite06-sparse-skips.md.
VERSION=moq-lite-07-wip

# Release code with line tables, so a failing run's heap dumps name their stacks.
PROFILE="${STEADY_PROFILE:-profiling}"
RELAY="${RELAY_BIN:-}"
MOQ="${MOQ_BIN:-}"
# Both binaries refuse an environment variable under `MOQ_` they don't know.
unset RELAY_BIN MOQ_BIN
# Measured by seeding a known leak; see README.md before changing any of these.
SECONDS_RUN=900
WARMUP=300
FPS=60
INTERVAL=5
# Live heap bytes each group may leave behind in a process, on average, before the run fails.
PER_GROUP_ALLOCATED=32
# Bytes a second the import must read and the export write in every interval: the media moves
# about 130 KiB/s, while each tick's heap dump and log lines add under 4 KiB/s on their own.
MIN_FLOW=32768

value() {
    if [[ $# -lt 2 || ! "$2" =~ ^[1-9][0-9]*$ ]]; then
        echo "error: $1 requires a positive whole number" >&2
        exit 2
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --seconds)
            value "$@"
            SECONDS_RUN="$2"
            shift 2
            ;;
        --warmup)
            value "$@"
            WARMUP="$2"
            shift 2
            ;;
        --fps)
            value "$@"
            FPS="$2"
            shift 2
            ;;
        --interval)
            value "$@"
            INTERVAL="$2"
            shift 2
            ;;
        --version)
            [[ $# -ge 2 && -n "$2" ]] || {
                echo "error: --version requires a value" >&2
                exit 2
            }
            VERSION="$2"
            shift 2
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

if ((WARMUP >= SECONDS_RUN)); then
    echo "error: --warmup ($WARMUP) must be shorter than --seconds ($SECONDS_RUN)" >&2
    exit 2
fi

for tool in cargo ffmpeg curl python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool not found; run inside 'nix develop'" >&2
        exit 1
    }
done

harness_begin steady "$RERUN"

# ── build ───────────────────────────────────────────────────────────────────
# The jemalloc feature answers SIGUSR1 with the exact live heap, which the sampler reads.
if [[ -z "$RELAY" || -z "$MOQ" ]]; then
    echo "building moq-relay + moq-cli with jemalloc ($PROFILE)..."
    (cd "$WORKSPACE" && cargo build --locked --profile "$PROFILE" -p moq-relay -p moq-cli \
        --features moq-relay/jemalloc,moq-cli/jemalloc)
    TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml" --no-deps |
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
    out="$PROFILE"
    [[ "$PROFILE" == dev ]] && out=debug
    [[ -n "$RELAY" ]] || RELAY="$TARGET_BASE/$out/moq-relay"
    [[ -n "$MOQ" ]] || MOQ="$TARGET_BASE/$out/moq"
fi
harness_stage bin/moq-relay "$RELAY"
harness_stage bin/moq "$MOQ"
RELAY="$HARNESS_RUN/bin/moq-relay"
MOQ="$HARNESS_RUN/bin/moq"

# Sampling stays on from the start, at jemalloc's default rate, so a failing run's dumps
# attribute the growth to the stacks that allocated it (`jeprof --base`).
mkdir "$HARNESS_RUN/heap"
malloc_conf() {
    printf 'prof:true,prof_active:true,prof_prefix:%s' "$HARNESS_RUN/heap/$1"
}

# Pin `moq_tokio::jemalloc=info` so an inherited `RUST_LOG=warn` cannot silence the sampler logs.
STEADY_LOG="${RUST_LOG:-info},moq_tokio::jemalloc=info"

# ── relay ───────────────────────────────────────────────────────────────────
harness_port relay
PORT="$HARNESS_PORT"
URL="http://127.0.0.1:$PORT"
if harness_probe "$URL/certificate.sha256"; then
    echo "error: something is already listening on 127.0.0.1:$PORT (stale relay?)" >&2
    exit 1
fi
harness_spawn relay "$HARNESS_RUN/relay.log" env "RUST_LOG=$STEADY_LOG" "MALLOC_CONF=$(malloc_conf relay)" \
    "$RELAY" "$STEADY_DIR/relay.toml" --listen-version "$VERSION" --listen "127.0.0.1:$PORT" --web-http-listen "127.0.0.1:$PORT"
RELAY_PID="$HARNESS_PID"
if ! harness_ready "$URL/certificate.sha256" 30 "$RELAY_PID"; then
    echo "error: relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi
harness_endpoint relay "$URL"

# ── chain ───────────────────────────────────────────────────────────────────
BROADCAST="steady-$$-$RANDOM"
SOURCE="$HARNESS_RUN/source.fifo"
mkfifo "$SOURCE"

# Each `exec`s so the spawned PID is moq's own, which the sampler reads and signals.
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
export_run() {
    exec env "RUST_LOG=$STEADY_LOG" "MALLOC_CONF=$(malloc_conf export)" "$MOQ" --connect "$URL" --connect-version "$VERSION" \
        --broadcast "$BROADCAST" export ts --passthrough >/dev/null
}
# Opened read-write, which never blocks on a FIFO, so moq is running before the source opens it.
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
import_run() {
    exec env "RUST_LOG=$STEADY_LOG" "MALLOC_CONF=$(malloc_conf import)" "$MOQ" --connect "$URL" --connect-version "$VERSION" \
        --broadcast "$BROADCAST" import ts --passthrough <>"$SOURCE"
}

# The export waits on the announce, so it starts first and never misses the start.
harness_spawn export "$HARNESS_RUN/export.log" export_run
EXPORT_PID="$HARNESS_PID"
harness_spawn import "$HARNESS_RUN/import.log" import_run
IMPORT_PID="$HARNESS_PID"

# A live source at FPS frames a second, every one an IDR, so the passthrough cuts a group at
# each. Tiny frames keep the encoder cheap: the group count is what the run measures.
harness_spawn source "$HARNESS_RUN/source.log" ffmpeg -hide_banner -loglevel warning -nostdin \
    -re -f lavfi -i "testsrc2=size=160x120:rate=$FPS" -t $((SECONDS_RUN + 30)) \
    -c:v libx264 -preset ultrafast -tune zerolatency -x264-params keyint=1:min-keyint=1:scenecut=0 -bf 0 -crf 30 \
    -f mpegts -y "$SOURCE"
SOURCE_PID="$HARNESS_PID"

# ── sample and judge ────────────────────────────────────────────────────────
echo "sampling for ${SECONDS_RUN}s at $FPS groups/s, judging after ${WARMUP}s"
status=0
python3 "$STEADY_DIR/../lib/growth.py" sample --out "$HARNESS_RUN/samples.jsonl" \
    --interval "$INTERVAL" --seconds "$SECONDS_RUN" \
    "import=$IMPORT_PID=$HARNESS_RUN/import.log" \
    "relay=$RELAY_PID=$HARNESS_RUN/relay.log" \
    "export=$EXPORT_PID=$HARNESS_RUN/export.log" || status=$?

# A kept run directory is uploaded, and opening a FIFO with no writer blocks forever.
rm -f "$SOURCE"
if harness_exited "$SOURCE_PID"; then
    echo "error: the source ended before sampling did" >&2
    sed 's/^/  source: /' "$HARNESS_RUN/source.log" >&2 || true
    status=1
fi

if ((status == 0)); then
    python3 "$STEADY_DIR/../lib/growth.py" judge "$HARNESS_RUN/samples.jsonl" --warmup "$WARMUP" \
        --max "allocated=$((PER_GROUP_ALLOCATED * FPS))" \
        --flow import=rchar --flow export=wchar --min-flow "$MIN_FLOW" --report "$HARNESS_RUN/report.json" || status=$?
fi

if ((status != 0)); then
    for name in relay import export; do
        echo "── $name log (tail) ──" >&2
        grep -v "jemalloc heap\|heap profile dumped" "$HARNESS_RUN/$name.log" | tail -n 40 | sed 's/^/  /' >&2 || true
    done
    echo "heap dumps: $HARNESS_RUN/heap (jeprof --base <first> <binary> <last>)" >&2
fi
exit $status
