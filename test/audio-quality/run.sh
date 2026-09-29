#!/usr/bin/env bash
# Plays a tone in headless Chromium over a seeded, impaired UDP path, and grades what it heard.
#
# One row is one page playing one codec through one shaper profile on one ring, for --duration
# seconds of audio. See README.md for the profiles, the metric contract, and the void rules.
set -euo pipefail

AQ_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$AQ_DIR/../.." && pwd)
CLIENT="$AQ_DIR/clients/js"

# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$AQ_DIR/../lib/harness.sh"

# Captured before the parse consumes it, so the rerun command carries every flag.
RERUN="$(harness_env AQ_PROFILE RELAY_BIN)just test audio-quality$(harness_argv "$@")"

ALL_PROFILES=(near-zero mild wide fixed-250)
ALL_RINGS=(isolated plain)
ALL_CODECS=(opus aac)

# Every recorded trace is a profile too, replayed through the rings instead of played in a browser.
ALL_TRACES=()
for trace in "$AQ_DIR"/traces/*.json; do
    ALL_TRACES+=("$(basename "$trace" .json)")
done

PROFILES=("${ALL_PROFILES[@]}" "${ALL_TRACES[@]}")
RINGS=("${ALL_RINGS[@]}")
CODECS=("${ALL_CODECS[@]}")
DURATION=60
SEED=7
OUT=""
LIST=0
ENFORCE=0
PROFILE="${AQ_PROFILE:-debug}"

split() {
    local IFS=,
    # shellcheck disable=SC2206  # splitting on commas is the point
    SPLIT=($1)
}

need() {
    if [[ $2 -lt 2 || -z "${3:-}" || "${3:-}" == -* ]]; then
        echo "error: $1 requires a value" >&2
        exit 2
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --profiles)
            need "$1" $# "${2:-}"
            split "$2"
            PROFILES=("${SPLIT[@]}")
            shift 2
            ;;
        --rings)
            need "$1" $# "${2:-}"
            split "$2"
            RINGS=("${SPLIT[@]}")
            shift 2
            ;;
        --codecs)
            need "$1" $# "${2:-}"
            split "$2"
            CODECS=("${SPLIT[@]}")
            shift 2
            ;;
        --duration)
            need "$1" $# "${2:-}"
            DURATION="$2"
            shift 2
            ;;
        --seed)
            need "$1" $# "${2:-}"
            SEED="$2"
            shift 2
            ;;
        --out)
            need "$1" $# "${2:-}"
            OUT="$2"
            shift 2
            ;;
        --list)
            LIST=1
            shift
            ;;
        --enforce)
            ENFORCE=1
            shift
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

if [[ ! "$DURATION" =~ ^[0-9]+$ ]] || ((DURATION <= 5)); then
    echo "error: --duration must be whole seconds, more than the 5s warmup (got '$DURATION')" >&2
    exit 2
fi
if [[ ! "$SEED" =~ ^[0-9]+$ ]]; then
    echo "error: --seed must be a non-negative integer (got '$SEED')" >&2
    exit 2
fi

valid() {
    local want="$1" name="$2"
    shift 2
    local known
    for known in "$@"; do
        [[ "$known" == "$want" ]] && return 0
    done
    echo "error: unknown $name '$want' (known: $*)" >&2
    exit 2
}
for p in "${PROFILES[@]}"; do valid "$p" profile "${ALL_PROFILES[@]}" "${ALL_TRACES[@]}"; done
for r in "${RINGS[@]}"; do valid "$r" ring "${ALL_RINGS[@]}"; done
for c in "${CODECS[@]}"; do valid "$c" codec "${ALL_CODECS[@]}"; done

if [[ $LIST -eq 0 && -n "$OUT" && -d "$OUT" ]] && [[ -n "$(ls -A "$OUT" 2>/dev/null)" ]]; then
    echo "error: --out $OUT is not empty; remove it or name a new directory" >&2
    exit 2
fi

# The rate is part of the row key: a 44.1 kHz stream's frames do not land on a 48 kHz quantum.
rate_of() {
    case "$1" in
        opus) echo 48000 ;;
        aac) echo 44100 ;;
    esac
}

# Every profile but the control adapts. The element requires a unit on a fixed delay.
delay_of() {
    case "$1" in
        fixed-250) echo "250ms" ;;
        *) echo "auto" ;;
    esac
}

# The path treatment, as moq-shaper flags. Loss, reorder, and rate limits stay off: the buffer's job
# is absorbing arrival spread, and congestion response would make a failure hard to attribute.
shaper_of() {
    case "$1" in
        near-zero | fixed-250) ;;
        mild) echo "--delay 5ms --jitter 5ms" ;;
        wide) echo "--delay 40ms --jitter 40ms" ;;
    esac
}

ROWS=()
TRACES=()
for profile in "${PROFILES[@]}"; do
    if [[ " ${ALL_TRACES[*]} " == *" $profile "* ]]; then
        TRACES+=("$profile")
        continue
    fi
    for codec in "${CODECS[@]}"; do
        for ring in "${RINGS[@]}"; do
            ROWS+=("chromium-$codec-$(rate_of "$codec")-$profile-$ring")
        done
    done
done

# A trace's codec and rate are its recording's, so replay.ts names those rows.
joined() {
    local IFS=,
    echo "$*"
}
replay=(bun "$CLIENT/replay.ts" --traces "$AQ_DIR/traces" --profiles "$(joined "${TRACES[@]}")")
replay+=(--rings "$(joined "${RINGS[@]}")" --codecs "$(joined "${CODECS[@]}")")
listed=$("${replay[@]}" --list)
mapfile -t REPLAYS <<<"$listed"
[[ -n "$listed" ]] || REPLAYS=()

if [[ $LIST -eq 1 ]]; then
    printf '%s\n' "${ROWS[@]}" "${REPLAYS[@]}"
    echo "${#ROWS[@]} rows at ${DURATION}s each, seed $SEED; ${#REPLAYS[@]} replays of their recorded trace" >&2
    exit 0
fi

for tool in cargo bun ffmpeg; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool not found; run inside 'nix develop'" >&2
        exit 1
    }
done

harness_begin audio-quality "$RERUN"

# ── build ───────────────────────────────────────────────────────────────────
flag=()
[[ "$PROFILE" == "debug" ]] || flag=(--profile "$PROFILE")
echo "building moq-relay, moq, moq-shaper ($PROFILE)..."
(cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli -p moq-shaper)
TARGET_BASE="${CARGO_TARGET_DIR:-$WORKSPACE/target}"
RELAY="${RELAY_BIN:-$TARGET_BASE/$PROFILE/moq-relay}"
MOQ="$TARGET_BASE/$PROFILE/moq"
SHAPER="$TARGET_BASE/$PROFILE/moq-shaper"

# Into the run directory: vite empties its output first, so a shared dist/ would vanish under a
# concurrent run's page loads.
echo "building the page..."
(
    cd "$CLIENT"
    bun install --frozen-lockfile
    bunx playwright install chromium
    bunx vite build --outDir "$HARNESS_RUN/page" --emptyOutDir
)

# ── relay ───────────────────────────────────────────────────────────────────
harness_port relay
RELAY_PORT="$HARNESS_PORT"
RELAY_URL="http://127.0.0.1:$RELAY_PORT"
sed "s/4443/$RELAY_PORT/g" "$AQ_DIR/relay.toml" >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" "$RELAY" "$HARNESS_RUN/relay.toml"
if ! harness_ready "$RELAY_URL/certificate.sha256" 30 "$HARNESS_PID"; then
    echo "relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi
harness_endpoint relay "$RELAY_URL"

# ── publishers ──────────────────────────────────────────────────────────────
# A sine tone, never silent, so any quiet quantum at the player's output is a gap or a stall. The
# publishers talk to the relay directly: impairing the ingest would grade the receiver on a stream
# already damaged before it was published. The AAC arm keeps ffmpeg's default PES packing, whose
# multi-frame bursts are the flush-span shape the reporter measured on the public relay (#3477).
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
publish_opus() {
    ffmpeg -hide_banner -v error -re -f lavfi -i "sine=frequency=440:sample_rate=48000" \
        -ac 2 -c:a libopus -b:a 128k \
        -f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer -frag_duration 1000 - |
        "$MOQ" --connect "$RELAY_URL" --broadcast "tone-opus.hang" import fmp4
}

# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
publish_aac() {
    ffmpeg -hide_banner -v error -re -f lavfi -i "sine=frequency=440:sample_rate=44100" \
        -ac 2 -c:a aac -b:a 128k -f mpegts - |
        "$MOQ" --connect "$RELAY_URL" --broadcast "tone-aac.hang" import ts
}

for codec in "${CODECS[@]}"; do
    harness_spawn "pub-$codec" "$HARNESS_RUN/pub-$codec.log" "publish_$codec"
done

# ── rows ────────────────────────────────────────────────────────────────────
# One shaper port for the run: rows play one at a time.
harness_port shaper
SHAPER_PORT="$HARNESS_PORT"

echo ""
echo "running ${#ROWS[@]} rows at ${DURATION}s each (seed $SEED)"
failed=0
for tag in "${ROWS[@]}"; do
    IFS=- read -r _ codec _ rest <<<"$tag"
    ring="${rest##*-}"
    profile="${rest%-*}"
    echo ""
    echo "── $tag ──"

    # shellcheck disable=SC2046  # the profile's flags are meant to split
    harness_spawn "shaper-$tag" "$HARNESS_RUN/shaper-$tag.log" "$SHAPER" \
        --listen "127.0.0.1:$SHAPER_PORT" --target "127.0.0.1:$RELAY_PORT" --seed "$SEED" $(shaper_of "$profile")
    shaper_pid="$HARNESS_PID"
    # It prints its seed and profile once bound; a page dialing before that would lose its handshake.
    until grep -q '^shaper:' "$HARNESS_RUN/shaper-$tag.log" 2>/dev/null; do
        if harness_exited "$shaper_pid"; then
            echo "shaper exited before binding for $tag" >&2
            sed 's/^/  shaper: /' "$HARNESS_RUN/shaper-$tag.log" >&2 || true
            exit 1
        fi
        sleep 0.05
    done

    status=0
    harness_spawn "driver-$tag" "$HARNESS_RUN/driver-$tag.log" bun "$CLIENT/driver.ts" \
        --url "http://127.0.0.1:$SHAPER_PORT" \
        --fingerprint "$RELAY_URL/certificate.sha256" \
        --broadcast "tone-$codec.hang" \
        --page "$HARNESS_RUN/page" \
        --ring "$ring" \
        --delay "$(delay_of "$profile")" \
        --duration "$DURATION" \
        --tag "$tag" \
        --out "$HARNESS_RUN"
    harness_wait "$HARNESS_PID" || status=$?
    if [[ $status -ne 0 ]]; then
        failed=1
        sed 's/^/  driver: /' "$HARNESS_RUN/driver-$tag.log" >&2 || true
    fi

    # SIGTERM, not the harness's SIGKILL: the shaper prints its counters and checks the profile
    # acted on the way out, and exits nonzero if it did not.
    kill -TERM -- -"$shaper_pid" 2>/dev/null || true
    shaper_status=0
    harness_wait "$shaper_pid" || shaper_status=$?
    echo "$shaper_status" >"$HARNESS_RUN/shaper-$tag.status"

    bun "$CLIENT/analyze.ts" --run "$HARNESS_RUN" --row "$tag" >"$HARNESS_RUN/$tag.summary.md" || {
        echo "analyze failed for $tag" >&2
        failed=1
    }
done

# ── replays ─────────────────────────────────────────────────────────────────
# Simulated time, so the whole set takes seconds, and deterministic, so the budgets are exact.
if [[ ${#REPLAYS[@]} -gt 0 ]]; then
    echo ""
    echo "replaying ${#REPLAYS[@]} rows"
    "${replay[@]}" --out "$HARNESS_RUN" || failed=1
    for tag in "${REPLAYS[@]}"; do
        bun "$CLIENT/analyze.ts" --run "$HARNESS_RUN" --row "$tag" >"$HARNESS_RUN/$tag.summary.md" || {
            echo "analyze failed for $tag" >&2
            failed=1
        }
    done
fi

# ── grade ───────────────────────────────────────────────────────────────────
echo ""
grade=(bun "$CLIENT/grade.ts" --run "$HARNESS_RUN" --budgets "$AQ_DIR/budgets.json")
[[ $ENFORCE -eq 0 ]] || grade+=(--enforce)
"${grade[@]}" || failed=1

if [[ -n "$OUT" ]]; then
    mkdir -p "$OUT"
    cp -R "$HARNESS_RUN/." "$OUT/"
    echo "saved: $OUT"
fi
exit "$failed"
