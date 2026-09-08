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
#   ./run.sh --with-eit            # add a synthetic EPG first, report which SI survived
#   ./run.sh --live                # grade PCR release timing off the live pipe

# `--live` swaps the analyzer, not the rig. compliance.py grades a captured file
# on the stream's own PCR clock, which is the right basis for the IRD model it
# builds and is why it needs no wall-clock capture -- and also why nothing it
# checks can see *when* the exporter handed the bytes over. pcr-timing.py reads
# the subscriber's stdout a packet at a time and stamps each read, so it grades
# release timing and byte position alongside the values. Nightly runs this arm
# (.github/workflows/nightly.yml); it is not a per-PR gate, because it needs a
# real-time window to measure at all.
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)

# Every run writes into a debug bundle instead of a temp dir, so a failure keeps
# its logs, its capture, and the stacks of whatever was still running. See
# test/README.md.
# shellcheck source=../lib/bundle.sh disable=SC1091
source "$WORKSPACE/test/lib/bundle.sh"
# shellcheck source=../lib/harness.sh disable=SC1091
source "$WORKSPACE/test/lib/harness.sh"

SOURCE=""       # real capture to publish instead of a generated clip
ANALYZE_ONLY="" # existing TS to analyze without a round-trip
DURATION="${TSC_DURATION:-20}"
BITRATE="${TSC_BITRATE:-10000000}"
# Empty means "any reserved port"; TSC_PORT or --port pins one instead.
PORT="${TSC_PORT:-}"
PROFILE="${TSC_PROFILE:-debug}"
STRICT=""
WITH_EIT="" # add a synthetic EPG to the source and report which SI survived
LIVE=""     # grade the exporter's stdout as it arrives, rather than a capture
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
        --with-eit)
            WITH_EIT=1
            shift
            ;;
        --live)
            LIVE=1
            shift
            ;;
        *)
            PASSTHRU+=("$1")
            shift
            ;;
    esac
done

URL="" # set once a port is reserved, below

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
    # The EIT fixture reads the service triplet out of the stream and may need to pad a
    # stuffing-free clip to make room for the table.
    if [[ -n "$WITH_EIT" ]]; then
        for t in tstables tsstuff; do have "$t" || missing+=("$t"); done
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

rerun=(just test ts --duration "$DURATION" --bitrate "$BITRATE" --port "$PORT")
[[ -z "$SOURCE" ]] || rerun+=(--source "$SOURCE")
[[ -z "$STRICT" ]] || rerun+=(--strict)
[[ -z "$WITH_EIT" ]] || rerun+=(--with-eit)
[[ -z "$LIVE" ]] || rerun+=(--live)
# The analyzer thresholds ride along too: a rerun without them grades the same
# capture against different limits, which is a different question.
rerun+=(${PASSTHRU[@]+"${PASSTHRU[@]}"})

# ── analyze-only: no relay, no build ────────────────────────────────────────
# No processes, no round-trip, and the analyzer prints its own verdict, so this
# arm has nothing for a bundle to retain and opens none.
if [[ -n "$ANALYZE_ONLY" ]]; then
    require_tools
    [[ -f "$ANALYZE_ONLY" ]] || {
        echo "error: no such file: $ANALYZE_ONLY" >&2
        exit 1
    }
    analyze "$ANALYZE_ONLY"
    exit $?
fi

# ── round-trip capture ──────────────────────────────────────────────────────
# The bundle opens before anything that can fail -- the tool check included, so
# a missing TSDuck still leaves the run identity and the toolchain behind for
# the upload to collect. Same order as the smoke and WASM harnesses.
bundle_init ts
harness_env_array
rerun_env=("${HARNESS_ENV[@]}" "TSC_PROFILE=$PROFILE")
bundle_rerun env "${rerun_env[@]}" "${rerun[@]}"
TMP="$BUNDLE_WORK"
HARNESS_RUN="$TMP"
BROADCAST="tscompliance-$$-${RANDOM}.hang"
SRC_TS="$TMP/source.ts"
SUB_TS="$TMP/sub.ts"
RELAY_PID=""
PUB_PID=""
SUB_PID=""

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
    local status=$?
    # Anything still running here never finished, and the signals below are what
    # erase where it was stuck, so read the stacks first.
    if [[ "$status" -ne 0 ]]; then
        [[ -z "$SUB_PID" ]] || bundle_stack subscriber "$SUB_PID"
        [[ -z "$PUB_PID" ]] || bundle_stack publisher "$PUB_PID"
        [[ -z "$RELAY_PID" ]] || bundle_stack relay "$RELAY_PID"
        if [[ -n "${MOQ_QA_QLOG:-}" ]] && ! find "$BUNDLE_QLOG_LIVE" -type f -print -quit | grep -q .; then
            bundle_capability qlog "requested, but the relay wrote no traces: this backend cannot capture them"
        fi
    fi
    if [[ "$status" -eq 0 ]] || ! bundle_retained; then
        harness_reap_all
        harness_release_ports
    elif ! harness_retain_ports; then
        bundle_note "the retained session had no live process to own its port reservations"
        harness_release_ports
    fi
    bundle_finish "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

require_tools

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml" --no-deps |
    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
[[ -n "$TARGET_BASE" ]] || {
    echo "error: could not resolve cargo target directory" >&2
    exit 1
}

echo "### building moq-relay + moq-cli ($PROFILE)"
flag=()
[[ "$PROFILE" == "release" ]] && flag=(--release)
# qlog is a cargo feature, so capturing traces means building a different relay.
# Opt-in for that reason: it recompiles quinn with the qlog encoder.
[[ -z "${MOQ_QA_QLOG:-}" ]] || flag+=(--features moq-relay/qlog)
(cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli)
RELAY="$TARGET_BASE/$PROFILE/moq-relay"
MOQ="$TARGET_BASE/$PROFILE/moq"

# Source TS: a real capture (preserves all PIDs/PSI) or a generated broadcast-like
# clip (H.264 + AAC, one-second GOP, per-frame PES so audio interleaves evenly).
if [[ -n "$SOURCE" ]]; then
    [[ -f "$SOURCE" ]] || {
        echo "error: no such source: $SOURCE" >&2
        exit 1
    }
    echo "### cutting ~${DURATION}s from $SOURCE with TSDuck (all PIDs preserved)"
    PKTS=$((DURATION * BITRATE / 8 / 188))
    tsp -I file "$SOURCE" -P until --packets "$PKTS" -O file "$SRC_TS" 2>"$HARNESS_RUN/tsp-cut.log" || {
        sed 's/^/  tsp: /' "$HARNESS_RUN/tsp-cut.log" >&2 || true
        exit 1
    }
else
    echo "### generating ~${DURATION}s broadcast-like clip with ffmpeg"
    # CBR with a 20 ms PCR, like a contribution feed. Not cosmetic: `regulate`
    # paces on the source PCR, so a clip whose clock is coarse and whose rate is
    # unconstrained is released unevenly and finishes early (measured: a 20 s clip
    # in 17 s, and release jitter of its own). The harness then grades ffmpeg.
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc=size=1280x720:rate=25" \
        -f lavfi -i "sine=frequency=1000:sample_rate=48000" \
        -t "$DURATION" \
        -c:v libx264 -profile:v high -preset veryfast -pix_fmt yuv420p \
        -x264-params "keyint=25:min-keyint=25:scenecut=0" -b:v 8M \
        -c:a aac -b:a 128k \
        -f mpegts -muxrate "$BITRATE" -pcr_period 20 -pes_payload_size 0 "$SRC_TS"
fi

# No capture in this repository carries EIT, so the import path's EIT handling is
# otherwise untestable. Synthesise one, and report below which SI PIDs came back.
if [[ -n "$WITH_EIT" ]]; then
    "$DIR/make-eit-fixture.sh" "$SRC_TS" "$HARNESS_RUN/source-eit.ts"
    mv "$HARNESS_RUN/source-eit.ts" "$SRC_TS"
fi

# Held for the rest of the run, so a concurrent harness cannot pick the same
# number between here and the relay's bind.
harness_port relay "$PORT"
PORT="$HARNESS_PORT"
URL="http://127.0.0.1:${PORT}"

# The reservation covers other harness runs, not the rest of the machine, so
# still refuse a port some unrelated process is already serving on. Without this
# the readiness probe below would be satisfied by that relay while ours died on
# its failed bind, and the round-trip would grade a binary nobody built here.
if harness_probe "$URL/certificate.sha256"; then
    echo "error: something is already listening on 127.0.0.1:${PORT} (stale relay?)" >&2
    exit 1
fi

# Which binaries the stacks and logs came out of, and what was fed in: a
# round-trip that behaves differently on a rerun is usually a different clip.
bundle_binary moq-relay "$RELAY"
bundle_binary moq-cli "$MOQ"
bundle_fixture "$SRC_TS" source

echo "### starting relay on 127.0.0.1:${PORT}"
sed "s/4443/${PORT}/g" "$DIR/../smoke/smoke.toml" >"$TMP/relay.toml"
relay_args=("$TMP/relay.toml")
[[ -z "${MOQ_QA_QLOG:-}" ]] || relay_args+=(--server-quic-qlog "$BUNDLE_QLOG_LIVE")
harness_spawn relay "$TMP/relay.log" "$RELAY" "${relay_args[@]}"
RELAY_PID="$HARNESS_PID"
bundle_endpoint relay "$URL" "negotiated per session"
bundle_process moq-relay "$RELAY_PID"
if ! harness_ready "$URL/certificate.sha256" 30 "$RELAY_PID"; then
    echo "error: relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi
harness_endpoint relay "$URL"

# Start the subscriber first so it is waiting on the announce before the
# publisher appears; a live broadcast has no history, so a late joiner would miss
# the start of the stream (or the whole thing for a short clip).
#
# In --live mode the grader reads that stdout directly rather than a capture: it
# stamps each 188-byte read, which is the only way release timing survives at all
# (a file has none left in it). It stops itself after its own window, so the
# round-trip below still bounds the run.

# Both halves matter and `wait` can only report one, so record each. The
# exporter's own status is not incidental here: it decides whether the grader saw
# the whole window or graded a stream that ended under it.
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
grade_live() {
    # `set -e` would abort on the pipeline's own failure, which is exactly the
    # status we are here to record.
    set +e
    timeout -k 3 $((DURATION + 20)) \
        "$MOQ" --client-connect "$URL" --broadcast "$BROADCAST" export ts 2>"$HARNESS_RUN/sub.log" |
        python3 "$DIR/pcr-timing.py" --live --seconds "$DURATION" --release-pct-max 1 $STRICT \
            ${PASSTHRU[@]+"${PASSTHRU[@]}"} >"$HARNESS_RUN/timing.out" 2>&1
    printf '%s\n' "${PIPESTATUS[0]} ${PIPESTATUS[1]}" >"$HARNESS_RUN/timing.rc"
}

# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
capture() {
    timeout -k 3 $((DURATION + 20)) \
        "$MOQ" --client-connect "$URL" --broadcast "$BROADCAST" export ts >"$SUB_TS" 2>"$HARNESS_RUN/sub.log"
}

if [[ -n "$LIVE" ]]; then
    echo "### grading subscriber output live (export ts | pcr-timing.py)"
    harness_spawn sub - grade_live
else
    echo "### capturing subscriber output (export ts)"
    harness_spawn sub - capture
fi
SUB_PID="$HARNESS_PID"
sleep 1

# Pace on the source PCR (real media time), not a fixed bitrate: a synthetic clip
# compresses tiny, so bitrate pacing would rush the whole stream out in a blink.
# `--wait-min 5` is what makes that pacing fine-grained enough to measure against:
# at the default, `regulate` releases in ~50 ms chunks, which is jitter of its own
# on top of whatever the exporter does (measured: p95 40 ms at the default, 1 ms
# at 5). It is the publisher's granularity, not the exporter's, so the harness has
# to be well inside it or it grades tsp.
# Bounded like the subscriber: if the relay stalls after readiness, `moq import
# ts` could otherwise block `wait "$PUB_PID"` forever. tsp/moq stderr lands in
# pub.log, which the empty-capture handler below dumps on failure.
echo "### publishing PCR-paced TS -> $BROADCAST"
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
publish() {
    # shellcheck disable=SC2016  # $1..$4 are the child bash -c positionals, not ours.
    timeout -k 3 $((DURATION + 20)) bash -c '
        tsp -I file "$1" -P regulate --pcr-synchronous --wait-min 5 |
            "$2" --client-connect "$3" --broadcast "$4" import ts
    ' _ "$SRC_TS" "$MOQ" "$URL" "$BROADCAST"
}
harness_spawn pub "$HARNESS_RUN/pub.log" publish
PUB_PID="$HARNESS_PID"

# Keep the publisher's exit status: `timeout` returns 124 when it had to kill a
# stalled `moq import ts`, non-zero/non-124 means the import itself errored. Both
# explain a truncated capture, so surface it alongside the logs on failure.
harness_wait "$PUB_PID" && PUB_RC=0 || PUB_RC=$?
PUB_PID=""

# shellcheck disable=SC2329  # invoked from multiple failure paths below
dump_logs() {
    echo "  publisher exit status: $PUB_RC" >&2
    sed 's/^/  pub: /' "$HARNESS_RUN/pub.log" >&2 || true
    sed 's/^/  sub: /' "$HARNESS_RUN/sub.log" >&2 || true
}

# ── live: the grader owns the verdict ───────────────────────────────────────
if [[ -n "$LIVE" ]]; then
    harness_wait "$SUB_PID" || true
    SUB_PID=""
    if [[ ! -s "$HARNESS_RUN/timing.rc" ]]; then
        echo "error: the live grader never reported a status" >&2
        dump_logs
        exit 1
    fi
    read -r EXPORT_RC GRADE_RC <"$HARNESS_RUN/timing.rc"
    echo
    cat "$HARNESS_RUN/timing.out"
    # 124 is `timeout` reaching the end of the window, which is how the exporter is
    # meant to stop; SIGPIPE (141) is the grader closing the pipe on its own window.
    # Anything else ended the stream under the grader, so say so: it graded a
    # shorter window than it was asked for, and the report alone doesn't show that.
    # Not fatal, because the grader's verdict on what it did see is still the
    # verdict, and a short window can only make the sample smaller, not kinder.
    if [[ "$EXPORT_RC" -ne 0 && "$EXPORT_RC" -ne 124 && "$EXPORT_RC" -ne 141 ]]; then
        echo >&2
        echo "warning: the exporter exited $EXPORT_RC before the window closed" >&2
        sed 's/^/  sub: /' "$HARNESS_RUN/sub.log" >&2 || true
    fi
    if [[ "$GRADE_RC" -ne 0 ]]; then
        echo >&2
        echo "error: PCR timing analysis failed (see round-trip logs below)" >&2
        bundle_result timing fail "" "grader exited $GRADE_RC"
        dump_logs
        exit "$GRADE_RC"
    fi
    # The grader's verdict is not the whole run. It grades whatever reached it, and
    # the sample floor only rejects a window that came up short: a publisher that
    # dies late still leaves enough behind to pass every check. That is a broken
    # round-trip reported as a good one, so the publisher's own status is a gate
    # too. 124 is `timeout` killing a stalled `moq import ts`, which is a failure
    # of the same kind rather than a clean end, so it is not excused here.
    if [[ "$PUB_RC" -ne 0 ]]; then
        echo >&2
        echo "error: the publisher exited $PUB_RC; the graded stream is not a whole round-trip" >&2
        bundle_result timing fail "" "publisher exited $PUB_RC"
        dump_logs
        exit 1
    fi
    bundle_result timing pass
    exit 0
fi

sleep 3
harness_reap "$SUB_PID"
SUB_PID=""

if [[ ! -s "$SUB_TS" ]]; then
    echo "error: subscriber captured no data" >&2
    dump_logs
    exit 1
fi

echo "### captured $(wc -c <"$SUB_TS" | tr -d ' ') bytes -> analyzing"

# Which SI survived the round-trip. Informational: the exporter rebuilds SI from the
# catalog, so a PID the import path does not route simply is not there, and that is a
# statement about SI_PIDS rather than a malformed stream. compliance.py grades the stream
# an IRD receives; this says what it was carrying on the way in.
if [[ -n "$WITH_EIT" ]]; then
    echo
    echo "### SI round-trip (source -> capture)"
    count_pid() {
        tsp -I file "$1" -P count --pid "$2" --total -O drop 2>&1 |
            sed -n 's/.*counted \([0-9,]*\) packets.*/\1/p' | head -1
    }
    printf '  %-10s %-8s %12s %12s\n' TABLE PID SOURCE CAPTURE
    for spec in "NIT:0x0010" "SDT:0x0011" "EIT:0x0012" "TDT/TOT:0x0014"; do
        printf '  %-10s %-8s %12s %12s\n' "${spec%%:*}" "${spec##*:}" \
            "$(count_pid "$SRC_TS" "${spec##*:}")" "$(count_pid "$SUB_TS" "${spec##*:}")"
    done
fi
echo
# Pass the source so duration-fidelity can pin the exported stream's rate. A tiny
# capture still parses, so the round-trip can fail here with a non-empty file;
# dump the logs and publisher status so the failure is diagnosable, not a mystery.
bundle_fixture "$SUB_TS" capture
if ! analyze "$SUB_TS" "$SRC_TS"; then
    echo >&2
    echo "error: compliance analysis failed (see round-trip logs below)" >&2
    bundle_result compliance fail
    dump_logs
    exit 1
fi
bundle_result compliance pass
