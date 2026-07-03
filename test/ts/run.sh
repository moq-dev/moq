#!/usr/bin/env bash
# MPEG-TS / IRD compliance harness for the moq subscriber's `export ts` output.
#
# It stands up a moq-relay built from this checkout, publishes a PCR-paced
# transport stream (`tsp -P regulate | moq ... import ts`), captures the
# round-tripped stream from a second client (`moq ... export ts`), and runs the
# TSDuck + custom analyzer in compliance.py against the capture. The point is to
# tell whether what the subscriber emits is something an Integrated
# Receiver/Decoder would accept, and to quantify where it diverges (the exporter
# is VBR, emits no null packets, and paces PCR per frame).
#
# Modes:
#   ./run.sh                       # generate a clip, round-trip it, analyze
#   ./run.sh --source cap.ts       # round-trip a real capture instead
#   ./run.sh --analyze-only cap.ts # skip the round-trip, just analyze a file
#   ./run.sh --strict              # fail on broadcast-shape warnings too
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)

SOURCE=""       # real capture to publish instead of a generated clip
ANALYZE_ONLY="" # existing TS to analyze without a round-trip
DURATION="${TSC_DURATION:-20}"
BITRATE="${TSC_BITRATE:-10000000}"
PORT="${TSC_PORT:-4443}"
PROFILE="${TSC_PROFILE:-debug}"
STRICT=""
PASSTHRU=() # forwarded to compliance.py (thresholds, --report-json, ...)

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source)
            SOURCE="$2"
            shift 2
            ;;
        --analyze-only)
            ANALYZE_ONLY="$2"
            shift 2
            ;;
        --duration)
            DURATION="$2"
            shift 2
            ;;
        --bitrate)
            BITRATE="$2"
            shift 2
            ;;
        --port)
            PORT="$2"
            shift 2
            ;;
        --strict)
            STRICT="--strict"
            shift
            ;;
        *)
            PASSTHRU+=("$1")
            shift
            ;;
    esac
done

URL="http://127.0.0.1:${PORT}"

have() { command -v "$1" >/dev/null 2>&1; }

require_tools() {
    local missing=() t
    for t in tsp tsanalyze python3; do
        have "$t" || missing+=("$t")
    done
    # ffmpeg + cargo are only needed for the round-trip, not for --analyze-only.
    if [[ -z "$ANALYZE_ONLY" ]]; then
        for t in cargo ffmpeg curl timeout; do have "$t" || missing+=("$t"); done
    fi
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required tools: ${missing[*]}" >&2
        echo "  TSDuck (tsp, tsanalyze) is required; install from https://tsduck.io" >&2
        exit 1
    fi
}

analyze() {
    # Single source of truth for the verdict: compliance.py runs the TSDuck
    # tools itself and prints the PASS/WARN/FAIL summary. A second argument is
    # the source TS, which enables the duration-fidelity check (round-trip only).
    local ref=()
    [[ -n "${2:-}" ]] && ref=(--reference "$2")
    python3 "$DIR/compliance.py" --ts "$1" ${ref[@]+"${ref[@]}"} $STRICT ${PASSTHRU[@]+"${PASSTHRU[@]}"}
}

require_tools

# ── analyze-only: no relay, no build ────────────────────────────────────────
if [[ -n "$ANALYZE_ONLY" ]]; then
    [[ -f "$ANALYZE_ONLY" ]] || {
        echo "error: no such file: $ANALYZE_ONLY" >&2
        exit 1
    }
    analyze "$ANALYZE_ONLY"
    exit $?
fi

# ── round-trip capture ──────────────────────────────────────────────────────
TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
[[ -n "$TARGET_BASE" ]] || {
    echo "error: could not resolve cargo target directory" >&2
    exit 1
}

echo "### building moq-relay + moq-cli ($PROFILE)"
flag=()
[[ "$PROFILE" == "release" ]] && flag=(--release)
(cd "$WORKSPACE" && cargo build ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli)
RELAY="$TARGET_BASE/$PROFILE/moq-relay"
MOQ="$TARGET_BASE/$PROFILE/moq"

TMP=$(mktemp -d)
BROADCAST="tscompliance-$$-${RANDOM}.hang"
SRC_TS="$TMP/source.ts"
SUB_TS="$TMP/sub.ts"
RELAY_PID=""
PUB_PID=""
SUB_PID=""

kill_tree() {
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2>/dev/null || true); do kill_tree "$child"; done
    kill -KILL "$pid" 2>/dev/null || true
}

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
    [[ -n "$SUB_PID" ]] && kill_tree "$SUB_PID"
    [[ -n "$PUB_PID" ]] && kill_tree "$PUB_PID"
    [[ -n "$RELAY_PID" ]] && kill_tree "$RELAY_PID"
    rm -rf "$TMP"
}
trap cleanup EXIT

# Source TS: a real capture (preserves all PIDs/PSI) or a generated broadcast-like
# clip (H.264 + AAC, one-second GOP, per-frame PES so audio interleaves evenly).
if [[ -n "$SOURCE" ]]; then
    [[ -f "$SOURCE" ]] || {
        echo "error: no such source: $SOURCE" >&2
        exit 1
    }
    echo "### cutting ~${DURATION}s from $SOURCE with TSDuck (all PIDs preserved)"
    PKTS=$((DURATION * BITRATE / 8 / 188))
    tsp -I file "$SOURCE" -P until --packets "$PKTS" -O file "$SRC_TS" 2>/dev/null
else
    echo "### generating ~${DURATION}s broadcast-like clip with ffmpeg"
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc=size=1280x720:rate=25" \
        -f lavfi -i "sine=frequency=1000:sample_rate=48000" \
        -t "$DURATION" \
        -c:v libx264 -profile:v high -preset veryfast -pix_fmt yuv420p \
        -x264-params "keyint=25:min-keyint=25:scenecut=0" -b:v 8M \
        -c:a aac -b:a 128k \
        -f mpegts -pes_payload_size 0 "$SRC_TS"
fi

echo "### starting relay on 127.0.0.1:${PORT}"
sed "s/4443/${PORT}/g" "$DIR/../smoke/smoke.toml" >"$TMP/relay.toml"
"$RELAY" "$TMP/relay.toml" >"$TMP/relay.log" 2>&1 &
RELAY_PID=$!
for _ in $(seq 1 60); do
    curl -sf "$URL/certificate.sha256" >/dev/null 2>&1 && break
    sleep 0.5
done
if ! curl -sf "$URL/certificate.sha256" >/dev/null 2>&1; then
    echo "error: relay never became ready" >&2
    sed 's/^/  relay: /' "$TMP/relay.log" >&2 || true
    exit 1
fi

# Start the subscriber first so it is waiting on the announce before the
# publisher appears; a live broadcast has no history, so a late joiner would miss
# the start of the stream (or the whole thing for a short clip).
echo "### capturing subscriber output (export ts)"
timeout -k 3 $((DURATION + 20)) \
    "$MOQ" --client-connect "$URL" --broadcast "$BROADCAST" export ts >"$SUB_TS" 2>"$TMP/sub.log" &
SUB_PID=$!
sleep 1

# Pace on the source PCR (real media time), not a fixed bitrate: a synthetic clip
# compresses tiny, so bitrate pacing would rush the whole stream out in a blink.
echo "### publishing PCR-paced TS -> $BROADCAST"
(tsp -I file "$SRC_TS" -P regulate --pcr-synchronous 2>/dev/null |
    "$MOQ" --client-connect "$URL" --broadcast "$BROADCAST" import ts) >"$TMP/pub.log" 2>&1 &
PUB_PID=$!

wait "$PUB_PID" 2>/dev/null || true
PUB_PID=""
sleep 3
kill_tree "$SUB_PID" 2>/dev/null || true
SUB_PID=""

if [[ ! -s "$SUB_TS" ]]; then
    echo "error: subscriber captured no data" >&2
    sed 's/^/  pub: /' "$TMP/pub.log" >&2 || true
    sed 's/^/  sub: /' "$TMP/sub.log" >&2 || true
    exit 1
fi

echo "### captured $(wc -c <"$SUB_TS" | tr -d ' ') bytes -> analyzing"
echo
# Pass the source so duration-fidelity can pin the exported stream's rate.
analyze "$SUB_TS" "$SRC_TS"
