#!/usr/bin/env bash
# Cross-language media interop smoke test against THIS checkout.
#
# Unlike the standalone moq-dev/smoke repo (which installs each client from its
# public registry to catch packaging breakage), this builds every client from
# the workspace source. It proves the code in the tree interoperates across
# implementations before anything is published: a relay built from rs/moq-relay,
# clients built from rs/moq-cli, py/, js/, and rs/libmoq, all talking to each
# other. There's no apt/brew/npm/PyPI here, just cargo/bun/uv/cc.
#
# It stands up a moq-relay, then for each publisher language publishes an H.264
# broadcast and confirms every subscriber sees data flowing (a non-empty frame
# before the timeout). We check that bytes move end-to-end across
# implementations, not that H.264 decodes.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$SMOKE_DIR/../.." && pwd)
CLIENTS="$SMOKE_DIR/clients"

PUBLISHERS="rust"
SUBSCRIBERS="rust"
TIMEOUT="${SMOKE_TIMEOUT:-20}"
FPS="${SMOKE_FPS:-30}"
SIZE="${SMOKE_SIZE:-320x240}"
PORT="${SMOKE_PORT:-4443}"
URL="http://127.0.0.1:${PORT}"
NEGATIVE=0

# Cargo profile for the relay/cli/libmoq builds. Debug compiles faster, which is
# what a smoke test wants; the workload (320x240@30) is trivial either way.
PROFILE="${SMOKE_PROFILE:-debug}"

# Binaries under test. Built from source below unless overridden to point at a
# prebuilt (mirrors the standalone smoke repo's RELAY_BIN/MOQ_BIN escape hatch).
RELAY="${RELAY_BIN:-}"
MOQ="${MOQ_BIN:-}"

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

# Numeric guards so a fat-fingered --timeout / SMOKE_PORT fails clearly here
# instead of surfacing later as a cryptic `timeout` or relay-bind error.
[[ "$TIMEOUT" =~ ^[0-9]+(\.[0-9]+)?$ ]] || {
    echo "error: timeout must be a positive number (got '$TIMEOUT')" >&2
    exit 2
}
[[ "$PORT" =~ ^[0-9]+$ ]] || {
    echo "error: port must be numeric (got '$PORT')" >&2
    exit 2
}

IFS=',' read -r -a PUB_LIST <<<"$PUBLISHERS"
IFS=',' read -r -a SUB_LIST <<<"$SUBSCRIBERS"

needs() {
    # needs <lang>: true if <lang> appears in either list.
    local lang="$1" x
    for x in "${PUB_LIST[@]}" "${SUB_LIST[@]}"; do [[ "$x" == "$lang" ]] && return 0; done
    return 1
}

# True if any browser/native JS client is in play (they share one bun install).
needs_js() {
    needs js || needs js-native-node || needs js-native-bun
}

TMP=$(mktemp -d)
RELAY_PID=""
TARGET_BASE=""    # cargo target dir (resolved in require_tools)
PY=""             # python interpreter with the workspace moq build (set in prepare)
C_SMOKE=""        # compiled C client binary (set in prepare)
GST_PLUGIN_DIR="" # dir holding the built moq-gst plugin (set in prepare)
BROKEN_LANGS=""   # clients whose source build failed

mark_broken() {
    # A client whose source build fails fails only its own matrix cells instead
    # of aborting the whole run, so one broken binding still lets the rest report.
    BROKEN_LANGS="$BROKEN_LANGS $1"
    echo "  WARN  $1 client unavailable: $2"
}

is_broken() {
    local lang="$1" x
    for x in $BROKEN_LANGS; do [[ "$x" == "$lang" ]] && return 0; done
    return 1
}

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

have() { command -v "$1" >/dev/null 2>&1; }

require_tools() {
    # The relay, CLI, ffmpeg, and harness essentials are hard requirements. A
    # missing per-client toolchain (uv / bun / node / cc) just marks that client
    # broken in prepare, so it fails its own cells instead of the whole run.
    local missing=() t
    for t in cargo ffmpeg curl pgrep timeout; do
        have "$t" || missing+=("$t")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required tools: ${missing[*]}" >&2
        exit 1
    fi
    # Resolve the cargo target dir once (honors a custom CARGO_TARGET_DIR, which
    # the self-hosted CI runner sets), so the built binaries and libmoq's header
    # are found wherever cargo actually writes them.
    TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE/Cargo.toml" --no-deps |
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
    [[ -n "$TARGET_BASE" ]] || {
        echo "error: could not resolve cargo target directory" >&2
        exit 1
    }
}

# Build moq-relay + moq-cli from the workspace. The relay is the spine of the
# test, so a failure here aborts rather than marking a single client broken.
build_relay_cli() {
    local flag=()
    [[ "$PROFILE" == "release" ]] && flag=(--release)
    echo "building moq-relay + moq-cli ($PROFILE)..."
    # ${arr[@]+...} guard: bash 3.2 (macOS /bin/bash) errors on "${flag[@]}" for
    # an empty (debug) array under `set -u`.
    (cd "$WORKSPACE" && cargo build ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli) || {
        echo "error: failed to build moq-relay / moq-cli" >&2
        exit 1
    }
    [[ -n "$RELAY" ]] || RELAY="$TARGET_BASE/$PROFILE/moq-relay"
    # The `moq-cli` crate ships its binary as `moq` (a `[[bin]]` override).
    [[ -n "$MOQ" ]] || MOQ="$TARGET_BASE/$PROFILE/moq"
}

# Editable-install the workspace Python build (maturin builds rs/moq-ffi, then
# the moq-rs wrapper installs on top) into the repo-root .venv. `import moq`
# then resolves to this checkout, not a PyPI wheel.
prepare_python() {
    have uv || {
        mark_broken python "uv not found"
        return
    }
    echo "building python client (workspace moq via maturin)..."
    if (cd "$WORKSPACE" && just py build) >"$TMP/py-build.log" 2>&1; then
        PY="$WORKSPACE/.venv/bin/python"
        [[ -x "$PY" ]] || {
            mark_broken python "workspace .venv python not found after build"
            sed 's/^/        /' "$TMP/py-build.log" >&2 || true
        }
    else
        mark_broken python "just py build failed"
        sed 's/^/        /' "$TMP/py-build.log" >&2 || true
    fi
}

# Link the JS workspace (the smoke clients are bun workspace members, so the
# @moq/* packages resolve to this checkout's source) and build the browser page.
prepare_js() {
    have bun || {
        for v in js js-native-node js-native-bun; do needs "$v" && mark_broken "$v" "bun not found"; done
        return
    }
    echo "installing js clients (workspace @moq/* via bun)..."
    if ! (cd "$WORKSPACE" && bun install) >"$TMP/js-install.log" 2>&1; then
        for v in js js-native-node js-native-bun; do needs "$v" && mark_broken "$v" "bun install failed"; done
        sed 's/^/        /' "$TMP/js-install.log" >&2 || true
        return
    fi
    if needs js; then
        # Nix provides Chromium via PLAYWRIGHT_BROWSERS_PATH; otherwise fetch it.
        if [[ -z "${PLAYWRIGHT_BROWSERS_PATH:-}" ]] && ! (cd "$CLIENTS/js" && bunx playwright install chromium) >"$TMP/js-chromium.log" 2>&1; then
            mark_broken js "playwright chromium install failed"
            sed 's/^/        /' "$TMP/js-chromium.log" >&2 || true
        elif ! (cd "$CLIENTS/js" && bunx vite build) >"$TMP/js-vite.log" 2>&1; then
            mark_broken js "vite build failed"
            sed 's/^/        /' "$TMP/js-vite.log" >&2 || true
        fi
    fi
    if needs js-native-node && ! have node; then
        mark_broken js-native-node "node not found"
    fi
}

# Build libmoq (the C staticlib + cbindgen header) and compile the C subscriber
# against it. cargo writes moq.h to $TARGET_BASE/include and libmoq.a to the
# profile dir.
prepare_c() {
    local cc="${CC:-cc}" header lib os_libs
    have "$cc" || {
        mark_broken c "no C compiler ($cc) on PATH"
        return
    }
    echo "building c client (workspace libmoq + cc)..."
    local flag=()
    [[ "$PROFILE" == "release" ]] && flag=(--release)
    if ! (cd "$WORKSPACE" && cargo build ${flag[@]+"${flag[@]}"} -p libmoq) >"$TMP/c-build.log" 2>&1; then
        mark_broken c "cargo build -p libmoq failed"
        sed 's/^/        /' "$TMP/c-build.log" >&2 || true
        return
    fi
    header="$TARGET_BASE/include/moq.h"
    lib="$TARGET_BASE/$PROFILE/libmoq.a"
    [[ -f "$header" && -f "$lib" ]] || {
        mark_broken c "libmoq artifacts missing ($header / $lib)"
        return
    }
    case "$(uname -s)" in
        Darwin) os_libs=(-framework CoreFoundation -framework Security -framework CoreServices) ;;
        *) os_libs=(-ldl -lm -lpthread) ;;
    esac
    C_SMOKE="$TMP/c-smoke"
    if ! "$cc" "$CLIENTS/c/subscribe.c" -I"$TARGET_BASE/include" -L"$TARGET_BASE/$PROFILE" -lmoq "${os_libs[@]}" -o "$C_SMOKE" >"$TMP/c-compile.log" 2>&1; then
        mark_broken c "cc compile failed"
        sed 's/^/        /' "$TMP/c-compile.log" >&2 || true
    fi
}

# Build the moq-gst plugin and confirm it loads against the GStreamer in the
# environment. moqsrc links the host's libgstreamer, so this wants a real
# GStreamer with the core plugins (the nix devShell ships gstreamer + base/good;
# a bare shell without it marks gst unavailable). Sets GST_PLUGIN_DIR to the dir
# holding libgstmoq.{so,dylib}. Subscribe only: moqsink publishing needs an
# encoder + request-pad muxing this client doesn't drive.
prepare_gst() {
    have gst-launch-1.0 || {
        mark_broken gst "gst-launch-1.0 not on PATH (needs a system GStreamer)"
        return
    }
    have gst-inspect-1.0 || {
        mark_broken gst "gst-inspect-1.0 not on PATH"
        return
    }
    echo "building gstreamer client (workspace moq-gst plugin)..."
    local flag=()
    [[ "$PROFILE" == "release" ]] && flag=(--release)
    if ! (cd "$WORKSPACE" && cargo build ${flag[@]+"${flag[@]}"} -p moq-gst) >"$TMP/gst-build.log" 2>&1; then
        mark_broken gst "cargo build -p moq-gst failed"
        sed 's/^/        /' "$TMP/gst-build.log" >&2 || true
        return
    fi
    GST_PLUGIN_DIR="$TARGET_BASE/$PROFILE"
    # gst-inspect exits 0 even when the .so fails to load, so grep for the
    # factory. Isolate discovery to our dir + a temp registry so a system-wide moq
    # plugin can't shadow it (mirrors rs/moq-gst/smoke.sh).
    if ! GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_PLUGIN_SYSTEM_PATH_1_0="" \
        GST_REGISTRY_1_0="$TMP/gst-registry.bin" \
        gst-inspect-1.0 moq 2>/dev/null | grep -qE '^[[:space:]]+moqsrc:'; then
        mark_broken gst "moqsrc not exposed (plugin failed to load against this GStreamer)"
    fi
}

# ── setup ───────────────────────────────────────────────────────────────────
require_tools
build_relay_cli

echo "relay:   $RELAY"
echo "moq-cli: $MOQ"

needs python && prepare_python
needs_js && prepare_js
needs c && prepare_c
needs gst && prepare_gst

if curl -sf "$URL/certificate.sha256" >/dev/null 2>&1; then
    echo "error: something is already listening on 127.0.0.1:${PORT} (stale relay?)" >&2
    exit 1
fi

echo "starting relay on 127.0.0.1:${PORT}..."
# smoke.toml is the source of truth; rewrite its port into a scratch copy so a
# busy 4443 (a dev relay, a parallel run) doesn't require editing the committed file.
sed "s/4443/${PORT}/g" "$SMOKE_DIR/smoke.toml" >"$TMP/relay.toml"
"$RELAY" "$TMP/relay.toml" >"$TMP/relay.log" 2>&1 &
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
# Every non-browser publisher consumes the same ffmpeg Annex-B stream on stdin;
# the client frames it (moq-cli / the FFI importers only frame-and-forward).
PUB_PID=""
start_publisher() {
    local lang="$1" broadcast="$2" log="$TMP/pub-$1.log"
    case "$lang" in
        rust)
            (ffmpeg_h264 | "$MOQ" --client-connect "$URL" --broadcast "$broadcast" import avc3) >"$log" 2>&1 &
            ;;
        python)
            (ffmpeg_h264 | "$PY" "$CLIENTS/python/smoke.py" \
                publish --url "$URL" --broadcast "$broadcast") >"$log" 2>&1 &
            ;;
        js)
            # Headless Chromium encodes its own H.264 from a fake camera via
            # WebCodecs (lazily, once a subscriber creates demand).
            (cd "$CLIENTS/js" && bun driver.ts publish \
                --url "$URL" --broadcast "$broadcast") >"$log" 2>&1 &
            ;;
        *)
            echo "unknown publisher: $lang" >&2
            return 1
            ;;
    esac
    PUB_PID=$!
}

# Run a native-JS subscriber and judge it by the "received N bytes" marker it
# prints, not its exit code. The @moq/web-transport NAPI addon can segfault
# during the runtime's exit teardown *after* a frame has arrived (an upstream bug
# under bun), which would turn a real success into a signal exit. The data path
# is what we test, so a printed marker is the verdict; the crash is swallowed.
run_native() {
    local out
    out=$( (cd "$CLIENTS/js-native" && "$@") 2>&1) || true
    printf '%s\n' "$out" >&2
    printf '%s\n' "$out" | grep -q '^received '
}

run_subscriber() {
    local lang="$1" broadcast="$2"
    case "$lang" in
        rust)
            # moq-cli only handles SIGINT, so -k forces SIGKILL if it ignores the
            # SIGTERM that fires when no data arrives within the timeout.
            local n
            n=$(timeout -k 3 "$TIMEOUT" "$MOQ" --client-connect "$URL" --broadcast "$broadcast" \
                export fmp4 2>/dev/null | head -c 1 | wc -c | tr -d ' ' || true)
            [[ "${n:-0}" -ge 1 ]]
            ;;
        python)
            "$PY" "$CLIENTS/python/smoke.py" \
                subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        c)
            "$C_SMOKE" subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        gst)
            # moqsrc exposes the broadcast's video as a Sometimes pad (video_%u);
            # gst-launch links it to filesink once it appears. We grab one byte,
            # the same "bytes moved" bar as the rust subscriber (no decode). head
            # closing the pipe SIGPIPEs gst-launch, so success returns at once; no
            # data just runs out the timeout. Our plugin dir rides on top of the
            # system path (which provides filesink); a private registry keeps the
            # scan off the user's cache. buffer-mode=2 makes filesink unbuffered so
            # the first frame reaches head immediately.
            local n
            n=$(GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_REGISTRY_1_0="$TMP/gst-run-registry.bin" \
                timeout -k 3 "$TIMEOUT" gst-launch-1.0 -q \
                moqsrc url="$URL" broadcast="$broadcast" ! filesink location=/dev/stdout buffer-mode=2 \
                2>/dev/null | head -c 1 | wc -c | tr -d ' ' || true)
            [[ "${n:-0}" -ge 1 ]]
            ;;
        js)
            # Headless Chromium decodes via WebCodecs; exits 0 once a frame lands.
            (cd "$CLIENTS/js" && bun driver.ts subscribe \
                --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT")
            ;;
        js-native-bun)
            # Native @moq/net via moq's WebTransport polyfill, under bun.
            run_native bun subscribe.ts subscribe \
                --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        js-native-node)
            # Same, under node (tsx runs the TS directly).
            run_native node --import tsx subscribe.ts subscribe \
                --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
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
    for sub in "${SUB_LIST[@]}"; do
        if is_broken "$sub"; then
            echo "  FAIL  $pub -> $sub (subscriber client unavailable)"
            overall=1
            continue
        fi
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
    # ${arr[@]+...} guard: a round may have no live subscribers (all broken),
    # and bash 3.2 (macOS) errors on "${!pids[@]}" for an empty array under `set -u`.
    for i in ${pids[@]+"${!pids[@]}"}; do
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
        if is_broken "$pub"; then
            for sub in "${SUB_LIST[@]}"; do
                echo "  FAIL  $pub -> $sub (publisher client unavailable)"
            done
            overall=1
            continue
        fi
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
