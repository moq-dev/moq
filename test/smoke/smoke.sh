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
# broadcast and confirms every subscriber sees data flowing before the timeout.
# The browser also verifies rendered WebCodecs output, player pause/resume, and
# audio when paired with the browser publisher.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$SMOKE_DIR/../.." && pwd)
CLIENTS="$SMOKE_DIR/clients"

# Run directory, reserved ports, and process-group ownership. See test/README.md.
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$SMOKE_DIR/../lib/harness.sh"

PUBLISHERS="rust"
SUBSCRIBERS="rust"
TIMEOUT="${SMOKE_TIMEOUT:-20}"
FPS="${SMOKE_FPS:-30}"
SIZE="${SMOKE_SIZE:-320x240}"
# Empty means "any reserved port"; SMOKE_PORT pins one instead.
PORT="${SMOKE_PORT:-}"
URL=""
NEGATIVE=0
MEDIA=0

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
        --media)
            MEDIA=1
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
[[ -z "$PORT" || "$PORT" =~ ^[0-9]+$ ]] || {
    echo "error: port must be numeric (got '$PORT')" >&2
    exit 2
}

# The media checks drive both roles from the browser client and never touch the matrix, so they
# pick their own axes rather than accepting --publishers / --subscribers.
if [[ "$MEDIA" -eq 1 ]]; then
    if [[ "$NEGATIVE" -eq 1 ]]; then
        echo "error: --media and --negative are separate runs" >&2
        exit 2
    fi
    PUBLISHERS="js"
    SUBSCRIBERS="js"
fi

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

RERUN="just test smoke --publishers $PUBLISHERS --subscribers $SUBSCRIBERS --timeout $TIMEOUT"
[[ "$NEGATIVE" -eq 1 ]] && RERUN="$RERUN --negative"
harness_begin smoke "$RERUN"

TARGET_BASE=""    # cargo target dir (resolved in require_tools)
PY=""             # python interpreter with the workspace moq build (set in prepare)
C_SMOKE=""        # compiled C client binary (set in prepare)
GO_SMOKE=""       # compiled Go client binary (set in prepare)
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

have() { command -v "$1" >/dev/null 2>&1; }

require_tools() {
    # The relay, CLI, ffmpeg, and harness essentials are hard requirements. A
    # missing per-client toolchain (uv / bun / node / cc) just marks that client
    # broken in prepare, so it fails its own cells instead of the whole run.
    local missing=() t
    for t in cargo ffmpeg curl timeout; do
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
    (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli) || {
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
    if (cd "$WORKSPACE" && just py build) >"$HARNESS_RUN/py-build.log" 2>&1; then
        PY="$WORKSPACE/.venv/bin/python"
        [[ -x "$PY" ]] || {
            mark_broken python "workspace .venv python not found after build"
            sed 's/^/        /' "$HARNESS_RUN/py-build.log" >&2 || true
        }
    else
        mark_broken python "just py build failed"
        sed 's/^/        /' "$HARNESS_RUN/py-build.log" >&2 || true
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
    if ! (cd "$WORKSPACE" && bun install --frozen-lockfile) >"$HARNESS_RUN/js-install.log" 2>&1; then
        for v in js js-native-node js-native-bun; do needs "$v" && mark_broken "$v" "bun install failed"; done
        sed 's/^/        /' "$HARNESS_RUN/js-install.log" >&2 || true
        return
    fi
    if needs js; then
        # Nix provides Chromium via PLAYWRIGHT_BROWSERS_PATH; otherwise fetch it.
        if [[ -z "${PLAYWRIGHT_BROWSERS_PATH:-}" ]] && ! (cd "$CLIENTS/js" && bunx playwright install chromium) >"$HARNESS_RUN/js-chromium.log" 2>&1; then
            mark_broken js "playwright chromium install failed"
            sed 's/^/        /' "$HARNESS_RUN/js-chromium.log" >&2 || true
        elif ! (cd "$CLIENTS/js" && bun run check) >"$HARNESS_RUN/js-check.log" 2>&1; then
            mark_broken js "type check failed"
            sed 's/^/        /' "$HARNESS_RUN/js-check.log" >&2 || true
        elif ! (cd "$CLIENTS/js" && bunx vite build) >"$HARNESS_RUN/js-vite.log" 2>&1; then
            mark_broken js "vite build failed"
            sed 's/^/        /' "$HARNESS_RUN/js-vite.log" >&2 || true
        fi
    fi
    if needs js-native-node && ! have node; then
        mark_broken js-native-node "node not found"
    fi
}

# Stage the Go modules from this checkout (go/scripts/stage.sh builds moq-ffi for
# the host, regenerates the bindings, and wires the wrapper to them by replace),
# then build the smoke client against that exact tree. The client is copied to a
# scratch dir first so the committed go.mod keeps its placeholder require; every
# dependency resolves to a local directory, so nothing hits the module proxy.
prepare_go() {
    have go || {
        mark_broken go "go not found"
        return
    }
    have uniffi-bindgen-go || {
        mark_broken go "uniffi-bindgen-go not found (see go/ffi/README.md)"
        return
    }
    echo "building go client (workspace moq-go via uniffi-bindgen-go)..."
    local staged ffi_pkg wrapper_pkg src="$TMP/go-client"
    if ! staged=$(bash "$WORKSPACE/go/scripts/stage.sh" 2>"$TMP/go-stage.log"); then
        mark_broken go "go/scripts/stage.sh failed"
        sed 's/^/        /' "$TMP/go-stage.log" >&2 || true
        return
    fi
    ffi_pkg=$(printf '%s\n' "$staged" | sed -n 1p)
    wrapper_pkg=$(printf '%s\n' "$staged" | sed -n 2p)
    mkdir -p "$src"
    cp "$CLIENTS/go/go.mod" "$CLIENTS/go/main.go" "$src/"
    GO_SMOKE="$TMP/go-smoke"
    if ! (
        cd "$src"
        export CGO_ENABLED=1 GOFLAGS=-mod=mod
        go mod edit \
            -replace="github.com/moq-dev/moq-go=$wrapper_pkg" \
            -replace="github.com/moq-dev/moq-go-ffi=$ffi_pkg"
        go build -o "$GO_SMOKE" .
    ) >"$TMP/go-build.log" 2>&1; then
        mark_broken go "go build failed"
        sed 's/^/        /' "$TMP/go-build.log" >&2 || true
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
    if ! (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p libmoq) >"$HARNESS_RUN/c-build.log" 2>&1; then
        mark_broken c "cargo build -p libmoq failed"
        sed 's/^/        /' "$HARNESS_RUN/c-build.log" >&2 || true
        return
    fi
    header="$TARGET_BASE/include/moq.h"
    lib="$TARGET_BASE/$PROFILE/libmoq.a"
    [[ -f "$header" && -f "$lib" ]] || {
        mark_broken c "libmoq artifacts missing ($header / $lib)"
        return
    }
    # cargo can't inject libmoq.a's native deps into an external link, so read
    # them from the same list build.rs and CMake use.
    local native_libs
    case "$(uname -s)" in
        Darwin) native_libs="$WORKSPACE/rs/libmoq/native-libs/apple.txt" ;;
        *) native_libs="$WORKSPACE/rs/libmoq/native-libs/linux.txt" ;;
    esac
    os_libs=()
    while read -r entry; do
        case "$entry" in
            '' | '#'*) continue ;;
            framework:*) os_libs+=(-framework "${entry#framework:}") ;;
            *) os_libs+=("-l$entry") ;;
        esac
    done <"$native_libs"
    C_SMOKE="$HARNESS_RUN/c-smoke"
    if ! "$cc" "$CLIENTS/c/subscribe.c" -I"$TARGET_BASE/include" -L"$TARGET_BASE/$PROFILE" -lmoq "${os_libs[@]}" -o "$C_SMOKE" >"$HARNESS_RUN/c-compile.log" 2>&1; then
        mark_broken c "cc compile failed"
        sed 's/^/        /' "$HARNESS_RUN/c-compile.log" >&2 || true
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
    if ! (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-gst) >"$HARNESS_RUN/gst-build.log" 2>&1; then
        mark_broken gst "cargo build -p moq-gst failed"
        sed 's/^/        /' "$HARNESS_RUN/gst-build.log" >&2 || true
        return
    fi
    GST_PLUGIN_DIR="$TARGET_BASE/$PROFILE"
    # gst-inspect exits 0 even when the .so fails to load, so grep for the
    # factory. Isolate discovery to our dir + a temp registry so a system-wide moq
    # plugin can't shadow it (mirrors rs/moq-gst/smoke.sh).
    if ! GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_PLUGIN_SYSTEM_PATH_1_0="" \
        GST_REGISTRY_1_0="$HARNESS_RUN/gst-registry.bin" \
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
needs go && prepare_go
needs_js && prepare_js
needs c && prepare_c
needs gst && prepare_gst

# Held for the rest of the run, so a concurrent harness cannot pick the same
# number between here and the relay's bind.
harness_port relay "$PORT"
PORT="$HARNESS_PORT"
URL="http://127.0.0.1:${PORT}"

# The reservation covers other harness runs, not the rest of the machine, so
# still refuse a port some unrelated process is already serving on.
if curl -sf "$URL/certificate.sha256" >/dev/null 2>&1; then
    echo "error: something is already listening on 127.0.0.1:${PORT} (stale relay?)" >&2
    exit 1
fi

echo "starting relay on 127.0.0.1:${PORT}..."
# smoke.toml is the source of truth; rewrite its port into a scratch copy so the
# committed file never has to be edited for a run.
sed "s/4443/${PORT}/g" "$SMOKE_DIR/smoke.toml" >"$HARNESS_RUN/relay.toml"
harness_spawn relay "$HARNESS_RUN/relay.log" "$RELAY" "$HARNESS_RUN/relay.toml"
if ! harness_ready "$URL/certificate.sha256" 30; then
    echo "relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi
harness_endpoint relay "$URL"

# ── client dispatch ─────────────────────────────────────────────────────────
# Encode an endless H.264 Annex-B stream from a synthetic source to stdout.
# Paced with -re so the broadcast streams in real time until the reader closes.
# Baseline + repeat-headers re-emits SPS/PPS before every keyframe so a late
# subscriber (or the stream importer) can initialize without the first packet.
# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
ffmpeg_h264() {
    ffmpeg -hide_banner -loglevel error -re -f lavfi -i "testsrc=size=${SIZE}:rate=${FPS}" \
        -an -c:v libx264 -profile:v baseline -preset ultrafast -pix_fmt yuv420p \
        -x264-params "keyint=${FPS}:min-keyint=${FPS}:scenecut=0:repeat-headers=1" \
        -f h264 -
}

# The publisher pipeline, run in a process group of its own by `harness_spawn`
# so reaping it takes ffmpeg with it. Every non-browser publisher consumes the
# same ffmpeg Annex-B stream on stdin; the client frames it (moq-cli / the FFI
# importers only frame-and-forward).
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
run_publisher() {
    local lang="$1" broadcast="$2"
    case "$lang" in
        rust)
            ffmpeg_h264 | "$MOQ" --client-connect "$URL" --broadcast "$broadcast" import avc3
            ;;
        python)
            ffmpeg_h264 | "$PY" "$CLIENTS/python/smoke.py" \
                publish --url "$URL" --broadcast "$broadcast"
            ;;
        go)
            (ffmpeg_h264 | "$GO_SMOKE" publish --url "$URL" --broadcast "$broadcast") >"$log" 2>&1 &
            ;;
        js)
            # Headless Chromium encodes its own H.264 from a fake camera via
            # WebCodecs (lazily, once a subscriber creates demand).
            cd "$CLIENTS/js" && bun driver.ts publish \
                --url "$URL" --broadcast "$broadcast"
            ;;
        *)
            echo "unknown publisher: $lang" >&2
            return 1
            ;;
    esac
}

# Sets global PUB_PID to the publisher's process group leader.
PUB_PID=""
start_publisher() {
    local lang="$1" broadcast="$2"
    harness_spawn "pub-$lang" "$HARNESS_RUN/pub-$lang.log" run_publisher "$lang" "$broadcast"
    PUB_PID="$HARNESS_PID"
}

# Run a native-JS subscriber and judge it by the "received N bytes" marker it
# prints, not its exit code. The @moq/web-transport NAPI addon can segfault
# during the runtime's exit teardown *after* a frame has arrived (an upstream bug
# under bun), which would turn a real success into a signal exit. The data path
# is what we test, so a printed marker is the verdict; the crash is swallowed.
# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
run_native() {
    local out
    out=$( (cd "$CLIENTS/js-native" && "$@") 2>&1) || true
    printf '%s\n' "$out" >&2
    printf '%s\n' "$out" | grep -q '^received '
}

# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
run_subscriber() {
    local lang="$1" broadcast="$2" publisher="${3:-}"
    case "$lang" in
        rust)
            # moq-cli only handles SIGINT, so -k forces SIGKILL if it ignores the
            # SIGTERM that fires when no data arrives within the timeout.
            local n
            n=$(timeout -k 3 "$TIMEOUT" "$MOQ" --client-connect "$URL" --broadcast "$broadcast" \
                export fmp4 | head -c 1 | wc -c | tr -d ' ' || true)
            [[ "${n:-0}" -ge 1 ]]
            ;;
        python)
            "$PY" "$CLIENTS/python/smoke.py" \
                subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        go)
            "$GO_SMOKE" subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        c)
            "$C_SMOKE" subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        gst)
            # moqsrc exposes each rendition as a Sometimes pad (video_%u / audio_%u),
            # named so the first of each kind is always video_0 / audio_0. Link
            # video_0 by name: a bare `moqsrc ! filesink` would take whichever pad
            # appears first, so a publisher with audio (the browser) could pass this
            # cell on audio bytes without video ever flowing. We grab one byte, the
            # same "bytes moved" bar as the rust subscriber (no decode). head closing
            # the pipe SIGPIPEs gst-launch, so success returns at once; no data just
            # runs out the timeout. Our plugin dir rides on top of the system path
            # (which provides filesink); a private registry keeps the scan off the
            # user's cache. buffer-mode=2 makes filesink unbuffered so the first frame
            # reaches head immediately.
            local n
            n=$(GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_REGISTRY_1_0="$HARNESS_RUN/gst-run-registry.bin" \
                timeout -k 3 "$TIMEOUT" gst-launch-1.0 -q \
                moqsrc name=s url="$URL" broadcast="$broadcast" \
                s.video_0 ! filesink location=/dev/stdout buffer-mode=2 \
                2>/dev/null | head -c 1 | wc -c | tr -d ' ' || true)
            [[ "${n:-0}" -ge 1 ]]
            ;;
        js)
            # Headless Chromium decodes and renders via WebCodecs, then drives
            # the real player's pause/resume controls. Browser publishers also
            # provide fake microphone input, so validate audio in that cell.
            if [[ "$publisher" == "js" ]]; then
                (cd "$CLIENTS/js" && bun driver.ts subscribe \
                    --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT" --expect-audio)
            else
                (cd "$CLIENTS/js" && bun driver.ts subscribe \
                    --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT")
            fi
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

# One matrix cell, run in its own process group so cancelling the round reaps
# whatever the subscriber spawned (a browser, a gst pipeline) along with it.
#
# Records how long the cell took. Every subscriber shares one budget, so the
# spread is the diagnostic: a cell that burns the whole timeout while its
# siblings finish in a couple of seconds is stalled, not merely slow, and one
# creeping up on $TIMEOUT is a near-miss worth seeing before it fails.
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
run_cell() {
    local pub="$1" sub="$2" broadcast="$3" started=$SECONDS status=0
    # `|| status=$?` rather than a bare call: under `set -e` a failing subscriber
    # would exit before it recorded anything, and a failure is exactly when the
    # duration is worth reading.
    run_subscriber "$sub" "$broadcast" "$pub" || status=$?
    echo "$((SECONDS - started))" >"$HARNESS_RUN/$pub-$sub.secs"
    return "$status"
}

run_round() {
    local pub="$1" broadcast="$2" pub_pid="$3"
    local pids=() names=() i sub
    for sub in "${SUB_LIST[@]}"; do
        if is_broken "$sub"; then
            echo "  FAIL  $pub -> $sub (subscriber client unavailable)"
            overall=1
            continue
        fi
        harness_spawn "$pub-$sub" "$HARNESS_RUN/$pub-$sub.log" run_cell "$pub" "$sub" "$broadcast"
        pids+=("$HARNESS_PID")
        names+=("$sub")
    done
    # A publisher that streams forever should still be alive; if it died, the
    # subscriber failures below are a publisher bug, so surface its log.
    if [[ -n "$pub_pid" ]] && ! kill -0 "$pub_pid" 2>/dev/null; then
        echo "  WARN  publisher '$pub' exited early:"
        sed 's/^/        /' "$HARNESS_RUN/pub-$pub.log" 2>/dev/null || true
    fi
    local want_pass=1 got round_pass=0 elapsed
    [[ "$NEGATIVE" -eq 1 ]] && want_pass=0
    # ${arr[@]+...} guard: a round may have no live subscribers (all broken),
    # and bash 3.2 (macOS) errors on "${!pids[@]}" for an empty array under `set -u`.
    for i in ${pids[@]+"${!pids[@]}"}; do
        if harness_wait "${pids[$i]}"; then got=1; else got=0; fi
        elapsed=$(cat "$HARNESS_RUN/$pub-${names[$i]}.secs" 2>/dev/null || echo "?")
        if [[ "$got" -eq "$want_pass" ]]; then
            echo "  PASS  $pub -> ${names[$i]} (${elapsed}s)"
            round_pass=1
        else
            echo "  FAIL  $pub -> ${names[$i]} (${elapsed}s of ${TIMEOUT}s)"
            sed 's/^/        /' "$HARNESS_RUN/$pub-${names[$i]}.log" 2>/dev/null || true
            overall=1
        fi
    done
    # Every leg failing points at the publisher; surface its log even when the
    # process is still alive (e.g. connected and announcing but producing nothing).
    if [[ "$NEGATIVE" -eq 0 && "$round_pass" -eq 0 && ${#pids[@]} -gt 0 && -n "$pub_pid" ]]; then
        echo "  INFO  publisher '$pub' log:"
        sed 's/^/        /' "$HARNESS_RUN/pub-$pub.log" 2>/dev/null || true
    fi
    # `harness_reap` retires the entry, so teardown never signals this now-reaped
    # (possibly recycled) PID again.
    if [[ -n "$pub_pid" ]]; then
        harness_reap "$pub_pid"
    fi
    return 0
}

# One media.ts invocation. It reports its own verdict (a negative control passes by failing on the
# assertion it names), so the exit code is the whole answer.
run_media() {
    local name="$1" log started status=0
    shift
    log="$TMP/media-${name// /-}.log"
    started=$SECONDS
    (cd "$CLIENTS/js" && bun media.ts --url "$URL" --timeout "$TIMEOUT" "$@") >"$log" 2>&1 || status=$?
    if [[ "$status" -eq 0 ]]; then
        echo "  PASS  $name ($((SECONDS - started))s)"
        # The measurements are the point even when nothing fails: a skew or frame rate creeping
        # toward its bound is worth seeing before it crosses.
        grep -E '^(  |=== )' "$log" || true
    else
        echo "  FAIL  $name ($((SECONDS - started))s)"
        sed 's/^/        /' "$log" >&2 || true
        overall=1
    fi
}

if [[ "$MEDIA" -eq 1 ]]; then
    # Media output and lifecycle, browser to browser, against the deterministic fixture. The
    # negative controls below inject a defect and name the assertion that has to catch it; each
    # passes only by failing there, which is what keeps the positive run from being vacuous.
    if is_broken js; then
        echo "  FAIL  media checks (browser client unavailable)"
        overall=1
    else
        echo "=== media output and lifecycle ==="
        run_media "media output + lifecycle"
        run_media "control: frozen video" --fault frozen-video --cases none --expect-fail "video progress"
        run_media "control: silent audio" --fault silent-audio --cases none --expect-fail "audio tone"
        run_media "control: offset audio" --fault audio-offset --cases none --expect-fail "audio/video sync"
        run_media "control: leaked session" --leak --cases detach --expect-fail "resource baseline"
    fi
elif [[ "$NEGATIVE" -eq 1 ]]; then
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
    # The relay's view is often the only place that says WHY a session died
    # (auth rejection, protocol error, close codes), so surface it on failure.
    echo "smoke: FAILURES detected" >&2
    echo "--- relay log (last 150 lines) ---" >&2
    tail -n 150 "$HARNESS_RUN/relay.log" 2>/dev/null | sed 's/^/  relay: /' >&2 || true
fi
exit "$overall"
