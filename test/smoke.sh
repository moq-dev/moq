#!/usr/bin/env bash
# Cross-language media interop smoke test.
#
# Stands up a real moq-relay, then for each publisher language publishes an
# H.264 broadcast and confirms every subscriber sees data flowing (a non-empty
# frame before the timeout). We check that bytes move end-to-end across
# implementations, not that H.264 decodes.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SMOKE_DIR/.." && pwd)

PUBLISHERS="rust,python"
SUBSCRIBERS="rust,python"
TIMEOUT="${SMOKE_TIMEOUT:-20}"
FPS="${SMOKE_FPS:-30}"
SIZE="${SMOKE_SIZE:-320x240}"
WARMUP="${SMOKE_WARMUP:-2}"
URL="http://127.0.0.1:4443"
NEGATIVE=0

require_value() {
    # require_value <flag> "$@": the flag plus the rest of the argv. Ensures a
    # non-flag value follows, so `set -u` doesn't abort on a bare `--timeout`.
    if [[ $# -lt 2 || -z "${2:-}" || "$2" == -* ]]; then
        echo "error: $1 requires a value" >&2
        exit 2
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --publishers)
            require_value "$@"
            PUBLISHERS="$2"
            shift 2
            ;;
        --subscribers)
            require_value "$@"
            SUBSCRIBERS="$2"
            shift 2
            ;;
        --timeout)
            require_value "$@"
            TIMEOUT="$2"
            shift 2
            ;;
        --negative)
            NEGATIVE=1
            shift
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

IFS=',' read -r -a PUB_LIST <<<"$PUBLISHERS"
IFS=',' read -r -a SUB_LIST <<<"$SUBSCRIBERS"

needs() {
    # needs <lang>: true if <lang> appears in either list.
    local lang="$1" x
    for x in "${PUB_LIST[@]}" "${SUB_LIST[@]}"; do [[ "$x" == "$lang" ]] && return 0; done
    return 1
}

MOQ="$REPO_ROOT/target/debug/moq-cli"
RELAY="$REPO_ROOT/target/debug/moq-relay"
TMP=$(mktemp -d)
RELAY_PID=""

kill_tree() {
    # SIGKILL, depth-first. moq-cli ignores SIGTERM (handles only SIGINT), so a
    # polite kill would leak it; these are ephemeral test processes, so -9 is fine.
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2>/dev/null || true); do kill_tree "$child"; done
    kill -KILL "$pid" 2>/dev/null || true
}

# shellcheck disable=SC2329  # invoked indirectly via 'trap cleanup EXIT'
cleanup() {
    # Reap the last publisher too; subscribers self-terminate via their timeouts.
    [[ -n "${PUB_PID:-}" ]] && kill_tree "$PUB_PID"
    [[ -n "$RELAY_PID" ]] && kill_tree "$RELAY_PID"
    rm -rf "$TMP"
}
trap cleanup EXIT

require_tools() {
    local missing=() t
    for t in cargo ffmpeg curl pgrep timeout; do
        command -v "$t" >/dev/null 2>&1 || missing+=("$t")
    done
    if needs python; then command -v uv >/dev/null 2>&1 || missing+=("uv"); fi
    if needs js-browser; then
        command -v bun >/dev/null 2>&1 || missing+=("bun")
        [[ -n "${PLAYWRIGHT_BROWSERS_PATH:-}" ]] || missing+=("PLAYWRIGHT_BROWSERS_PATH (run inside 'nix develop')")
    fi
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required tools: ${missing[*]}" >&2
        echo "       run inside 'nix develop' (or install them) and retry." >&2
        exit 1
    fi
}

# ── setup ───────────────────────────────────────────────────────────────────
require_tools

echo "building moq-relay + moq-cli..."
cargo build -q -p moq-relay -p moq-cli

if needs python; then
    echo "preparing python bindings..."
    # sync the workspace dev group (ruff/maturin/...) from py/, then build the
    # moq-ffi editable wheel (cdylib + uniffi bindings) and install the pure
    # python moq-rs wrapper (import `moq`) that depends on it.
    (cd "$REPO_ROOT/py" && uv sync --no-install-workspace)
    (cd "$REPO_ROOT/py/moq-ffi" && uv run --no-sync maturin develop --uv)
    (cd "$REPO_ROOT/py" && uv pip install --no-deps -e moq-rs)
fi

if needs js-browser; then
    echo "preparing browser harness..."
    (cd "$REPO_ROOT" && bun install)
    (cd "$REPO_ROOT/test/browser" && bunx vite build)
fi

if curl -sf "$URL/certificate.sha256" >/dev/null 2>&1; then
    echo "error: something is already listening on 127.0.0.1:4443 (stale relay?)" >&2
    exit 1
fi

echo "starting relay..."
"$RELAY" "$SMOKE_DIR/smoke.toml" >"$TMP/relay.log" 2>&1 &
RELAY_PID=$!
for _ in $(seq 1 60); do
    curl -sf "$URL/certificate.sha256" >/dev/null 2>&1 && break
    sleep 0.5
done
if ! curl -sf "$URL/certificate.sha256" >/dev/null 2>&1; then
    echo "relay never became ready" >&2
    sed 's/^/  relay: /' "$TMP/relay.log" >&2 || true
    exit 1
fi

# ── client dispatch ─────────────────────────────────────────────────────────
# Encode an endless H.264 Annex-B stream from a synthetic source to stdout.
# Paced with -re so the broadcast streams in real time until the reader closes.
# Baseline + repeat-headers re-emits SPS/PPS before every keyframe so a late
# subscriber (or the stream importer) can initialize without the first packet.
ffmpeg_h264() {
    ffmpeg -hide_banner -loglevel error -re -f lavfi -i "testsrc=size=${SIZE}:rate=${FPS}" \
        -an -c:v libx264 -profile:v baseline -preset ultrafast -pix_fmt yuv420p \
        -x264-params "keyint=${FPS}:min-keyint=${FPS}:scenecut=0:repeat-headers=1" \
        -f h264 -
}

# Sets global PUB_PID. Called in the current shell (no command substitution) so
# $! refers to the backgrounded job and kill_tree can reap the whole pipeline.
# Every publisher just consumes the same ffmpeg Annex-B stream on stdin; the
# client frames it (moq-cli / moq-ffi only frame-and-forward, ffmpeg encodes).
PUB_PID=""
start_publisher() {
    local lang="$1" broadcast="$2" log="$TMP/pub-$1.log"
    case "$lang" in
        rust)
            (ffmpeg_h264 | "$MOQ" publish --url "$URL" --broadcast "$broadcast" avc3) >"$log" 2>&1 &
            ;;
        python)
            (ffmpeg_h264 | (cd "$REPO_ROOT/py/moq-rs" && uv run --no-sync python examples/smoke.py \
                publish --url "$URL" --broadcast "$broadcast")) >"$log" 2>&1 &
            ;;
        js-browser)
            # Headless Chromium encodes its own H.264 from a fake camera via
            # WebCodecs (lazily, once a subscriber creates demand).
            (cd "$REPO_ROOT" && bun test/browser/driver.ts publish \
                --url "$URL" --broadcast "$broadcast") >"$log" 2>&1 &
            ;;
        *)
            echo "unknown publisher: $lang" >&2
            return 1
            ;;
    esac
    PUB_PID=$!
}

run_subscriber() {
    local lang="$1" broadcast="$2"
    case "$lang" in
        rust)
            # moq-cli only handles SIGINT, so -k forces SIGKILL if it ignores the
            # SIGTERM that fires when no data arrives within the timeout.
            local n
            n=$(timeout -k 3 "$TIMEOUT" "$MOQ" subscribe --url "$URL" --broadcast "$broadcast" \
                --format fmp4 2>/dev/null | head -c 1 | wc -c | tr -d ' ' || true)
            [[ "${n:-0}" -ge 1 ]]
            ;;
        python)
            (cd "$REPO_ROOT/py/moq-rs" && uv run --no-sync python examples/smoke.py \
                subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT")
            ;;
        js-browser)
            # Headless Chromium decodes via WebCodecs; exits 0 once a frame lands.
            (cd "$REPO_ROOT" && bun test/browser/driver.ts subscribe \
                --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT")
            ;;
        *)
            echo "unknown subscriber: $lang" >&2
            return 1
            ;;
    esac
}

# ── matrix ──────────────────────────────────────────────────────────────────
overall=0

run_round() {
    local pub="$1" broadcast="$2" pub_pid="$3"
    local pids=() names=() i sub
    # Let the publisher announce the broadcast + catalog before subscribers
    # connect. Native subscribers tolerate the race, but the browser watch gives
    # up on a catalog RESET_STREAM if it connects first.
    [[ -n "$pub_pid" ]] && sleep "$WARMUP"
    for sub in "${SUB_LIST[@]}"; do
        (run_subscriber "$sub" "$broadcast") >"$TMP/$pub-$sub.log" 2>&1 &
        pids+=("$!")
        names+=("$sub")
    done
    # A publisher that streams forever should still be alive; if it died, the
    # subscriber failures below are a publisher bug, so surface its log.
    if [[ -n "$pub_pid" ]] && ! kill -0 "$pub_pid" 2>/dev/null; then
        echo "  WARN  publisher '$pub' exited early:"
        sed 's/^/        /' "$TMP/pub-$pub.log" 2>/dev/null || true
    fi
    local want_pass=1 got
    [[ "$NEGATIVE" -eq 1 ]] && want_pass=0
    for i in "${!pids[@]}"; do
        if wait "${pids[$i]}"; then got=1; else got=0; fi
        if [[ "$got" -eq "$want_pass" ]]; then
            echo "  PASS  $pub -> ${names[$i]}"
        else
            echo "  FAIL  $pub -> ${names[$i]}"
            sed 's/^/        /' "$TMP/$pub-${names[$i]}.log" 2>/dev/null || true
            overall=1
        fi
    done
    if [[ -n "$pub_pid" ]]; then
        kill_tree "$pub_pid"
        wait "$pub_pid" 2>/dev/null || true
        # Don't let cleanup() later signal this now-reaped (possibly recycled) PID.
        [[ "${PUB_PID:-}" == "$pub_pid" ]] && PUB_PID=""
    fi
    return 0
}

if [[ "$NEGATIVE" -eq 1 ]]; then
    # Negative control: no publisher. Every subscriber must FAIL (time out with
    # no data), proving the harness can actually report failure.
    echo "=== negative control: subscribers expect NO data ==="
    run_round "none" "smoke-missing-$$-$RANDOM.hang" ""
else
    for pub in "${PUB_LIST[@]}"; do
        broadcast="smoke-${pub}-$$-${RANDOM}.hang"
        echo "=== publisher: $pub  broadcast: $broadcast ==="
        start_publisher "$pub" "$broadcast"
        run_round "$pub" "$broadcast" "$PUB_PID"
    done
fi

if [[ "$overall" -eq 0 ]]; then
    echo "smoke: all checks passed"
else
    echo "smoke: FAILURES detected" >&2
fi
exit "$overall"
