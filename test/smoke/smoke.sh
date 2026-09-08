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

# Every run writes into a debug bundle instead of a temp dir, so a failure keeps
# its logs, configs, browser traces, and stacks. See test/README.md.
# shellcheck source=../lib/bundle.sh disable=SC1091
source "$WORKSPACE/test/lib/bundle.sh"

PUBLISHERS="rust"
SUBSCRIBERS="rust"
TIMEOUT="${SMOKE_TIMEOUT:-20}"
FPS="${SMOKE_FPS:-30}"
SIZE="${SMOKE_SIZE:-320x240}"
PORT="${SMOKE_PORT:-4443}"
URL="http://127.0.0.1:${PORT}"
NEGATIVE=0
MEDIA=0

# Fault injection for the debug-bundle drills: each mode breaks the run in one
# of the ways a real failure arrives, so the evidence path can be exercised on
# demand rather than only when something is genuinely broken.
#   browser     the browser subscriber fails an assertion mid-playback
#   relay       the relay is killed while the matrix is running
#   hang        no publisher starts, so every subscriber runs out its timeout
FAULT="${MOQ_QA_FAULT:-}"

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
[[ "$PORT" =~ ^[0-9]+$ ]] || {
    echo "error: port must be numeric (got '$PORT')" >&2
    exit 2
}
case "$FAULT" in
    "" | browser | relay | hang) ;;
    *)
        echo "error: MOQ_QA_FAULT must be browser, relay, or hang (got '$FAULT')" >&2
        exit 2
        ;;
esac

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

rerun=(just test smoke --publishers "$PUBLISHERS" --subscribers "$SUBSCRIBERS" --timeout "$TIMEOUT")
[[ "$NEGATIVE" -eq 0 ]] || rerun+=(--negative)
bundle_init smoke
bundle_rerun "${rerun[@]}"
[[ -z "$FAULT" ]] || bundle_note "fault injection: MOQ_QA_FAULT=$FAULT"

# The harness scratch lives in the bundle, so the per-process logs, the relay
# config, and the timings are retained by construction. Build products go to the
# scratch dir instead: they are large, and the binaries they came from are
# recorded by identity below.
TMP="$BUNDLE_WORK"
RELAY_PID=""
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

kill_tree() {
    # SIGKILL, depth-first. moq-cli ignores SIGTERM (handles only SIGINT), so a
    # polite kill would leak it; these are ephemeral test processes, so -9 is fine.
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2>/dev/null || true); do kill_tree "$child"; done
    kill -KILL "$pid" 2>/dev/null || true
}

# shellcheck disable=SC2329  # invoked indirectly via 'trap cleanup EXIT'
cleanup() {
    local status=$?

    # First, before any of the reaping below: the fault timer holds a PID it is
    # about to SIGKILL, and that PID stops being the relay the moment we reap it.
    if [[ -n "${FAULT_PID:-}" ]]; then
        kill_tree "$FAULT_PID"
        wait "$FAULT_PID" 2>/dev/null || true
        FAULT_PID=""
    fi

    # Whatever is still running here never finished: a relay that wedged, a
    # publisher that stopped producing. Their stacks are the only thing left
    # that says where, and reaping them is what destroys it, so read first.
    if [[ "$status" -ne 0 ]]; then
        [[ -z "$RELAY_PID" ]] || bundle_stack relay "$RELAY_PID"
        [[ -z "${PUB_PID:-}" ]] || bundle_stack publisher "$PUB_PID"
        if [[ -n "${MOQ_QA_QLOG:-}" ]] && [[ -z "$(ls -A "$BUNDLE_QLOG" 2>/dev/null)" ]]; then
            bundle_capability qlog "requested, but the relay wrote no traces: this backend cannot capture them"
        fi
    fi

    # A retained session is the whole point of MOQ_QA_RETAIN: the ports stay
    # held and the processes stay attachable until teardown.sh runs.
    if [[ "$status" -eq 0 ]] || ! bundle_retained; then
        # Reap the last publisher too; subscribers self-terminate via their timeouts.
        # `wait` after each kill consumes the job status: bundle_finish below runs
        # long enough for the shell to otherwise report "Killed" on its own, which
        # reads like a failure in a run that passed.
        if [[ -n "${PUB_PID:-}" ]]; then
            kill_tree "$PUB_PID"
            wait "$PUB_PID" 2>/dev/null || true
        fi
        if [[ -n "$RELAY_PID" ]]; then
            kill_tree "$RELAY_PID"
            wait "$RELAY_PID" 2>/dev/null || true
        fi
    fi
    bundle_finish "$status"
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
    # qlog is a cargo feature, so capturing traces means building a different
    # relay. Opt-in for that reason: it recompiles quinn with the qlog encoder,
    # which a run that is only going to pass has no use for.
    [[ -z "${MOQ_QA_QLOG:-}" ]] || flag+=(--features moq-relay/qlog)
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
    # Which binaries the stacks and logs below came out of: a backtrace is only
    # as useful as the symbols it can be matched against.
    bundle_binary moq-relay "$RELAY"
    bundle_binary moq-cli "$MOQ"
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
    if ! (cd "$WORKSPACE" && bun install --frozen-lockfile) >"$TMP/js-install.log" 2>&1; then
        for v in js js-native-node js-native-bun; do needs "$v" && mark_broken "$v" "bun install failed"; done
        sed 's/^/        /' "$TMP/js-install.log" >&2 || true
        return
    fi
    if needs js; then
        # Nix provides Chromium via PLAYWRIGHT_BROWSERS_PATH; otherwise fetch it.
        if [[ -z "${PLAYWRIGHT_BROWSERS_PATH:-}" ]] && ! (cd "$CLIENTS/js" && bunx playwright install chromium) >"$TMP/js-chromium.log" 2>&1; then
            mark_broken js "playwright chromium install failed"
            sed 's/^/        /' "$TMP/js-chromium.log" >&2 || true
        elif ! (cd "$CLIENTS/js" && bun run check) >"$TMP/js-check.log" 2>&1; then
            mark_broken js "type check failed"
            sed 's/^/        /' "$TMP/js-check.log" >&2 || true
        elif ! (cd "$CLIENTS/js" && bunx vite build) >"$TMP/js-vite.log" 2>&1; then
            mark_broken js "vite build failed"
            sed 's/^/        /' "$TMP/js-vite.log" >&2 || true
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
    if ! (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p libmoq) >"$TMP/c-build.log" 2>&1; then
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
    C_SMOKE="$BUNDLE_SCRATCH/c-smoke"
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
    if ! (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-gst) >"$TMP/gst-build.log" 2>&1; then
        mark_broken gst "cargo build -p moq-gst failed"
        sed 's/^/        /' "$TMP/gst-build.log" >&2 || true
        return
    fi
    GST_PLUGIN_DIR="$TARGET_BASE/$PROFILE"
    # gst-inspect exits 0 even when the .so fails to load, so grep for the
    # factory. Isolate discovery to our dir + a temp registry so a system-wide moq
    # plugin can't shadow it (mirrors rs/moq-gst/smoke.sh).
    if ! GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_PLUGIN_SYSTEM_PATH_1_0="" \
        GST_REGISTRY_1_0="$BUNDLE_SCRATCH/gst-registry.bin" \
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

if curl -sf "$URL/certificate.sha256" >/dev/null 2>&1; then
    echo "error: something is already listening on 127.0.0.1:${PORT} (stale relay?)" >&2
    exit 1
fi

echo "starting relay on 127.0.0.1:${PORT}..."
# smoke.toml is the source of truth; rewrite its port into a scratch copy so a
# busy 4443 (a dev relay, a parallel run) doesn't require editing the committed file.
sed "s/4443/${PORT}/g" "$SMOKE_DIR/smoke.toml" >"$TMP/relay.toml"
relay_args=("$TMP/relay.toml")
[[ -z "${MOQ_QA_QLOG:-}" ]] || relay_args+=(--server-quic-qlog "$BUNDLE_QLOG")
"$RELAY" "${relay_args[@]}" >"$TMP/relay.log" 2>&1 &
RELAY_PID=$!
# The wire version is negotiated per session rather than pinned here, so the
# relay log is what records which one each client ended up on.
bundle_endpoint relay "$URL" "negotiated per session"
bundle_process moq-relay "$RELAY_PID"
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
        go)
            (ffmpeg_h264 | "$GO_SMOKE" publish --url "$URL" --broadcast "$broadcast") >"$log" 2>&1 &
            ;;
        js)
            # Headless Chromium encodes its own H.264 from a fake camera via
            # WebCodecs (lazily, once a subscriber creates demand).
            (cd "$CLIENTS/js" && MOQ_QA_LABEL="publish-js" bun driver.ts publish \
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
            n=$(GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_REGISTRY_1_0="$BUNDLE_SCRATCH/gst-run-registry.bin" \
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
            local label="${publisher:-none}-to-js"
            if [[ "$publisher" == "js" ]]; then
                (cd "$CLIENTS/js" && MOQ_QA_LABEL="$label" bun driver.ts subscribe \
                    --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT" --expect-audio)
            else
                (cd "$CLIENTS/js" && MOQ_QA_LABEL="$label" bun driver.ts subscribe \
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

run_round() {
    local pub="$1" broadcast="$2" pub_pid="$3"
    local pids=() names=() i sub
    for sub in "${SUB_LIST[@]}"; do
        if is_broken "$sub"; then
            echo "  FAIL  $pub -> $sub (subscriber client unavailable)"
            bundle_result "$pub -> $sub" fail "" "subscriber client unavailable"
            overall=1
            continue
        fi
        # Record how long each cell took. Every subscriber shares one budget, so the
        # spread is the diagnostic: a cell that burns the whole timeout while its
        # siblings finish in a couple of seconds is stalled, not merely slow, and one
        # creeping up on $TIMEOUT is a near-miss worth seeing before it fails.
        (
            started=$SECONDS
            # `|| status=$?` rather than a bare call: under `set -e` a failing subscriber
            # would exit the subshell before it recorded anything, and a failure is exactly
            # when the duration is worth reading.
            status=0
            run_subscriber "$sub" "$broadcast" "$pub" &
            cell=$!
            # A cell still running at STACK_AT is out of budget, and the client's
            # own timeout is about to kill it. That kill is what erases where it
            # was stuck, so read the stacks first.
            #
            # Not in the negative control, where every cell is supposed to run
            # its timeout out, and not for the browser: it spends two budgets by
            # design (startup, then the interaction checks), so a healthy cell is
            # still running here, and its evidence is the Playwright trace rather
            # than a backtrace through bun and a Chromium process tree.
            watchdog=""
            if [[ "$NEGATIVE" -eq 0 && "$sub" != js ]]; then
                (
                    sleep "$STACK_AT"
                    bundle_stack "$pub-$sub" "$cell"
                ) &
                watchdog=$!
            fi
            wait "$cell" || status=$?
            [[ -z "$watchdog" ]] || kill_tree "$watchdog"
            echo "$((SECONDS - started))" >"$TMP/$pub-$sub.secs"
            exit "$status"
        ) >"$TMP/$pub-$sub.log" 2>&1 &
        pids+=("$!")
        names+=("$sub")
    done
    # A publisher that streams forever should still be alive; if it died, the
    # subscriber failures below are a publisher bug, so surface its log.
    if [[ -n "$pub_pid" ]] && ! kill -0 "$pub_pid" 2>/dev/null; then
        echo "  WARN  publisher '$pub' exited early:"
        sed 's/^/        /' "$TMP/pub-$pub.log" 2>/dev/null || true
    fi
    local want_pass=1 got round_pass=0 elapsed
    [[ "$NEGATIVE" -eq 1 ]] && want_pass=0
    # ${arr[@]+...} guard: a round may have no live subscribers (all broken),
    # and bash 3.2 (macOS) errors on "${!pids[@]}" for an empty array under `set -u`.
    for i in ${pids[@]+"${!pids[@]}"}; do
        if wait "${pids[$i]}"; then got=1; else got=0; fi
        elapsed=$(cat "$TMP/$pub-${names[$i]}.secs" 2>/dev/null || echo "?")
        if [[ "$got" -eq "$want_pass" ]]; then
            echo "  PASS  $pub -> ${names[$i]} (${elapsed}s)"
            bundle_result "$pub -> ${names[$i]}" pass "$elapsed"
            round_pass=1
        else
            echo "  FAIL  $pub -> ${names[$i]} (${elapsed}s of ${TIMEOUT}s)"
            bundle_result "$pub -> ${names[$i]}" fail "$elapsed" "budget ${TIMEOUT}s"
            sed 's/^/        /' "$TMP/$pub-${names[$i]}.log" 2>/dev/null || true
            overall=1
        fi
    done
    # Every leg failing points at the publisher; surface its log even when the
    # process is still alive (e.g. connected and announcing but producing nothing).
    if [[ "$NEGATIVE" -eq 0 && "$round_pass" -eq 0 && ${#pids[@]} -gt 0 && -n "$pub_pid" ]]; then
        echo "  INFO  publisher '$pub' log:"
        sed 's/^/        /' "$TMP/pub-$pub.log" 2>/dev/null || true
    fi
    if [[ -n "$pub_pid" ]]; then
        # A retained session promises the client side too, and reaping here is
        # what breaks that promise: by the time cleanup runs, the publisher that
        # was streaming into the failed round is already gone. Leave it up, and
        # let teardown.sh reap it with everything else the run recorded. Only for
        # a round that actually failed, since a passing round has nothing to
        # inspect and its publisher would otherwise stream for the whole run.
        if [[ "$round_pass" -eq 0 ]] && [[ ${#pids[@]} -gt 0 ]] && bundle_retained; then
            echo "  INFO  publisher '$pub' left running for the retained session (pid $pub_pid)"
        else
            kill_tree "$pub_pid"
            wait "$pub_pid" 2>/dev/null || true
        fi
        # Don't let cleanup() later signal this now-reaped (possibly recycled) PID.
        [[ "${PUB_PID:-}" == "$pub_pid" ]] && PUB_PID=""
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

# Stack a cell just under its own budget; awk because $TIMEOUT may be fractional.
STACK_AT=$(awk -v t="$TIMEOUT" 'BEGIN { v = t - 2; if (v < 1) v = 1; printf "%.1f", v }')

# The relay is killed mid-matrix, which is how a crash reaches the clients:
# every in-flight session drops at once, with nothing in their own logs to say
# why. The relay log and its stack are the only account of it.
#
# Tracked in FAULT_PID and reaped by cleanup: a matrix that finishes inside the
# delay would otherwise leave this subshell holding a PID the run has already
# reaped, to fire at whatever the kernel hands that number to next.
FAULT_PID=""
if [[ "$FAULT" == relay ]]; then
    echo "=== fault injection: killing the relay in 3s ==="
    (
        sleep 3
        kill -KILL "$RELAY_PID" 2>/dev/null || true
    ) &
    FAULT_PID=$!
fi

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
                bundle_result "$pub -> $sub" fail "" "publisher client unavailable"
            done
            overall=1
            continue
        fi
        if [[ "$FAULT" == hang ]]; then
            echo "  (fault injection: no publisher, every subscriber will hang)"
            PUB_PID=""
        else
            start_publisher "$pub" "$broadcast"
            bundle_process "publisher-$pub" "$PUB_PID"
        fi
        run_round "$pub" "$broadcast" "$PUB_PID"
    done
fi

# The cells alone cannot tell you the relay survived. Most of them pass on the
# first byte, and a byte that arrived before the relay died still counts, so a
# crash halfway through the matrix can be reported as a clean run. It is not
# one: nothing after the crash was actually tested. Checked here rather than
# per-cell, because it is a property of the run.
if [[ -n "$RELAY_PID" ]] && ! kill -0 "$RELAY_PID" 2>/dev/null; then
    echo "  FAIL  relay (exited during the matrix)"
    bundle_result relay fail "" "exited during the matrix"
    overall=1
fi

if [[ "$overall" -eq 0 ]]; then
    echo "smoke: all checks passed"
else
    # The relay's view is often the only place that says WHY a session died
    # (auth rejection, protocol error, close codes), so surface it on failure.
    echo "smoke: FAILURES detected" >&2
    echo "--- relay log (last 150 lines) ---" >&2
    tail -n 150 "$TMP/relay.log" 2>/dev/null | sed 's/^/  relay: /' >&2 || true
fi
exit "$overall"
