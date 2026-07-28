#!/usr/bin/env bash
# 1+1 active/active source-failover drill across a two-relay cluster.
#
# Two relays are meshed, two publishers publish the SAME broadcast with a shared
# origin id (so they are interchangeable sources), and the active publisher is
# killed. The point is to prove, with real processes rather than model tests,
# that the surviving standby is already advertised across the mesh and that the
# relay serving the dead source reselects onto it.
#
# ```text
#     pubA --> relayA(:4443) <--cluster--> relayB(:5443) <-- pubB
#               ^   ^                          ^
#             sub1 sub2                       sub3
# ```
#
# sub3 subscribes on relayB, which has no local publisher at first, so relayB
# must carry the broadcast across the mesh from relayA. That is what makes relayA
# a hop in relayB's route, and therefore the peer that per-peer announce
# selection has to treat specially: relayB may not advertise the route that runs
# through relayA back to relayA, but it must advertise the local pubB standby
# once that exists.
#
# Two graded checks:
#   CHECK 1 (failover)      kill pubA; sub1 on relayA must resume, i.e. relayA
#                           reselected onto the pubB standby advertised by relayB.
#   CHECK 2 (standby join)  sub3 on relayB must keep its subscription when pubB
#                           attaches locally. A standby wins dispatch the moment
#                           it attaches, which is before a real publisher has
#                           created every track, so a per-track refusal must not
#                           abort the whole subscription.
#
# Three properties of the harness are load-bearing. Changing any of them silently
# turns a real result into a meaningless one, so each is explained where it is
# set: the observation window (derived from the QUIC idle timeout, see below),
# the kill (atomic, see kill_pub), and the standby's join time (small, because
# the two publishers are not timestamp-aligned, see PRE).
#
# TIMELINE, READ BEFORE CHANGING. Killing a publisher never sends
# CONNECTION_CLOSE, so the relay keeps serving the dead source until the QUIC
# idle timeout expires (30s by default). The relay logs nothing between the kill
# and `connection closed err=timed out`, and only then can it reselect. A grading
# window shorter than that budget CANNOT pass on any build. The window is
# therefore derived from the detection budget rather than hard-coded, and
# `--idle` shortens both together for a faster run.
#
# Modes:
#   ./run.sh                  # generated clip, default 30s detection budget (~90s)
#   ./run.sh --idle 10s       # lower the relays' idle timeout, faster (~70s)
#   ./run.sh --source cap.ts  # publish a real capture instead of a generated clip
#   ./run.sh --keep-logs      # keep the work dir and print its path
set -euo pipefail

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)

SOURCE=""
IDLE=""        # --server-quic-idle-timeout on the relays
IDLE_BUDGET="" # seconds the relay needs to notice a dead publisher
PORT_A="${FAILOVER_PORT_A:-4443}"
PORT_B="${FAILOVER_PORT_B:-5443}"
PROFILE="${FAILOVER_PROFILE:-debug}"
KEEP_LOGS=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source)
            SOURCE="$2"
            shift 2
            ;;
        --idle)
            IDLE="$2"
            shift 2
            ;;
        --keep-logs)
            KEEP_LOGS=1
            shift
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

# The detection budget has to be a number of seconds this script can do
# arithmetic on, so accept only whole seconds rather than the full humantime
# grammar the relay itself takes. Anything else would silently produce a garbage
# window, which is the one failure mode this drill cannot afford.
if [[ -n "$IDLE" ]]; then
    if [[ ! "$IDLE" =~ ^([0-9]+)s?$ ]]; then
        echo "error: --idle takes whole seconds, e.g. '10s' or '10' (got '$IDLE')" >&2
        exit 1
    fi
    IDLE_BUDGET="${BASH_REMATCH[1]}"
    IDLE="${IDLE_BUDGET}s"
else
    IDLE_BUDGET=30
fi

# pubB joins relayB as the standby. Keep this SMALL. The two publishers replay
# independent copies of the same clip from its start, so the standby's media
# timeline lags the active one by roughly PRE seconds, and on the splice the
# subscriber's muxer has to wait for the new source's timestamps to overtake the
# last ones it wrote. That wait is a property of this harness, not of the relay,
# and it scales one-for-one with PRE. A real 1+1 pair shares a timestamp-aligned
# feed and does not pay it.
PRE="${FAILOVER_PRE:-4}"
KILLA="${FAILOVER_KILLA:-32}"       # pubA (the active source) is killed
KILLB=$((KILLA + IDLE_BUDGET + 20)) # >=20s of observation AFTER detection
END=$((KILLB + 8))

# GSO stalls on macOS loopback, which this drill leans on heavily.
GSO_RELAY=()
GSO_CLIENT=()
if [[ "$(uname -s)" == "Darwin" ]]; then
    GSO_RELAY=(--server-quic-gso=false --client-quic-gso=false)
    GSO_CLIENT=(--client-quic-gso=false)
fi

have() { command -v "$1" >/dev/null 2>&1; }

require_tools() {
    local missing=() t
    for t in cargo tsp ffmpeg curl pgrep pkill; do
        have "$t" || missing+=("$t")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required tools: ${missing[*]}" >&2
        echo "  TSDuck (tsp) is required for PCR pacing; install from https://tsduck.io" >&2
        exit 1
    fi
}

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
(cd "$WORKSPACE" && cargo build ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli)
RELAY="$TARGET_BASE/$PROFILE/moq-relay"
MOQ="$TARGET_BASE/$PROFILE/moq"

# A 1+1 pair is only a redundant pair if both publishers declare the same origin.
# Without that they are two unrelated broadcasts and nothing here means anything,
# so fail loudly rather than letting every check report a bogus zero-byte result.
if ! "$MOQ" --help 2>&1 | grep -q -- '--origin'; then
    echo "error: this build has no 'moq --origin', so a publisher pair cannot" >&2
    echo "  declare itself interchangeable (added in moq-dev/moq#2473)." >&2
    exit 1
fi

TMP=$(mktemp -d)
BROADCAST="failover-$$-${RANDOM}.hang"
ORIGIN="${FAILOVER_ORIGIN:-424242}" # shared: declares pubA/pubB interchangeable
SRC_TS="$TMP/source.ts"
URL_A="http://127.0.0.1:${PORT_A}"
URL_B="http://127.0.0.1:${PORT_B}"
PIDS=()

# shellcheck disable=SC2329  # invoked from cleanup, which runs via trap
kill_tree() {
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2>/dev/null || true); do kill_tree "$child"; done
    kill -KILL "$pid" 2>/dev/null || true
}

# Killing a publisher must look like the process vanished, leaving the relay
# holding a QUIC connection nobody will ever close. Do NOT reuse kill_tree here.
# It walks children in pid order, so it kills `tsp` first, and `moq import` then
# reads a truncated stream plus EOF and shuts its broadcast down cleanly. The
# relay would unannounce immediately and the drill would grade a graceful detach
# instead of a source failure, which is a completely different code path. SIGKILL
# the whole pipeline in one pass so nothing gets to run shutdown code.
kill_pub() {
    pkill -KILL -P "$1" 2>/dev/null || true
    kill -KILL "$1" 2>/dev/null || true
}

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
    local pid
    for pid in ${PIDS[@]+"${PIDS[@]}"}; do kill_tree "$pid"; done
    if [[ -n "$KEEP_LOGS" ]]; then
        echo "### logs kept in $TMP"
    else
        rm -rf "$TMP"
    fi
}
trap cleanup EXIT

sz() { stat -f%z "$1" 2>/dev/null || stat -c%s "$1" 2>/dev/null || echo 0; }

relay_config() { # $1=port  $2=cluster id  $3=peer port
    cat >"$TMP/relay$2.toml" <<EOF
[log]
level = "info"

[server]
listen = "127.0.0.1:$1"
tls.generate = ["localhost", "127.0.0.1"]

[web.http]
listen = "127.0.0.1:$1"

[auth]
public = ""

[cluster]
id = $2
connect = ["https://127.0.0.1:$3/"]
EOF
}

wait_ready() { # $1=url
    local _
    for _ in $(seq 1 60); do
        curl -sf "$1/certificate.sha256" >/dev/null 2>&1 && return 0
        sleep 0.5
    done
    return 1
}

# The clip must outlast the whole timeline. tsp --infinite loops it as a backstop
# but a wrap mid-drill would muddy the byte accounting.
CLIP=$((END + 15))
if [[ -n "$SOURCE" ]]; then
    [[ -f "$SOURCE" ]] || {
        echo "error: no such source: $SOURCE" >&2
        exit 1
    }
    SRC_TS="$SOURCE"
    echo "### publishing from $SOURCE"
else
    echo "### generating ~${CLIP}s clip with ffmpeg (H.264 + AAC, 1s GOP)"
    ffmpeg -y -hide_banner -loglevel error \
        -f lavfi -i "testsrc=size=640x360:rate=25" \
        -f lavfi -i "sine=frequency=1000:sample_rate=48000" \
        -t "$CLIP" \
        -c:v libx264 -profile:v high -preset ultrafast -pix_fmt yuv420p \
        -x264-params "keyint=25:min-keyint=25:scenecut=0" -b:v 2M \
        -c:a aac -b:a 128k \
        -f mpegts -pes_payload_size 0 "$SRC_TS"
fi

echo "### starting meshed relays on ${PORT_A} and ${PORT_B}"
relay_config "$PORT_A" 11111 "$PORT_B"
relay_config "$PORT_B" 22222 "$PORT_A"
# --client-tls-disable-verify: each relay dials its peer's self-signed cert.
for id in 11111 22222; do
    "$RELAY" "$TMP/relay$id.toml" --client-tls-disable-verify \
        ${GSO_RELAY[@]+"${GSO_RELAY[@]}"} \
        ${IDLE:+--server-quic-idle-timeout "$IDLE"} >"$TMP/relay$id.log" 2>&1 &
    PIDS+=($!)
done
for url in "$URL_A" "$URL_B"; do
    wait_ready "$url" || {
        echo "error: relay at $url never became ready" >&2
        sed 's/^/  relay: /' "$TMP"/relay*.log >&2 || true
        exit 1
    }
done

LAST_PID=""
sub() { # $1=name  $2=url
    "$MOQ" --client-connect "$2" ${GSO_CLIENT[@]+"${GSO_CLIENT[@]}"} \
        --broadcast "$BROADCAST" export ts >"$TMP/$1.ts" 2>"$TMP/$1.log" &
    LAST_PID=$!
    PIDS+=("$LAST_PID")
}
pub() { # $1=name  $2=url
    # shellcheck disable=SC2016  # expansions are the child bash -c's, not ours
    bash -c '
        src=$1 moq=$2 url=$3 origin=$4 bcast=$5
        shift 5
        tsp -I file "$src" --infinite -P regulate --pcr-synchronous |
            "$moq" --client-connect "$url" "$@" --origin "$origin" --broadcast "$bcast" import ts
    ' _ "$SRC_TS" "$MOQ" "$2" "$ORIGIN" "$BROADCAST" \
        ${GSO_CLIENT[@]+"${GSO_CLIENT[@]}"} >"$TMP/$1.log" 2>&1 &
    LAST_PID=$!
    PIDS+=("$LAST_PID")
}

echo "### timeline: pubB joins t=${PRE}, pubA killed t=${KILLA}, pubB killed t=${KILLB}, end t=${END}"
echo "###           (detection budget ${IDLE_BUDGET}s${IDLE:+, --server-quic-idle-timeout $IDLE})"

sub sub1 "$URL_A" # watched for failover
sub sub2 "$URL_A"
sub sub3 "$URL_B" # forces relayB to carry via relayA
sleep 1
pub pubA "$URL_A"
PUBA=$LAST_PID

echo "t,sub1,sub3,event" >"$TMP/sizes.csv"
(
    for t in $(seq 0 "$END"); do
        ev=""
        [[ "$t" -eq "$PRE" ]] && ev=pubB_join
        [[ "$t" -eq "$KILLA" ]] && ev=KILL_pubA
        [[ "$t" -eq "$KILLB" ]] && ev=KILL_pubB
        printf '%s,%s,%s,%s\n' "$t" "$(sz "$TMP/sub1.ts")" "$(sz "$TMP/sub3.ts")" "$ev" >>"$TMP/sizes.csv"
        sleep 1
    done
) &
PIDS+=($!)

sleep "$PRE"
pub pubB "$URL_B"
PUBB=$LAST_PID
echo "### t=$PRE pubB joined relayB (hot standby)"
sleep $((KILLA - PRE))
echo "### t=$KILLA killing pubA (the active source)"
kill_pub "$PUBA"
sleep $((KILLB - KILLA))
echo "### t=$KILLB killing pubB"
kill_pub "$PUBB"
sleep $((END - KILLB))

RC=0

# CHECK 1: did relayA reselect onto the standby held by relayB?
BEFORE=$(awk -F, -v k="$KILLA" '$1==k{print $2}' "$TMP/sizes.csv")
AFTER=$(awk -F, -v k="$((KILLB - 1))" '$1==k{print $2}' "$TMP/sizes.csv")
RESUME=$(awk -F, -v k="$KILLA" -v b="$BEFORE" -v e="$KILLB" '$1>k && $1<e && $2>b {print $1; exit}' "$TMP/sizes.csv")
echo
echo "### CHECK 1 failover: sub1 at kill(t=$KILLA)=$BEFORE -> t=$((KILLB - 1))=$AFTER"
if [[ -n "$BEFORE" && -n "$AFTER" && "$AFTER" -gt "$BEFORE" ]]; then
    echo "PASS: sub1 resumed at t=$RESUME, $((RESUME - KILLA))s after the kill (+$((AFTER - BEFORE)) bytes)"
else
    echo "FAIL: sub1 frozen for the whole $((KILLB - KILLA))s window after pubA died" >&2
    RC=1
fi

# CHECK 2: did the standby joining cost relayB's subscriber its subscription?
# Graded on survival, sampled well clear of the join, because the join is not
# instantaneous here: see PRE for why this harness makes the subscriber wait out
# its own timeline offset.
JOIN=$(awk -F, -v k="$PRE" '$1==k{print $3}' "$TMP/sizes.csv")
JOINED=$(awk -F, -v k="$((KILLA - 1))" '$1==k{print $3}' "$TMP/sizes.csv")
# Longest run of dead seconds between the join and the kill.
STALL=$(awk -F, -v a="$PRE" -v b="$KILLA" '
    NR > 1 {
        if ($1 > a && $1 < b) { if ($3 - p <= 0) { r++; if (r > m) m = r } else r = 0 }
        p = $3
    }
    END { print m + 0 }' "$TMP/sizes.csv")
echo "### CHECK 2 standby join: sub3 at join(t=$PRE)=$JOIN -> t=$((KILLA - 1))=$JOINED"
if [[ -n "$JOIN" && -n "$JOINED" && "$JOINED" -gt "$JOIN" ]]; then
    echo "PASS: sub3 kept its subscription across pubB's join (+$((JOINED - JOIN)) bytes)"
    # Up to ~PRE seconds is the harness offset. Beyond that is worth a look.
    if [[ "$STALL" -gt "$PRE" ]]; then
        echo "WARN: sub3 stalled ${STALL}s at the standby join, more than the ~${PRE}s this"
        echo "      harness can explain by its own publisher timeline offset (see PRE)"
    fi
else
    echo "FAIL: sub3 lost its subscription when the shared-origin standby joined the carrying relay" >&2
    RC=1
fi

if [[ "$RC" -ne 0 ]]; then
    echo >&2
    echo "### sizes.csv" >&2
    sed 's/^/  /' "$TMP/sizes.csv" >&2
    grep -iE "unroutable|dropped" "$TMP"/sub*.log 2>/dev/null | sed 's/^/  /' >&2 || true
fi

exit "$RC"
