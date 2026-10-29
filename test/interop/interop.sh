#!/usr/bin/env bash
# Cross-language media interop test against THIS checkout.
#
# Unlike test/smoke (which installs each client from its
# public registry to catch packaging breakage), this builds every client from
# the workspace source. It proves the code in the tree interoperates across
# implementations before anything is published: a relay built from rs/moq-relay,
# clients built from rs/moq-cli, py/, go/, cpp/moq, js/, and rs/moq-c, all
# talking to each other. There's no apt/brew/npm/PyPI here, just
# cargo/bun/uv/go/cmake/cc.
#
# It stands up a moq-relay, then for each publisher language publishes an H.264
# broadcast with an Opus tone per audio channel (the Rust CLI an AAC one too), and
# confirms every subscriber sees video and decodes that tone in every rendition,
# contiguous and in phase with its timestamps (see phase_checked), before the
# timeout (tone.py judges the decode; the browser judges its own playback). A
# publisher with its channels' tones swapped must then fail every subscriber's
# check. The browser subscriber
# also verifies rendered WebCodecs output and player pause/resume, then that a
# session the relay refuses hands Chromium the close code and reason.
#
# Every client dials with a token minted for its cell and verified by
# `moq auth serve`. Clients that print the grant the relay sent back over AUTH
# must report exactly what their token implies, and a final round per enforcing
# publisher mints a token that excludes its broadcast: the publisher must fail
# loud with Unauthorized and no subscriber may see data. AUTH is only on the
# work-in-progress moq-lite-07, so those clients dial it alone while the rest keep
# their defaults, and the matrix also crosses versions through the relay.
#
# `--versions` reruns the matrix and the tails with every client pinned to one
# version per pass, such as each even moq-transport draft (`--versions ietf`).
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi

INTEROP_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$INTEROP_DIR/../.." && pwd)
CLIENTS="$INTEROP_DIR/clients"
# The native-JS subscriber. A released-compat run swaps in its own client, which runs
# against either the checkout's or the released packages (see compat/client.ts).
NATIVE_DIR="${INTEROP_COMPAT_CLIENT:-$CLIENTS/js-native}"
NATIVE_SCRIPT=subscribe.ts
[[ -z "${INTEROP_COMPAT_CLIENT:-}" ]] || NATIVE_SCRIPT=client.ts

# Run directory, reserved ports, and process-group ownership. See test/README.md.
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$INTEROP_DIR/../lib/harness.sh"

# Captured before the parse below consumes it, so the rerun command carries every
# flag and every environment override this run was actually given.
RERUN="$(harness_env INTEROP_TIMEOUT INTEROP_FPS INTEROP_SIZE INTEROP_PORT INTEROP_PROFILE RELAY_BIN CLI_BIN INTEROP_SUB_MOQ INTEROP_VERSION INTEROP_COMPAT_CLIENT INTEROP_DECODE INTEROP_FETCH_DATA INTEROP_FETCH_TRACK)just test interop$(harness_argv "$@")"

PUBLISHERS="rust"
SUBSCRIBERS="rust"
# The idle-out check and the finite-tail lanes guard this checkout's relay and clients. A
# run that swaps in other binaries or JS clients (the wire-compat lanes run released ones)
# can't be held to them: a released client may not close cleanly, a released relay names no
# connection when one closes, and the tail clients are built from this checkout only.
IN_TREE=1
if [[ -n "${RELAY_BIN:-}${CLI_BIN:-}${INTEROP_SUB_MOQ:-}${INTEROP_COMPAT_CLIENT:-}" ]]; then
    IN_TREE=0
fi
TIMEOUT="${INTEROP_TIMEOUT:-20}"
FPS="${INTEROP_FPS:-30}"
SIZE="${INTEROP_SIZE:-320x240}"
# Empty means "any reserved port"; INTEROP_PORT pins one instead.
PORT="${INTEROP_PORT:-}"
URL=""
KEY=""         # the HMAC key every cell's token is signed with (set once the relay's auth server starts)
METRICS_URL="" # the relay's internal /metrics, served only by an in-tree run's relay
NEGATIVE=0
MEDIA=0
MEDIA_CASES=""
MEDIA_ENGINES="chromium"
TAIL_ONLY=0
FINISH_ONLY=0
# The versions the matrix runs on, one pass each. `auto` leaves every client its defaults,
# except the clients the harness reads a grant from, which offer $AUTH_VERSION alone. Any other
# entry pins every client in its pass to that version, and `ietf` expands to IETF_VERSIONS.
VERSIONS="auto"
IETF_VERSIONS="moq-transport-18,moq-transport-22"

# Cargo profile for the relay/cli/moq-c builds. Debug compiles faster, which is
# what an interop test wants; the workload (320x240@30) is trivial either way.
PROFILE="${INTEROP_PROFILE:-debug}"

# Binaries under test. Built from source below unless overridden to point at a
# prebuilt (mirrors test/smoke's RELAY_BIN/CLI_BIN escape hatch).
RELAY="${RELAY_BIN:-}"
MOQ="${CLI_BIN:-}"

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
        --finish)
            FINISH_ONLY=1
            shift
            ;;
        --tail)
            TAIL_ONLY=1
            shift
            ;;
        --versions)
            require_value "$@"
            VERSIONS="$2"
            shift 2
            ;;
        --media)
            MEDIA=1
            shift
            ;;
        --media-cases)
            require_value "$@"
            MEDIA_CASES="$2"
            shift 2
            ;;
        --media-engines)
            require_value "$@"
            MEDIA_ENGINES="$2"
            MEDIA=1
            shift 2
            ;;
        # The full matrix. --timeout 30 gives headless Chromium cold-start
        # headroom; flags after it still override.
        --all)
            PUBLISHERS="rust,python,go,cpp,js"
            SUBSCRIBERS="rust,python,go,cpp,js,js-native-node,js-native-bun,c,gst"
            TIMEOUT=30
            shift
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

if [[ -n "$MEDIA_CASES" && "$MEDIA" -ne 1 ]]; then
    echo "error: --media-cases requires --media" >&2
    exit 2
fi

# Numeric guards so a fat-fingered --timeout / INTEROP_PORT fails clearly here
# instead of surfacing later as a cryptic `timeout` or relay-bind error.
[[ "$TIMEOUT" =~ ^[0-9]+(\.[0-9]+)?$ ]] || {
    echo "error: timeout must be a positive number (got '$TIMEOUT')" >&2
    exit 2
}
[[ -z "$PORT" || "$PORT" =~ ^[0-9]+$ ]] || {
    echo "error: port must be numeric (got '$PORT')" >&2
    exit 2
}

if [[ "$FINISH_ONLY" -eq 1 ]]; then
    if [[ "$NEGATIVE" -eq 1 || "$MEDIA" -eq 1 || "$TAIL_ONLY" -eq 1 || "$IN_TREE" -eq 0 ]]; then
        echo "error: --finish requires in-tree clients and is separate from --tail, --media, and --negative" >&2
        exit 2
    fi
fi

if [[ "$TAIL_ONLY" -eq 1 ]]; then
    if [[ "$NEGATIVE" -eq 1 || "$MEDIA" -eq 1 ]]; then
        echo "error: --tail, --media, and --negative are separate runs" >&2
        exit 2
    fi
    if [[ "$IN_TREE" -eq 0 ]]; then
        echo "error: --tail builds its clients from this checkout; drop the binary and client overrides" >&2
        exit 2
    fi
    PUBLISHERS="rust,js-native-node,js-native-bun"
    SUBSCRIBERS="$PUBLISHERS"
fi

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

if [[ ! "$MEDIA_ENGINES" =~ ^(chromium|firefox|webkit)(,(chromium|firefox|webkit))*$ ]]; then
    echo "error: media engines must be a comma-separated list of chromium,firefox,webkit" >&2
    exit 2
fi
IFS=',' read -r -a MEDIA_ENGINE_LIST <<<"$MEDIA_ENGINES"

IFS=',' read -r -a PUB_LIST <<<"$PUBLISHERS"
IFS=',' read -r -a SUB_LIST <<<"$SUBSCRIBERS"

if [[ "$VERSIONS" != auto ]] && [[ "$MEDIA" -eq 1 || "$NEGATIVE" -eq 1 || -n "${INTEROP_VERSION:-}" ]]; then
    echo "error: --versions applies to the matrix and the tails, not --media, --negative, or wire-compat" >&2
    exit 2
fi
# A version the relay does not offer could only fail its pass, so refuse it here.
VERSION_LIST=()
IFS=',' read -r -a requested <<<"$VERSIONS"
for version in "${requested[@]}"; do
    if [[ "$version" == ietf ]]; then
        IFS=',' read -r -a expanded <<<"$IETF_VERSIONS"
        VERSION_LIST+=("${expanded[@]}")
    elif [[ "$version" == auto ]] || grep -qx "    \"$version\"," "$INTEROP_DIR/interop.toml"; then
        VERSION_LIST+=("$version")
    else
        echo "error: unknown version '$version': use auto, ietf, or a version interop.toml offers" >&2
        exit 2
    fi
done
# The pass running now.
VERSION=auto

needs() {
    # needs <lang>: true if <lang> appears in either list.
    local lang="$1" x
    for x in "${PUB_LIST[@]}" "${SUB_LIST[@]}"; do [[ "$x" == "$lang" ]] && return 0; done
    return 1
}

# True if any browser/native JS client is in play (they share one bun install).
needs_js() {
    needs js || needs js-native || needs js-native-node || needs js-native-bun
}

# Released-wire compatibility (compat.sh) pins one version and may run released
# binaries that predate AUTH, so it runs anonymous: it compares the wire, and the
# default matrix covers auth.
COMPAT=0
[[ -n "${INTEROP_VERSION:-}" ]] && COMPAT=1

# The clients that print the grant they received as an `auth granted` line. The
# Python, Go, and C++ clients read it through moq-ffi; moq-c's frozen ABI and
# GStreamer expose none, and the browser's shared connection keeps its session private.
# Grants are checked on $AUTH_VERSION, so only in the `auto` pass.
prints_grant() {
    [[ "$COMPAT" -eq 0 && "$VERSION" == auto ]] || return 1
    case "$1" in
        rust | python | go | cpp | js-native-node | js-native-bun) return 0 ;;
        *) return 1 ;;
    esac
}

# The publishers whose refusal the harness can read: each logs moq-net's error.
# The browser driver reports no session error.
enforces_grant() {
    [[ "$COMPAT" -eq 0 && "$VERSION" == auto ]] || return 1
    case "$1" in
        rust | python | go | cpp) return 0 ;;
        *) return 1 ;;
    esac
}

# The version every client offers alone in this pass, or nothing in the `auto` pass.
pinned_version() {
    [[ "$VERSION" == auto ]] || printf '%s' "$VERSION"
}

# The one version <lang> offers in this pass, or nothing for its defaults.
# shellcheck disable=SC2329  # reached from functions 'harness_spawn' invokes
cell_version() {
    if [[ "$VERSION" != auto ]]; then
        printf '%s' "$VERSION"
    elif [[ "$1" == rust ]] || prints_grant "$1"; then
        printf '%s' "$AUTH_VERSION"
    fi
}

# Set FLAGS to <lang>'s version flag in this pass, plus `--grant` for the Python, Go, and C++
# clients when their grant is checked.
FLAGS=()
# shellcheck disable=SC2329  # reached from functions 'harness_spawn' invokes
client_flags() {
    local version
    version=$(cell_version "$1")
    FLAGS=()
    [[ -z "$version" ]] || FLAGS+=(--version "$version")
    case "$1" in
        python | go | cpp) ! prints_grant "$1" || FLAGS+=(--grant) ;;
    esac
}

harness_begin interop "$RERUN"
# Where the matrix and tail logs go: the run directory, or one of its own per pinned pass.
LOGS="$HARNESS_RUN"

# ── results ─────────────────────────────────────────────────────────────────
# Each mode is its own suite in the CI report.
SUITE=interop
[[ "$NEGATIVE" -eq 0 ]] || SUITE=interop-negative
[[ "$MEDIA" -eq 0 ]] || SUITE=media

# The lite drafts the finite-track drop lanes cross between Rust and Node.
DROP_VERSIONS=(moq-lite-05 moq-lite-06 moq-lite-07-wip)
# The lite drafts the compressed-track lanes cross: lite-07 carries the flag, lite-06 gets inflated bytes.
COMPRESS_VERSIONS=(moq-lite-07-wip moq-lite-06)
# The compressed catalog's concurrent readers: lite-07 carries the flag, and lite-06 and a
# moq-transport reader that does not offer FLATE share the relay's one inflated copy.
CATALOG_VERSIONS=(moq-lite-07-wip moq-lite-06 moq-transport-22)

# Every publisher in this checkout sends a tone per audio channel, and every subscriber decodes
# AUDIO_MS of it for tone.py (the browser: its page) to check against the publisher's list. This
# is the one place the tones are chosen. A stereo publisher sends a different tone per channel, so a
# channel swap fails too. A run that swaps in released CLIs or JS clients (the wire-compat lanes)
# checks audio only in its decoding media lanes, and the bindings' clients, which always check it,
# refuse to run in the rest.
AUDIO=1
[[ -z "${CLI_BIN:-}${INTEROP_SUB_MOQ:-}${INTEROP_COMPAT_CLIENT:-}" || "${INTEROP_DECODE:-0}" == 1 ]] || AUDIO=0
if [[ "$AUDIO" -eq 0 ]]; then
    for sub in "${SUB_LIST[@]}"; do
        case "$sub" in
            rust) ;;
            js-native-node | js-native-bun) [[ -n "${INTEROP_COMPAT_CLIENT:-}" ]] || sub_needs_audio="$sub" ;;
            *) sub_needs_audio="$sub" ;;
        esac
        if [[ -n "${sub_needs_audio:-}" ]]; then
            echo "error: the $sub subscriber always checks audio, which a run with CLI_BIN, INTEROP_SUB_MOQ, or INTEROP_COMPAT_CLIENT turns off without INTEROP_DECODE" >&2
            exit 2
        fi
    done
fi
AUDIO_MS=600
STEREO_TONE="440,660"
publisher_tone() {
    case "$1" in
        python | cpp) echo 440 ;;
        *) echo "$STEREO_TONE" ;;
    esac
}
# The wrong-tone control's publisher sends the stereo tone with its channels swapped.
SWAPPED_TONE="660,440"
# The audio renditions <lang> sends the tone in, by codec: the catalog codec up to its first `.`.
# Every subscriber decodes each one, the native ones to <prefix>.<codec>.{txt,pcm}. The wire-compat
# lanes send Opus alone: the released @moq/hang's libav fallback has no AAC decoder.
publisher_codecs() {
    case "$1" in
        rust) if [[ -n "${INTEROP_COMPAT_CLIENT:-}" ]]; then echo opus; else echo "opus,mp4a"; fi ;;
        *) echo opus ;;
    esac
}
# phase_checked <round> <subscriber> <codec>: whether tone.py also holds the tone's phase to the
# decode's timestamps, which fails a misapplied pre-skip. Every publisher's tone starts at
# timestamp zero except the browser's, which stamps its file from its own page clock. The native-JS
# subscriber hands WebCodecs raw Opus without the OpusHead, as the player does, which keeps the
# pre-skip for the player to trim, and its libav fallback stamps milliseconds. GStreamer's AAC is
# skipped for its decoder: `faad`, which decodebin picks, drops the first frame's output and stamps
# the rest a frame early, so it reads the priming 1024 samples early whatever the timestamps say.
# shellcheck disable=SC2329  # reached from run_cell, which 'harness_spawn' invokes
phase_checked() {
    [[ "$1" != js* && "$2" != js-native-* && ("$2" != gst || "$3" == opus) ]]
}

# Record a case for the CI report: `record <outcome> case=C [field=value...]` (see
# harness_result). Every session in a case dials the same relay with each client's own
# transport choice, and the version of its pass, unless compat.sh pins one for the whole run.
record() {
    harness_result "$1" suite="$SUITE" transport=auto version="${INTEROP_VERSION:-$VERSION}" "${@:2}"
}

# Call <action> <name> <media.ts args...> for each media run on <engine>. The planning
# pass and the real one share it, so they cannot disagree on which runs exist.
media_runs() {
    local action="$1" engine="$2"
    if [[ -n "$MEDIA_CASES" ]]; then
        "$action" "$engine: media cases: $MEDIA_CASES" --player-engine "$engine" --cases "$MEDIA_CASES" --focused
        return
    fi
    "$action" "$engine: media output + lifecycle" --player-engine "$engine"
    "$action" "$engine: control: frozen video" --player-engine "$engine" --fault frozen-video --cases none --expect-fail "video progress"
    "$action" "$engine: control: silent audio" --player-engine "$engine" --fault silent-audio --cases none --expect-fail "audio tone"
    "$action" "$engine: control: offset audio" --player-engine "$engine" --fault audio-offset --cases none --expect-fail "audio/video sync"
    "$action" "$engine: control: leaked session" --player-engine "$engine" --leak --cases detach --expect-fail "resource baseline"
    "$action" "$engine: control: lagging latecomer" --player-engine "$engine" --lag --cases late-join --expect-fail "late join reaches live"
}

# shellcheck disable=SC2329  # invoked indirectly via media_runs
plan_media() {
    record "not run" case="$1" publisher=js subscriber=js
}

# Record every case this run will reach as `not run`, before any build, so one the run
# never reaches still shows in the report. Mirrors the dispatch at the end of this file.
plan_cases() {
    local sub engine
    if [[ "$MEDIA" -eq 1 ]]; then
        for engine in "${MEDIA_ENGINE_LIST[@]}"; do media_runs plan_media "$engine"; done
        return
    fi
    if [[ "$NEGATIVE" -eq 1 ]]; then
        for sub in "${SUB_LIST[@]}"; do record "not run" case="no publisher" publisher=none subscriber="$sub"; done
        return
    fi
    for VERSION in "${VERSION_LIST[@]}"; do plan_pass; done
    VERSION=auto
}

# One version's share of plan_cases.
plan_pass() {
    local pub sub runtime version
    if [[ "$TAIL_ONLY" -eq 0 && "$FINISH_ONLY" -eq 0 ]]; then
        for pub in "${PUB_LIST[@]}"; do
            for sub in "${SUB_LIST[@]}"; do record "not run" case=media publisher="$pub" subscriber="$sub"; done
            [[ "$IN_TREE" -eq 0 ]] || record "not run" case="relay close" publisher="$pub"
            [[ "$VERSION" == auto ]] || record "not run" case="negotiated version" publisher="$pub"
            ! prints_grant "$pub" || record "not run" case=grant publisher="$pub"
        done
        for pub in "${PUB_LIST[@]}"; do
            enforces_grant "$pub" || continue
            for sub in "${SUB_LIST[@]}"; do record "not run" case=unauthorized publisher="$pub" subscriber="$sub"; done
            record "not run" case="unauthorized publisher" publisher="$pub"
        done
        if [[ "$AUDIO" -eq 1 && "$VERSION" == auto ]]; then
            for sub in "${SUB_LIST[@]}"; do record "not run" case="wrong tone" publisher=rust subscriber="$sub"; done
        fi
        [[ "$VERSION" != auto ]] || ! needs js || record "not run" case="refused session" subscriber=js
    fi
    [[ "$IN_TREE" -eq 1 ]] || return 0
    if [[ "$TAIL_ONLY" -eq 0 ]]; then
        for pub in "${PUB_LIST[@]}"; do
            for sub in "${SUB_LIST[@]}"; do record "not run" case=finish publisher="$pub" subscriber="$sub"; done
        done
    fi
    [[ "$FINISH_ONLY" -eq 0 ]] || return 0
    record "not run" case="CLI FETCH" publisher=rust subscriber=rust
    record "not run" case="late join" publisher=rust subscriber=rust
    record "not run" case=tail publisher=rust subscriber=rust
    for runtime in js-native-node js-native-bun; do
        needs "$runtime" || continue
        record "not run" case="CLI FETCH" publisher="$runtime" subscriber=rust
        record "not run" case="late join" publisher=rust subscriber="$runtime"
        record "not run" case="late join" publisher="$runtime" subscriber=rust
        record "not run" case=tail publisher=rust subscriber="$runtime"
        record "not run" case=tail publisher="$runtime" subscriber=rust
        if [[ "$VERSION" == moq-transport-* ]]; then
            record "not run" case=datagram publisher=rust subscriber="$runtime"
            record "not run" case=datagram publisher="$runtime" subscriber=rust
            record "not run" case="wire track status" publisher=rust subscriber="$runtime"
            record "not run" case=goaway publisher=rust subscriber="$runtime"
            [[ "$runtime" != js-native-bun ]] || record "not run" case=goaway publisher="$runtime" subscriber=rust
            record "not run" case="track status" publisher="$runtime" subscriber=rust
        fi
    done
    [[ "$VERSION" == auto ]] || return 0
    for runtime in js-native-node js-native-bun; do
        needs "$runtime" || continue
        for version in "${COMPRESS_VERSIONS[@]}"; do
            record "not run" case=compress publisher=rust subscriber="$runtime" version="$version"
            record "not run" case=compress publisher="$runtime" subscriber=rust version="$version"
        done
        record "not run" case="compressed catalog" publisher=rust subscriber="$runtime"
    done
    needs js-native-node || return 0
    for version in "${DROP_VERSIONS[@]}"; do
        record "not run" case=drop publisher=rust subscriber=js-native-node version="$version"
        record "not run" case=drop publisher=js-native-node subscriber=rust version="$version"
    done
}
plan_cases

TARGET_BASE=""    # cargo target dir (resolved in require_tools)
PY=""             # python interpreter with the workspace moq build (set in prepare)
C_INTEROP=""      # compiled C client binary (set in prepare)
GO_INTEROP=""     # compiled Go client binary (set in prepare)
CPP_INTEROP=""    # compiled C++ client binary (set in prepare)
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
    # Tail-only runs publish raw tracks, so they never encode with ffmpeg.
    local missing=() t tools=(cargo curl timeout)
    [[ "$TAIL_ONLY" -eq 1 ]] || tools+=(ffmpeg)
    for t in "${tools[@]}"; do
        have "$t" || missing+=("$t")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required tools: ${missing[*]}" >&2
        exit 1
    fi
    # Resolve the cargo target dir once (honors a custom CARGO_TARGET_DIR, which
    # the self-hosted CI runner sets), so the built binaries and moq-c's header
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
    if [[ -n "$RELAY" && -n "$MOQ" ]]; then
        [[ -x "$RELAY" && -x "$MOQ" ]] || {
            echo "override binaries are not executable" >&2
            exit 1
        }
        return
    fi
    local flag=()
    [[ "$PROFILE" == "release" ]] && flag=(--release)
    echo "building moq-relay + moq-cli ($PROFILE)..."
    # ${arr[@]+...} guard: bash 3.2 (macOS /bin/bash) errors on "${flag[@]}" for
    # an empty (debug) array under `set -u`.
    (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay -p moq-cli) || {
        echo "error: failed to build moq-relay / moq-cli" >&2
        exit 1
    }
    if [[ "$MEDIA" -eq 0 ]]; then
        (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-cli --example interop-tail --example interop-catalog --example interop-audio --example interop-control --example interop-goaway --example interop-goaway-client --example interop-finish)
    fi
    [[ -n "$RELAY" ]] || RELAY="$TARGET_BASE/$PROFILE/moq-relay"
    # The `moq-cli` crate ships its binary as `moq` (a `[[bin]]` override).
    [[ -n "$MOQ" ]] || MOQ="$TARGET_BASE/$PROFILE/moq"
}

# Build the workspace Python packages as wheels (maturin builds rs/moq-ffi, hatchling
# the moq-rs wrapper) and install them into a venv in the run directory, so `import moq`
# resolves to this checkout rather than a PyPI wheel. The shared .venv is off limits: a
# concurrent run's `just py build` uninstalls the package there while this run's
# clients are importing it.
prepare_python() {
    have uv || {
        mark_broken python "uv not found"
        return
    }
    echo "building python client (workspace moq wheels)..."
    local venv="$HARNESS_RUN/py-venv" wheels="$HARNESS_RUN/py-wheels"
    # maturin stages the bindings at a fixed path under the cargo target dir, so one
    # build at a time per target. Debug like the other clients; its build backend
    # defaults to release.
    if (cd "$WORKSPACE" &&
        harness_locked "$TARGET_BASE/.moq-test-maturin.lock" \
            env MATURIN_PEP517_ARGS="--profile dev --locked" \
            uv build --wheel --package moq-ffi --out-dir "$wheels" &&
        uv build --wheel --package moq-rs --out-dir "$wheels" &&
        uv venv "$venv" &&
        uv pip install --python "$venv/bin/python" --no-deps "$wheels"/*.whl) >"$HARNESS_RUN/py-build.log" 2>&1; then
        PY="$venv/bin/python"
    else
        mark_broken python "wheel build failed"
        sed 's/^/        /' "$HARNESS_RUN/py-build.log" >&2 || true
    fi
}

# Link the JS workspace (the interop clients are bun workspace members, so the
# @moq/* packages resolve to this checkout's source) and build the browser page.
prepare_js() {
    have bun || {
        for v in js js-native js-native-node js-native-bun; do needs "$v" && mark_broken "$v" "bun not found"; done
        return
    }
    # A staged released-compat client brings its own packages.
    if [[ -n "${INTEROP_COMPAT_CLIENT:-}" ]]; then
        [[ -f "$INTEROP_COMPAT_CLIENT/client.ts" ]] || {
            echo "missing staged compat client" >&2
            exit 1
        }
        return
    fi
    echo "installing js clients (workspace @moq/* via bun)..."
    if ! (cd "$WORKSPACE" && bun install --frozen-lockfile) >"$HARNESS_RUN/js-install.log" 2>&1; then
        for v in js js-native js-native-node js-native-bun; do needs "$v" && mark_broken "$v" "bun install failed"; done
        sed 's/^/        /' "$HARNESS_RUN/js-install.log" >&2 || true
        return
    fi
    if needs js; then
        if ! (cd "$CLIENTS/js" && bun run check) >"$HARNESS_RUN/js-check.log" 2>&1; then
            mark_broken js "type check failed"
            sed 's/^/        /' "$HARNESS_RUN/js-check.log" >&2 || true
        # Into the run directory, where harness.ts serves it from: vite empties its output
        # first, so a shared dist/ vanishes under a concurrent run's page loads.
        elif ! (cd "$CLIENTS/js" && bunx vite build --outDir "$HARNESS_RUN/js-dist" --emptyOutDir) >"$HARNESS_RUN/js-vite.log" 2>&1; then
            mark_broken js "vite build failed"
            sed 's/^/        /' "$HARNESS_RUN/js-vite.log" >&2 || true
        fi
    fi
    if (needs js-native-node || needs js-native-bun) && ! (cd "$CLIENTS/js-native" && bun run check) >"$HARNESS_RUN/js-native-check.log" 2>&1; then
        for v in js-native-node js-native-bun; do needs "$v" && mark_broken "$v" "type check failed"; done
        sed 's/^/        /' "$HARNESS_RUN/js-native-check.log" >&2 || true
    fi
    if needs js-native-node && ! have node; then
        mark_broken js-native-node "node not found"
    fi
}

# Stage the Go modules from this checkout (sh/go/stage.sh builds moq-ffi for
# the host, regenerates the bindings, and wires the wrapper to them by replace),
# then build the interop client against that exact tree. The client is copied to a
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
    local staged ffi_pkg wrapper_pkg src="$HARNESS_RUN/go-client"
    if ! staged=$(bash "$WORKSPACE/sh/go/stage.sh" --output "$HARNESS_RUN/go-stage" 2>"$HARNESS_RUN/go-stage.log"); then
        mark_broken go "sh/go/stage.sh failed"
        sed 's/^/        /' "$HARNESS_RUN/go-stage.log" >&2 || true
        return
    fi
    ffi_pkg=$(printf '%s\n' "$staged" | sed -n 1p)
    wrapper_pkg=$(printf '%s\n' "$staged" | sed -n 2p)
    mkdir -p "$src"
    cp "$CLIENTS/go/go.mod" "$CLIENTS/go/main.go" "$src/"
    GO_INTEROP="$HARNESS_RUN/go-interop"
    if ! (
        cd "$src"
        export CGO_ENABLED=1 GOFLAGS=-mod=mod
        go mod edit \
            -replace="moq.dev/moq=$wrapper_pkg" \
            -replace="moq.dev/moq-ffi=$ffi_pkg"
        # Scratch copy is outside the git tree; stamping VCS info fails in a worktree.
        go build -buildvcs=false -o "$GO_INTEROP" .
    ) >"$HARNESS_RUN/go-build.log" 2>&1; then
        mark_broken go "go build failed"
        sed 's/^/        /' "$HARNESS_RUN/go-build.log" >&2 || true
    fi
}

# Build and install cpp/moq (cargo builds moq-ffi, uniffi-bindgen-cpp renders the
# bindings), then build the C++ client against the installed package with
# find_package(moq-cpp), the way an external project consumes it. Debug, like the
# rest of the run.
prepare_cpp() {
    local t
    for t in cmake uniffi-bindgen-cpp; do
        have "$t" || {
            mark_broken cpp "$t not found (see cpp/moq/README.md)"
            return
        }
    done
    echo "building c++ client (workspace cpp/moq via uniffi-bindgen-cpp + cmake)..."
    local build="$HARNESS_RUN/cpp-build" prefix="$HARNESS_RUN/cpp-prefix" config=Debug
    [[ "$PROFILE" == "release" ]] && config=Release
    if ! {
        cmake -S "$WORKSPACE/cpp/moq" -B "$build/package" -DCMAKE_BUILD_TYPE="$config" -DCMAKE_INSTALL_LIBDIR=lib &&
            cmake --build "$build/package" &&
            cmake --install "$build/package" --prefix "$prefix" &&
            cmake -S "$CLIENTS/cpp" -B "$build/client" -DCMAKE_BUILD_TYPE="$config" -DCMAKE_PREFIX_PATH="$prefix" &&
            cmake --build "$build/client"
    } >"$HARNESS_RUN/cpp-build.log" 2>&1; then
        mark_broken cpp "cmake build failed"
        sed 's/^/        /' "$HARNESS_RUN/cpp-build.log" >&2 || true
        return
    fi
    CPP_INTEROP="$build/client/cpp-interop"
}

# Build moq-c (the C staticlib + cbindgen header) and compile the C subscriber
# against it. cargo writes libmoq.a to the profile dir, and build.rs writes
# moq.h into its OUT_DIR, which only cargo's JSON messages name.
prepare_c() {
    local cc="${CC:-cc}" out_dir header lib os_libs
    have "$cc" || {
        mark_broken c "no C compiler ($cc) on PATH"
        return
    }
    echo "building c client (workspace moq-c + cc)..."
    local flag=()
    [[ "$PROFILE" == "release" ]] && flag=(--release)
    if ! (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-c --message-format=json-render-diagnostics) >"$HARNESS_RUN/c-build.json" 2>"$HARNESS_RUN/c-build.log"; then
        mark_broken c "cargo build -p moq-c failed"
        sed 's/^/        /' "$HARNESS_RUN/c-build.log" >&2 || true
        return
    fi
    out_dir=$(grep '"reason":"build-script-executed"' "$HARNESS_RUN/c-build.json" | grep -F '/moq-c#' |
        sed -n 's/.*"out_dir":"\([^"]*\)".*/\1/p' | tail -1) || true
    header="$out_dir/include/moq.h"
    lib="$TARGET_BASE/$PROFILE/libmoq.a"
    [[ -f "$header" && -f "$lib" ]] || {
        mark_broken c "moq-c artifacts missing ($header / $lib)"
        return
    }
    # cargo can't inject libmoq.a's native deps into an external link, so read
    # them from the same list moq-c.pc and CMake use.
    local native_libs
    case "$(uname -s)" in
        Darwin) native_libs="$WORKSPACE/rs/moq-c/native-libs/apple.txt" ;;
        *) native_libs="$WORKSPACE/rs/moq-c/native-libs/linux.txt" ;;
    esac
    os_libs=()
    while read -r entry; do
        case "$entry" in
            '' | '#'*) continue ;;
            framework:*) os_libs+=(-framework "${entry#framework:}") ;;
            *) os_libs+=("-l$entry") ;;
        esac
    done <"$native_libs"
    C_INTEROP="$HARNESS_RUN/c-interop"
    if ! "$cc" "$CLIENTS/c/subscribe.c" -I"$out_dir/include" -L"$TARGET_BASE/$PROFILE" -lmoq "${os_libs[@]}" -o "$C_INTEROP" >"$HARNESS_RUN/c-compile.log" 2>&1; then
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
    # Use the packaging smoke check's isolated discovery and factory assertions.
    if ! "$WORKSPACE/rs/moq-gst/smoke.sh" "$GST_PLUGIN_DIR" >"$HARNESS_RUN/gst-inspect.log" 2>&1; then
        mark_broken gst "moqsrc not exposed (plugin failed to load against this GStreamer)"
        sed 's/^/        /' "$HARNESS_RUN/gst-inspect.log" >&2 || true
        return
    fi
    # The audio check decodes through these, from the system's plugins.
    local factory
    for factory in decodebin opusdec audioconvert identity filesink; do
        GST_REGISTRY_1_0="$HARNESS_RUN/gst-run-registry.bin" gst-inspect-1.0 "$factory" >/dev/null 2>&1 || {
            mark_broken gst "GStreamer has no $factory"
            return
        }
    done
    for factory in fdkaacdec avdec_aac faad ""; do
        [[ -n "$factory" ]] || {
            mark_broken gst "GStreamer has no AAC decoder (fdkaacdec, avdec_aac, or faad)"
            return
        }
        GST_REGISTRY_1_0="$HARNESS_RUN/gst-run-registry.bin" gst-inspect-1.0 "$factory" >/dev/null 2>&1 && break
    done
}

# ── setup ───────────────────────────────────────────────────────────────────
require_tools
build_relay_cli

echo "relay:   $RELAY"
echo "moq-cli: $MOQ"

needs python && prepare_python
needs go && prepare_go
needs cpp && prepare_cpp
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
if harness_probe "$URL/certificate.sha256"; then
    echo "error: something is already listening on 127.0.0.1:${PORT} (stale relay?)" >&2
    exit 1
fi

# The relay's auth server: `moq auth serve` verifying every token this run mints
# against one fresh key. It answers only POST, so readiness is any HTTP reply.
if [[ "$COMPAT" -eq 0 ]]; then
    harness_port auth
    AUTH_URL="http://127.0.0.1:${HARNESS_PORT}/"
    auth_up() { curl -s -o /dev/null --max-time 1 "$AUTH_URL"; }
    if auth_up; then
        echo "error: something is already listening on $AUTH_URL" >&2
        exit 1
    fi
    KEY="$HARNESS_RUN/key.jwk"
    "$MOQ" auth generate --out "$KEY"
    harness_spawn auth "$HARNESS_RUN/auth.log" "$MOQ" auth serve --listen "127.0.0.1:${HARNESS_PORT}" --key "$KEY"
    AUTH_PID="$HARNESS_PID"
    deadline=$((SECONDS + 30))
    until auth_up; do
        if ((SECONDS >= deadline)) || harness_exited "$AUTH_PID"; then
            echo "auth server never became ready" >&2
            sed 's/^/  auth: /' "$HARNESS_RUN/auth.log" >&2 || true
            exit 1
        fi
        sleep 0.05
    done
fi

echo "starting relay on 127.0.0.1:${PORT}..."
# interop.toml is the source of truth; rewrite its ports into a scratch copy so the
# committed file never has to be edited for a run.
if [[ "$COMPAT" -eq 1 ]]; then
    # Anonymous, offering only the pinned version, which comes from the executable's
    # advertised CLI choices.
    sed -e "s|:4443\"|:${PORT}\"|g" -e '/^version = \[/,/^\]/d' -e 's|^url = "http://127.0.0.1:4440/"|public = "**"|' \
        "$INTEROP_DIR/interop.toml" >"$HARNESS_RUN/relay.toml"
    sed -i "/\[listen\]/a version = [\"${INTEROP_VERSION}\"]" "$HARNESS_RUN/relay.toml"
    [[ "$(grep -c '^version = ' "$HARNESS_RUN/relay.toml")" == 1 && "$(grep -c '^public = ' "$HARNESS_RUN/relay.toml")" == 1 ]] || {
        echo "relay.toml needs exactly one pinned [listen] version and anonymous access" >&2
        exit 1
    }
else
    sed -e "s|:4443\"|:${PORT}\"|g" -e "s|http://127.0.0.1:4440/|${AUTH_URL}|" \
        "$INTEROP_DIR/interop.toml" >"$HARNESS_RUN/relay.toml"
    # The compressed-catalog lane reads the relay's inflate counter from /metrics, which counts
    # only while stats are enabled.
    harness_port metrics
    METRICS_URL="http://127.0.0.1:${HARNESS_PORT}/metrics"
    printf '\n[internal]\nlisten = "127.0.0.1:%s"\n\n[stats]\nenabled = true\n' "$HARNESS_PORT" >>"$HARNESS_RUN/relay.toml"
fi
# Connection spans and clean closes must survive an inherited warn-only filter.
harness_spawn relay "$HARNESS_RUN/relay.log" env \
    "RUST_LOG=${RUST_LOG:+$RUST_LOG,}moq_relay::connection=info,moq_relay::relay=info" \
    "$RELAY" "$HARNESS_RUN/relay.toml"
if ! harness_ready "$URL/certificate.sha256" 30 "$HARNESS_PID"; then
    echo "relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi
harness_endpoint relay "$URL"

# shellcheck source-path=SCRIPTDIR source=relay-log.sh
source "$INTEROP_DIR/relay-log.sh"

# ── tokens ──────────────────────────────────────────────────────────────────
# Print the relay URL carrying a fresh token; the arguments are `moq auth sign`'s
# (`--publish P`, `--subscribe S`). Compat runs anonymous, so it prints the bare URL.
token_url() {
    if [[ "$COMPAT" -eq 1 ]]; then
        printf '%s' "$URL"
        return
    fi
    local token
    token=$("$MOQ" auth sign --key "$KEY" "$@")
    printf '%s/?jwt=%s' "$URL" "$token"
}

# Set `connect` to moq-cli's arguments for <url>, moving a `?jwt=` token to
# `--connect-token` so it rides SETUP where the version carries one. The binding clients keep it
# in the URL, which covers the query form.
# shellcheck disable=SC2329  # invoked from the indirectly invoked publisher and subscriber
rust_connect() {
    case "$1" in
        *'?jwt='*) connect=(--connect "${1%%\?jwt=*}" --connect-token "${1#*\?jwt=}") ;;
        *) connect=(--connect "$1") ;;
    esac
}

# The pattern a cell's token grants its publisher, or its subscribers, on <broadcast>. The `auto`
# pass mints patterns no prefix could carry, so AUTH must deliver them as minted. moq-transport's
# AUTH extension carries prefixes alone and refuses a narrower grant, closing the
# session, so a pinned pass grants the broadcast's subtree.
publish_grant() {
    if [[ "$VERSION" == auto ]]; then printf '%s' "$1"; else printf '%s/**' "$1"; fi
}
subscribe_grant() {
    if [[ "$VERSION" == auto ]]; then printf '**/%s' "$1"; else printf '%s/**' "$1"; fi
}

# moq-cli logs each grant it receives over AUTH at debug, under `moq_net::auth`.
CLI_LOG="${RUST_LOG:-info},moq_net::auth=debug"

# The lite version with AUTH, which no client offers by default. The clients the
# harness reads a grant or a refusal from dial it. Compat dials its pinned version.
AUTH_VERSION="${INTEROP_VERSION:-moq-lite-07-wip}"

# The `auth granted` line a token minted with at most one publish and one
# subscribe pattern implies. Every client that prints one dials $AUTH_VERSION,
# which carries AUTH, so a client that prints nothing never got its grant.
grant_line() {
    local publish="${1:+\"$1\"}" subscribe="${2:+\"$2\"}"
    printf 'auth granted publish=[%s] subscribe=[%s]' "$publish" "$subscribe"
}

# The last grant a client printed to <log>, with ANSI colour dropped and list
# separators normalized so the Rust and JS renderings compare equal.
reported_grant() {
    sed 's/\x1b\[[0-9;]*m//g' "$1" 2>/dev/null |
        grep -o 'auth granted publish=\[[^]]*\] subscribe=\[[^]]*\]' |
        tail -n 1 | sed 's/", "/","/g' || true
}

# Check that <lang>'s log reports <expected>; prints why not and fails otherwise.
check_grant() {
    local lang="$1" log="$2" expected="$3" got
    prints_grant "$lang" || return 0
    got=$(reported_grant "$log")
    [[ "$got" == "$expected" ]] && return 0
    echo "grant: got '${got:-nothing}', token implies '$expected'"
    return 1
}

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

# An ffmpeg audio source playing <tone>: one 0.5-amplitude sine per channel, at 48 kHz.
# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
tone_source() {
    local hz tones exprs=() layout=mono
    IFS=',' read -r -a tones <<<"$1"
    for hz in "${tones[@]}"; do exprs+=("0.5*sin(2*PI*$hz*t)"); done
    [[ "${#exprs[@]}" -eq 1 ]] || layout=stereo
    local IFS='|'
    printf 'aevalsrc=%s:s=48000:c=%s' "${exprs[*]}" "$layout"
}

# ffmpeg_fmp4 <tone> <codecs>: the Rust CLI's input, the same picture plus <tone> in each of
# <codecs> (see publisher_codecs), all in fragmented MP4, so its cells also cover moq-mux's fMP4
# Opus and AAC import, and every subscriber's AAC decode.
# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
ffmpeg_fmp4() {
    local codec i=0 audio=()
    for codec in ${2//,/ }; do
        case "$codec" in
            opus) audio+=(-map 1:a "-c:a:$i" libopus "-b:a:$i" 128k) ;;
            mp4a) audio+=(-map 1:a "-c:a:$i" aac "-b:a:$i" 128k) ;;
            *)
                echo "error: ffmpeg_fmp4 cannot encode $codec" >&2
                return 1
                ;;
        esac
        i=$((i + 1))
    done
    ffmpeg -hide_banner -loglevel error -re -f lavfi -i "testsrc=size=${SIZE}:rate=${FPS}" \
        -re -f lavfi -i "$(tone_source "$1")" -map 0:v "${audio[@]}" \
        -c:v libx264 -profile:v baseline -preset ultrafast -pix_fmt yuv420p \
        -x264-params "keyint=${FPS}:min-keyint=${FPS}:scenecut=0" \
        -f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer -frag_duration 1000 -
}

# The browser's input: the picture and <tone> as a file it decodes and loops, H.264 beside 16-bit
# PCM so decoding it loses nothing. Not Chromium's fake devices: their timers skip a late 10 ms
# tick, and Web Audio then splices the tone without a timestamp gap whenever the runner is short
# of CPU. Ten seconds is a whole number of cycles of any whole-Hz tone and of video frames, so the
# loop is seamless.
# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
tone_media() {
    local media
    media="$HARNESS_RUN/tone-${1//,/-}.mov"
    [[ -f "$media" ]] || ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc=size=${SIZE}:rate=${FPS}:duration=10" \
        -f lavfi -i "$(tone_source "$1"):d=10" -c:v libx264 -profile:v baseline -preset ultrafast -pix_fmt yuv420p \
        -x264-params "keyint=${FPS}:min-keyint=${FPS}:scenecut=0" -c:a pcm_s16le "$media"
    printf '%s' "$media"
}

# The publisher pipeline, run in a process group of its own by `harness_spawn`
# so reaping it takes ffmpeg with it. Every non-browser publisher consumes the
# same ffmpeg Annex-B stream on stdin; the client frames it (moq-cli / the FFI
# importers only frame-and-forward).
# shellcheck disable=SC2329  # invoked indirectly via 'harness_spawn'
run_publisher() {
    local lang="$1" broadcast="$2" url="$3" tone="$4"
    client_flags "$lang"
    case "$lang" in
        rust)
            # A finished import stays up for its linger (a released CLI: its max-age);
            # `stop_publisher` allows 10 s to close. The wire-compat FETCH lanes keep the
            # video-only Annex-B input.
            local connect
            rust_connect "$url"
            if [[ "$AUDIO" -eq 1 ]]; then
                # The max-age caps every reader's max delay, so it covers the longest, the native
                # JS audio readers' 5 s. Each 1 s fragment lands as one burst, so with a 1 s
                # max-age the oldest group a joining reader is served goes stale as soon as the
                # next group lands, cutting it mid-read and failing the gap check.
                ffmpeg_fmp4 "$tone" "$(publisher_codecs rust)" | RUST_LOG="$CLI_LOG" "$MOQ" "${connect[@]}" --connect-version "$(cell_version rust)" \
                    --broadcast "$broadcast" import --max-age 5s fmp4
            else
                ffmpeg_h264 | RUST_LOG="$CLI_LOG" "$MOQ" "${connect[@]}" --connect-version "$(cell_version rust)" \
                    --broadcast "$broadcast" import --max-age 1s avc3
            fi
            ;;
        python)
            ffmpeg_h264 | "$PY" "$CLIENTS/python/interop.py" \
                publish --url "$url" --broadcast "$broadcast" --tone "$tone" ${FLAGS[@]+"${FLAGS[@]}"}
            ;;
        go)
            ffmpeg_h264 | "$GO_INTEROP" publish --url "$url" --broadcast "$broadcast" --tone "$tone" ${FLAGS[@]+"${FLAGS[@]}"}
            ;;
        cpp)
            ffmpeg_h264 | "$CPP_INTEROP" publish --url "$url" --broadcast "$broadcast" --tone "$tone" ${FLAGS[@]+"${FLAGS[@]}"}
            ;;
        js-native)
            # exec, so the client leads the group and `stop_publisher` waits for its own close.
            exec bun "$INTEROP_COMPAT_CLIENT/client.ts" publish "$url" "$broadcast"
            ;;
        js)
            # Headless Chromium decodes the tone file and encodes its own H.264 and Opus
            # via WebCodecs (lazily, once a subscriber creates demand). exec, so the driver
            # leads the group and `stop_publisher` waits for its own close.
            local media
            media=$(tone_media "$tone")
            cd "$CLIENTS/js" && exec bun driver.ts publish \
                --url "$url" --broadcast "$broadcast" --media "$media" ${FLAGS[@]+"${FLAGS[@]}"}
            ;;
        *)
            echo "unknown publisher: $lang" >&2
            return 1
            ;;
    esac
}

# start_publisher <round> <lang> <broadcast> <url> [tone], logging to pub-<round>.log. The tone
# defaults to the one subscribers expect from <lang>. Sets global PUB_PID to the publisher's
# process group leader.
PUB_PID=""
start_publisher() {
    local round="$1" lang="$2" broadcast="$3" url="$4" tone="${5:-$(publisher_tone "$2")}"
    harness_spawn "pub-$round" "$LOGS/pub-$round.log" run_publisher "$lang" "$broadcast" "$url" "$tone"
    PUB_PID="$HARNESS_PID"
}

# Stop a publisher the way a real one ends, so it closes its session and the idle-out check
# below counts only real stalls. A stdin-fed client finishes once ffmpeg exits and its input
# ends; the browser driver and the native JS client close on SIGTERM. Whatever is still
# running after the wait is killed, and its connection then fails the round as an idle-out.
stop_publisher() {
    local pid="$1" lang="$2" deadline=$((SECONDS + 10))
    case "$lang" in
        # The leader alone: a SIGTERM to the group would reach Chromium too, which then exits
        # without closing its sessions.
        js | js-native) kill -TERM "$pid" 2>/dev/null || true ;;
        *) pkill -TERM -g "$pid" -x ffmpeg 2>/dev/null || true ;;
    esac
    while ! harness_exited "$pid" && ((SECONDS < deadline)); do
        sleep 0.1
    done
    if ! harness_exited "$pid"; then
        echo "  WARN  publisher '$lang' was still running 10 s after it was stopped; killing it"
    fi
    harness_reap "$pid"
}

# The relay log line the next connection starts on.
relay_next_line() {
    echo $(($(wc -l <"$HARNESS_RUN/relay.log") + 1))
}

# Run a native-JS subscriber and judge it by the "received N bytes" marker it
# prints, not its exit code. The @moq/web-transport NAPI addon can segfault
# during the runtime's exit teardown *after* a frame has arrived (an upstream bug
# under bun), which would turn a real success into a signal exit. The data path
# is what we test, so a printed marker is the verdict; the crash is swallowed.
# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
run_native() {
    local out
    out=$( (cd "$NATIVE_DIR" && "$@") 2>&1) || true
    printf '%s\n' "$out" >&2
    grep -q '^received ' <<<"$out"
}

# Write a track name and the group the compat client's live subscription saw on it to
# $HARNESS_RUN/<broadcast>.track. Extra args go to its subscribe (e.g. --track).
# shellcheck disable=SC2329  # reached from run_subscriber, which 'harness_spawn' invokes
observe_group() {
    local broadcast="$1"
    shift
    (cd "$INTEROP_COMPAT_CLIENT" && node --import tsx client.ts subscribe --url "$URL" --broadcast "$broadcast" \
        --timeout "$TIMEOUT" --track-file "$HARNESS_RUN/$broadcast.track" "$@")
}

# The GStreamer subscriber with audio: moqsrc's video_0 must deliver a byte, and each of the
# <codecs> audio pads, audio_0 on, decodes through decodebin to <out>.<pad>.pcm while the log tracer
# records each pad's caps and each decoded buffer's timestamp and size where an identity passes it
# on. The tracer logs from the streaming thread before the data moves on, so whatever reached a file
# is in the trace; -v's notifications trail the data through the bus, can miss the interrupt, and drop
# and repeat buffers once two pads stream at once. Once enough of all has landed, an interrupt makes
# gst-launch stop the pipeline and close its session. Not with -e: moqsrc does not end its pads on
# an EOS sent into the pipeline. Each pad's log, trimmed to the buffers its file holds, becomes
# <out>.<codec>.txt beside <out>.<codec>.pcm, the layout tone.py reads, the codec named by the
# pad's caps.
# shellcheck disable=SC2329  # reached from run_subscriber, which 'harness_spawn' invokes
run_gst_audio() {
    local url="$1" broadcast="$2" out="$3" tone="$4" codecs="$5" tones renditions pid want deadline version i short
    local pads=()
    version=$(cell_version gst)
    IFS=',' read -r -a tones <<<"$tone"
    IFS=',' read -r -a renditions <<<"$codecs"
    # At 48 kHz, f32: a stop condition only. tone.py judges whatever rate and layout decoded.
    want=$((AUDIO_MS * 48 * 4 * ${#tones[@]}))
    # SECONDS counts whole seconds, so a fractional timeout rounds up rather than down to nothing.
    deadline=$((SECONDS + ${TIMEOUT%.*}))
    [[ "$TIMEOUT" != *.* ]] || deadline=$((deadline + 1))
    for i in "${!renditions[@]}"; do
        pads+=("s.audio_$i" ! decodebin "name=decode$i" ! audioconvert ! "audio/x-raw,format=F32LE,layout=interleaved"
            ! identity "name=frames$i" ! filesink "location=$out.$i.pcm" buffer-mode=2)
    done
    GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_REGISTRY_1_0="$HARNESS_RUN/gst-run-registry.bin" \
        GST_TRACERS=log GST_DEBUG=GST_BUFFER:7,GST_EVENT:7 GST_DEBUG_NO_COLOR=1 GST_DEBUG_FILE="$out.trace" \
        gst-launch-1.0 moqsrc name=s url="$url" broadcast="$broadcast" ${version:+connect-version="$version"} \
        s.video_0 ! filesink location="$out.video" buffer-mode=2 "${pads[@]}" >"$out.gst" 2>&1 &
    pid=$!
    while :; do
        short=0
        [[ -s "$out.video" ]] || short=1
        for i in "${!renditions[@]}"; do
            [[ "$(wc -c 2>/dev/null <"$out.$i.pcm" || echo 0)" -ge "$want" ]] || short=1
        done
        [[ "$short" -eq 1 ]] || break
        if ! kill -0 "$pid" 2>/dev/null || ((SECONDS >= deadline)); then
            kill -KILL "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            echo "gst: no video or not enough audio within ${TIMEOUT}s"
            tail -n 20 "$out.gst"
            return 1
        fi
        sleep 0.1
    done
    kill -INT "$pid"
    # Shutdown shares the cell's budget, with the same 3 s grace `timeout -k 3` gives the others.
    while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline + 3)); do sleep 0.1; done
    kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" || {
        echo "gst-launch did not stop cleanly on an interrupt"
        tail -n 20 "$out.gst"
        return 1
    }
    python3 - "$out" "${#renditions[@]}" <<'PYCODE'
import os, re, sys
out, pads = sys.argv[1], int(sys.argv[2])
trace = open(f"{out}.trace").read()

def caps(pad):
    found = re.search(rf'do_push_event_pre:<{pad}> .*?caps=\(caps\)"((?:[^"\\]|\\.)*)"', trace)
    assert found, f"no caps event on {pad} in the gst trace"
    return re.sub(r"\\(.)", r"\1", found[1])

for pad in range(pads):
    src = caps(f"s:audio_{pad}")
    if src.startswith("audio/x-opus,"):
        codec = "opus"
    elif src.startswith("audio/mpeg,") and "mpegversion=(int)4" in src:
        codec = "mp4a"
    else:
        raise SystemExit(f"audio_{pad} carries {src}, which this check does not name")
    decoded = caps(f"frames{pad}:src")
    rate = int(re.search(r"rate=\(int\)(\d+)", decoded)[1])
    channels = int(re.search(r"channels=\(int\)(\d+)", decoded)[1])
    # The pipeline stopped mid-stream, so the file may lack the last buffers the tracer logged.
    held, kept, lines = os.path.getsize(f"{out}.{pad}.pcm"), 0, [f"{rate} {channels}"]
    pushes = rf"do_push_buffer_pre:<frames{pad}:src> .*? pts (\d+):(\d+):([\d.]+), dts [^,]*, dur [^,]*, size (\d+)"
    for h, m, s, size in re.findall(pushes, trace):
        if kept + int(size) > held:
            break
        kept += int(size)
        pts = round((int(h) * 3600 + int(m) * 60 + float(s)) * 1_000_000)
        lines.append(f"{pts} {int(size) // (4 * channels)}")
    os.truncate(f"{out}.{pad}.pcm", kept)
    # Exclusive, so two pads of one codec fail rather than overwrite each other.
    with open(f"{out}.{codec}.txt", "x") as log:
        log.write("\n".join(lines) + "\n")
    os.rename(f"{out}.{pad}.pcm", f"{out}.{codec}.pcm")
PYCODE
}

# run_subscriber <lang> <broadcast> <url> [audio prefix] [tone] [codecs]: with $AUDIO, every
# subscriber but the browser also decodes $AUDIO_MS of each audio rendition to
# <audio prefix>.<codec>.{txt,pcm} for tone.py, and the browser checks <tone> itself in each of
# <codecs>.
# shellcheck disable=SC2329  # reached from a function 'harness_spawn' invokes
run_subscriber() {
    local lang="$1" broadcast="$2" url="$3" audio_out="${4:-}" tone="${5:-}" codecs="${6:-}" audio=()
    client_flags "$lang"
    if [[ "$AUDIO" -eq 1 ]]; then audio=(--audio-out "$audio_out" --audio-ms "$AUDIO_MS"); fi
    case "$lang" in
        rust)
            # moq-cli only handles SIGINT, so -k forces SIGKILL if it ignores the
            # SIGTERM that fires when no data arrives within the timeout.
            if [[ "${INTEROP_FETCH_TRACK:-0}" == 1 ]]; then
                # Discover the track from the decoded catalog, never today's naming
                # convention, and a group live demand is filling.
                observe_group "$broadcast" || return
                local track group binary index=0
                { read -r track && read -r group; } <"$HARNESS_RUN/$broadcast.track" || return
                # Both readers FETCH that group by its observed ID, which waits for it to finish.
                for binary in "$MOQ" "${INTEROP_SUB_MOQ:-$MOQ}"; do
                    timeout -k 3 "$TIMEOUT" "$binary" --connect "$URL" ${INTEROP_VERSION:+--connect-version "$INTEROP_VERSION"} \
                        --broadcast "$broadcast" fetch "$track" --group "$group" --json >"$HARNESS_RUN/$broadcast.$index.json" || return
                    index=$((index + 1))
                done
                python3 - "$HARNESS_RUN/$broadcast.0.json" "$HARNESS_RUN/$broadcast.1.json" <<'PYCODE'
import base64, json, sys
outputs=[]
sequence=None
for path in sys.argv[1:]:
    frames=[json.loads(line) for line in open(path)]
    assert frames, "FETCH returned no frames"
    payloads=[]
    for index, frame in enumerate(frames):
        payload=base64.b64decode(frame["payload"], validate=True)
        if sequence is None: sequence=frame["group"]
        assert frame["group"] == sequence and frame["frame"] == index and frame["size"] == len(payload), frame
        payloads.append(payload)
    outputs.append(payloads)
assert outputs[0] == outputs[1], "current/released FETCH changed the immutable group's payloads"
PYCODE
                return
            fi
            if [[ "${INTEROP_FETCH_DATA:-0}" == 1 ]]; then
                # The JS fixture writes one group on `data` once a subscriber wants it. A
                # live subscriber observes it; the reader then FETCHes it by that ID.
                observe_group "$broadcast" --track data || return
                local track group
                { read -r track && read -r group; } <"$HARNESS_RUN/$broadcast.track" || return
                timeout -k 3 "$TIMEOUT" "${INTEROP_SUB_MOQ:-$MOQ}" --connect "$URL" ${INTEROP_VERSION:+--connect-version "$INTEROP_VERSION"} \
                    --broadcast "$broadcast" fetch "$track" --group "$group" >"$HARNESS_RUN/$broadcast.fetch" || return
                [[ "$(cat "$HARNESS_RUN/$broadcast.fetch")" == "compat-fetch" ]]
                return
            fi
            if [[ "${INTEROP_DECODE:-0}" == 1 ]]; then
                # ffmpeg actually decodes the exported media; mux headers alone cannot pass. The
                # first $AUDIO_MS of audio from its first decoded sample (atrim, unlike -t, ignores
                # where video starts) lands in a WAV, and the encoder's input stats log each decoded
                # frame's timestamp and length for tone.py. The outputs are the verdict: a CLI may
                # exit on EPIPE with a plain error once ffmpeg has what it needs. The WAV cannot say
                # which codec it came from, so this takes a publisher of one audio rendition.
                if [[ "$codecs" == *,* ]]; then
                    echo "error: the ffmpeg decode reads one audio rendition, not $codecs" >&2
                    return 1
                fi
                (timeout -k 3 "$TIMEOUT" "${INTEROP_SUB_MOQ:-$MOQ}" --connect "$URL" \
                    ${INTEROP_VERSION:+--connect-version "$INTEROP_VERSION"} --broadcast "$broadcast" export fmp4 || [[ "$?" == 141 ]]) |
                    ffmpeg -hide_banner -loglevel error -i - \
                        -map 0:v:0 -frames:v 1 -f rawvideo -pix_fmt gray "$HARNESS_RUN/$broadcast.raw" \
                        -map 0:a:0 -af "atrim=duration=${AUDIO_MS}ms" -c:a pcm_f32le \
                        -stats_enc_pre "$audio_out.stats" -stats_enc_pre_fmt '{tb} {pts} {samp}' -f wav "$audio_out.wav"
                [[ -s "$HARNESS_RUN/$broadcast.raw" ]] || return
                python3 - "$audio_out" "$codecs" <<'PYCODE'
import struct, sys
out, codec = sys.argv[1:]
wav = open(f"{out}.wav", "rb").read()
assert wav[:4] == b"RIFF" and wav[8:12] == b"WAVE", "ffmpeg wrote no WAV"
pos, rate, pcm = 12, None, None
while pos + 8 <= len(wav):
    chunk, size = wav[pos : pos + 4], struct.unpack_from("<I", wav, pos + 4)[0]
    if chunk == b"fmt ":
        tag, channels, rate = struct.unpack_from("<HHI", wav, pos + 8)
        bits = struct.unpack_from("<H", wav, pos + 22)[0]
        assert (tag, bits) == (3, 32), f"WAV is not f32 (format {tag}, {bits} bits)"
    elif chunk == b"data":
        pcm = wav[pos + 8 : pos + 8 + size]
    pos += 8 + size + (size & 1)
assert rate and pcm is not None, "WAV has no fmt or data chunk"
lines = [f"{rate} {channels}"]
for line in open(f"{out}.stats"):
    tb, pts, samples = line.split()
    num, den = (int(v) for v in tb.split("/"))
    lines.append(f"{round(int(pts) * num * 1_000_000 / den)} {samples}")
open(f"{out}.{codec}.pcm", "xb").write(pcm)
open(f"{out}.{codec}.txt", "x").write("\n".join(lines) + "\n")
PYCODE
                return
            fi
            # The CLI has no PCM output, so moq-audio decodes the audio in a client of its own,
            # alongside the CLI so the cell stays within one timeout.
            local n connect audio_pid="" audio_status=0
            rust_connect "$url"
            if [[ "$AUDIO" -eq 1 ]]; then
                timeout -k 3 "$TIMEOUT" "$TARGET_BASE/$PROFILE/examples/interop-audio" \
                    "$url" "$broadcast" "$audio_out" "$AUDIO_MS" "$(cell_version rust)" &
                audio_pid=$!
            fi
            n=$(RUST_LOG="$CLI_LOG" timeout -k 3 "$TIMEOUT" "${INTEROP_SUB_MOQ:-$MOQ}" "${connect[@]}" \
                --connect-version "$(cell_version rust)" --broadcast "$broadcast" export fmp4 | head -c 1 | wc -c | tr -d ' ' || true)
            [[ -z "$audio_pid" ]] || wait "$audio_pid" || audio_status=$?
            [[ "${n:-0}" -ge 1 && "$audio_status" -eq 0 ]]
            ;;
        python)
            "$PY" "$CLIENTS/python/interop.py" \
                subscribe --url "$url" --broadcast "$broadcast" --timeout "$TIMEOUT" ${FLAGS[@]+"${FLAGS[@]}"} ${audio[@]+"${audio[@]}"}
            ;;
        go)
            "$GO_INTEROP" subscribe --url "$url" --broadcast "$broadcast" --timeout "$TIMEOUT" ${FLAGS[@]+"${FLAGS[@]}"} ${audio[@]+"${audio[@]}"}
            ;;
        cpp)
            "$CPP_INTEROP" subscribe --url "$url" --broadcast "$broadcast" --timeout "$TIMEOUT" ${FLAGS[@]+"${FLAGS[@]}"} ${audio[@]+"${audio[@]}"}
            ;;
        c)
            "$C_INTEROP" subscribe --url "$url" --broadcast "$broadcast" --timeout "$TIMEOUT" ${FLAGS[@]+"${FLAGS[@]}"} ${audio[@]+"${audio[@]}"}
            ;;
        gst)
            # moqsrc exposes each rendition as a Sometimes pad (video_%u / audio_%u),
            # named so the first of each kind is always video_0 / audio_0. Link
            # video_0 by name: a bare `moqsrc ! filesink` would take whichever pad
            # appears first, so a publisher with audio (every one but the Rust CLI)
            # could pass this cell on audio bytes without video ever flowing. We grab one byte, the
            # same "bytes moved" bar as the rust subscriber (no decode). SIGPIPE is ignored, so
            # head closing the pipe fails filesink's next write and gst-launch stops the pipeline,
            # closing its session, rather than dying with it open; no data just runs out the
            # timeout. Our plugin dir rides on top of the system path
            # (which provides filesink); a private registry keeps the scan off the
            # user's cache. buffer-mode=2 makes filesink unbuffered so the first frame
            # reaches head immediately.
            if [[ "$AUDIO" -eq 1 ]]; then
                run_gst_audio "$url" "$broadcast" "$audio_out" "$tone" "$codecs"
                return
            fi
            local n version
            version=$(cell_version gst)
            n=$(
                trap '' PIPE
                GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_REGISTRY_1_0="$HARNESS_RUN/gst-run-registry.bin" \
                    timeout -k 3 "$TIMEOUT" gst-launch-1.0 -q \
                    moqsrc name=s url="$url" broadcast="$broadcast" ${version:+connect-version="$version"} \
                    s.video_0 ! filesink location=/dev/stdout buffer-mode=2 \
                    2>/dev/null | head -c 1 | wc -c | tr -d ' ' || true
            )
            [[ "${n:-0}" -ge 1 ]]
            ;;
        js)
            # Headless Chromium decodes and renders via WebCodecs, checks the tone on each
            # channel the player outputs, then drives the real player's pause/resume controls,
            # once per audio rendition with the player pinned to it.
            (cd "$CLIENTS/js" && bun driver.ts subscribe \
                --url "$url" --broadcast "$broadcast" --timeout "$TIMEOUT" --tone "$tone" --codecs "$codecs" ${FLAGS[@]+"${FLAGS[@]}"})
            ;;
        js-native-bun)
            # Native @moq/net via moq's WebTransport polyfill, under bun.
            run_native bun "$NATIVE_SCRIPT" subscribe \
                --url "$url" --broadcast "$broadcast" --timeout "$TIMEOUT" ${FLAGS[@]+"${FLAGS[@]}"} ${audio[@]+"${audio[@]}"}
            ;;
        js-native-node)
            # Same, under node (tsx runs the TS directly).
            run_native node --import tsx "$NATIVE_SCRIPT" subscribe \
                --url "$url" --broadcast "$broadcast" --timeout "$TIMEOUT" ${FLAGS[@]+"${FLAGS[@]}"} ${audio[@]+"${audio[@]}"}
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
    local pub="$1" sub="$2" broadcast="$3" url="$4" tone="$5" codecs="$6" started=$SECONDS status=0
    local decoded="$LOGS/$pub-$sub-audio" codec
    # `|| status=$?` rather than a bare call: under `set -e` a failing subscriber
    # would exit before it recorded anything, and a failure is exactly when the
    # duration is worth reading.
    run_subscriber "$sub" "$broadcast" "$url" "$decoded" "$tone" "$codecs" || status=$?
    # The browser judges the tone in its page; every other subscriber leaves a decode per
    # rendition here, and a missing one fails like a garbled one. Each is judged, so the
    # wrong-tone control can require every rendition to fail.
    if [[ "$status" -eq 0 && "$AUDIO" -eq 1 && "$sub" != js ]]; then
        for codec in ${codecs//,/ }; do
            local phase=()
            if phase_checked "$pub" "$sub" "$codec"; then phase=(--phase); fi
            echo "the $codec rendition:"
            python3 "$INTEROP_DIR/tone.py" --tone "$tone" ${phase[@]+"${phase[@]}"} "$decoded.$codec" || status=$?
        done
    fi
    echo "$((SECONDS - started))" >"$LOGS/$pub-$sub.secs"
    return "$status"
}

# run_round <round> <broadcast> <pub_pid> <expect> <from>: every subscriber dials with a
# token for <broadcast> alone (see subscribe_grant), and must see media with the tone its
# publisher sends (expect=media), time out (nothing), or fail its audio check (wrong-tone).
# <round> names the publisher in the output and the logs, and <from> is the relay log
# line the round's connections start on.
run_round() {
    local pub="$1" broadcast="$2" pub_pid="$3" expect="$4" from="$5"
    local pids=() names=() i sub sub_url sub_grant why tone codecs renditions
    # A round is named after its publisher's language, plus `-denied` or `-swapped` for a control.
    local publisher="${pub%-denied}"
    publisher="${publisher%-swapped}"
    # The report's case: media must flow, the token or the missing publisher must keep it from
    # flowing, or every subscriber must refuse the swapped tone as the wrong tone.
    local case=media
    if [[ "$expect" == nothing ]]; then
        case=unauthorized
        [[ "$pub" != none ]] || case="no publisher"
    elif [[ "$expect" == wrong-tone ]]; then
        case="wrong tone"
    fi
    tone=$(publisher_tone "$publisher")
    codecs=$(publisher_codecs "$publisher")
    IFS=',' read -r -a renditions <<<"$codecs"
    sub_url=$(token_url --subscribe "$(subscribe_grant "$broadcast")")
    sub_grant=$(grant_line "" "**/$broadcast")
    for sub in "${SUB_LIST[@]}"; do
        if is_broken "$sub"; then
            echo "  FAIL  $pub -> $sub (subscriber client unavailable)"
            record fail case="$case" publisher="$publisher" subscriber="$sub" detail="subscriber client unavailable"
            overall=1
            continue
        fi
        harness_spawn "$pub-$sub" "$LOGS/$pub-$sub.log" run_cell "$pub" "$sub" "$broadcast" "$sub_url" "$tone" "$codecs"
        pids+=("$HARNESS_PID")
        names+=("$sub")
    done
    # A publisher that streams forever should still be alive; if it died, the
    # subscriber failures below are a publisher bug, so surface its log.
    if [[ "$expect" != nothing && -n "$pub_pid" ]] && ! kill -0 "$pub_pid" 2>/dev/null; then
        echo "  WARN  publisher '$pub' exited early:"
        sed 's/^/        /' "$LOGS/pub-$pub.log" 2>/dev/null || true
    fi
    local got round_pass=0 elapsed
    # ${arr[@]+...} guard: a round may have no live subscribers (all broken),
    # and bash 3.2 (macOS) errors on "${!pids[@]}" for an empty array under `set -u`.
    for i in ${pids[@]+"${!pids[@]}"}; do
        why=""
        local log="$LOGS/$pub-${names[$i]}.log" outcome=nothing
        if harness_wait "${pids[$i]}"; then
            outcome=media
        elif [[ "$(grep -c '^audio check failed: wrong tone' "$log" || true)" -eq "${#renditions[@]}" ]]; then
            # Media flowed and every rendition decoded, but none into the publisher's tone.
            outcome=wrong-tone
        fi
        elapsed=$(cat "$LOGS/$pub-${names[$i]}.secs" 2>/dev/null || echo "?")
        local cell=(case="$case" publisher="$publisher" subscriber="${names[$i]}" log="$log")
        [[ "$elapsed" == "?" ]] || cell+=(seconds="$elapsed")
        if [[ "$outcome" == "$expect" ]] && why=$(check_grant "${names[$i]}" "$log" "$sub_grant"); then
            echo "  PASS  $pub -> ${names[$i]} (${elapsed}s)"
            record pass "${cell[@]}"
            round_pass=1
        else
            echo "  FAIL  $pub -> ${names[$i]} (${elapsed}s of ${TIMEOUT}s)${why:+ $why}"
            sed 's/^/        /' "$log" 2>/dev/null || true
            if [[ -n "$why" ]]; then
                :
            elif [[ "$expect" == media ]] && grep -q '^audio check failed' "$log"; then
                why=$(grep -m 1 '^audio check failed' "$log")
            elif [[ "$expect" == media ]]; then
                why="no data within ${TIMEOUT}s"
            elif [[ "$expect" == wrong-tone ]]; then
                why="did not fail its audio check (outcome: $outcome)"
            else
                why="saw data it should not have"
            fi
            record fail "${cell[@]}" detail="$why"
            overall=1
        fi
    done
    # Every leg failing points at the publisher; surface its log even when the
    # process is still alive (e.g. connected and announcing but producing nothing).
    if [[ "$expect" != nothing && "$round_pass" -eq 0 && ${#pids[@]} -gt 0 && -n "$pub_pid" ]]; then
        echo "  INFO  publisher '$pub' log:"
        sed 's/^/        /' "$LOGS/pub-$pub.log" 2>/dev/null || true
    fi
    # `stop_publisher` reaps it, retiring the entry, so teardown never signals this
    # now-reaped (possibly recycled) PID again.
    if [[ -n "$pub_pid" ]]; then
        stop_publisher "$pub_pid" "$publisher"
    fi
    # A round that expects no data ends its subscribers by timing out, which is its point.
    [[ "$expect" == media && "$IN_TREE" -eq 1 ]] || return 0
    if check_idle_outs "$pub" "$from"; then
        record pass case="relay close" publisher="$publisher" log="$HARNESS_RUN/relay.log"
    else
        record fail case="relay close" publisher="$publisher" log="$HARNESS_RUN/relay.log" detail="relay connections did not all close cleanly"
    fi
    # The relay logs which version each connection negotiated, but not which client dialed it, so a
    # client that ignored its pin fails the round rather than its own cell.
    [[ "$VERSION" != auto ]] || return 0
    if why=$(check_versions "$pub" "$from" "$VERSION"); then
        record pass case="negotiated version" publisher="$publisher" log="$HARNESS_RUN/relay.log"
    else
        echo "$why"
        overall=1
        record fail case="negotiated version" publisher="$publisher" log="$HARNESS_RUN/relay.log" detail="a relay connection negotiated another version"
    fi
}

# Finish readers connect before publication, but make no media request until finish is fixed.
# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_finish_client() {
    local lang="$1" role="$2" broadcast="$3" url="$4" ack="${5:-}" version limit read_limit
    version=$(cell_version "$lang")
    # Setup and the read phase each have a deadline. The latter includes the real media linger.
    read_limit=$(awk -v t="$TIMEOUT" 'BEGIN { print t * 2 }')
    limit=$(awk -v t="$TIMEOUT" 'BEGIN { print t * 4 }')
    local args=("$role" --url "$url" --broadcast "$broadcast" --timeout "$read_limit")
    [[ -z "$version" ]] || args+=(--version "$version")
    if [[ "$role" == publish ]]; then args+=(--finish-ack "$ack"); else args+=(--finish --frames 10); fi
    case "$lang/$role" in python/publish | go/publish | cpp/publish) args+=(--tone "$(publisher_tone "$lang")") ;; esac
    case "$lang" in
        rust) timeout -k 3 "$limit" "$TARGET_BASE/$PROFILE/examples/interop-finish" "${args[@]}" ;;
        python) timeout -k 3 "$limit" "$PY" "$CLIENTS/python/interop.py" "${args[@]}" ;;
        go) timeout -k 3 "$limit" "$GO_INTEROP" "${args[@]}" ;;
        cpp) timeout -k 3 "$limit" "$CPP_INTEROP" "${args[@]}" ;;
        c) timeout -k 3 "$limit" "$C_INTEROP" "${args[@]}" ;;
        js) (cd "$CLIENTS/js" && timeout -k 3 "$limit" bun driver.ts "${args[@]}") ;;
        js-native-node) (cd "$CLIENTS/js-native" && timeout -k 3 "$limit" node --import tsx subscribe.ts "${args[@]}") ;;
        js-native-bun) (cd "$CLIENTS/js-native" && timeout -k 3 "$limit" bun subscribe.ts "${args[@]}") ;;
        *)
            echo "finish assertions unavailable for $lang" >&2
            return 1
            ;;
    esac
}

# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_finish_publisher() {
    local lang="$1" broadcast="$2" url="$3" ack="$4"
    if [[ "$lang" == js ]]; then
        run_finish_client "$lang" publish "$broadcast" "$url" "$ack"
    else
        ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc2=size=$SIZE:rate=$FPS" \
            -frames:v 10 -c:v libx264 -preset ultrafast -tune zerolatency -pix_fmt yuv420p \
            -g 10 -keyint_min 10 -sc_threshold 0 -f h264 - | run_finish_client "$lang" publish "$broadcast" "$url" "$ack"
    fi
}

# moqsrc's watch is persistent: a finished broadcast keeps its pads, since an announcement means
# online/offline, so GStreamer has no end, withdrawal, or session to report. It joins once the
# broadcast has finished and checks the late tail alone: ten nonempty video buffers, and no more
# by the time an interrupt stops the pipeline.
# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_finish_gst() {
    local broadcast="$1" url="$2" out="$3" version pid deadline count
    version=$(cell_version gst)
    # The read phase's deadline, as the other readers get; SECONDS counts whole seconds.
    deadline=$((SECONDS + ${TIMEOUT%.*} * 2))
    [[ "$TIMEOUT" != *.* ]] || deadline=$((deadline + 2))
    GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_REGISTRY_1_0="$HARNESS_RUN/gst-run-registry.bin" \
        gst-launch-1.0 -v moqsrc name=s url="$url" broadcast="$broadcast" ${version:+connect-version="$version"} \
        s.video_0 ! identity name=frames silent=false ! fakesink >"$out" 2>&1 &
    pid=$!
    count=0
    until ((count >= 10)); do
        if ! kill -0 "$pid" 2>/dev/null || ((SECONDS >= deadline)); then
            kill -KILL "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            echo "gst: $count of 10 finished frames before the deadline"
            tail -n 20 "$out"
            return 1
        fi
        sleep 0.1
        count=$(grep -cE 'GstIdentity:frames: last-message = chain.*\([1-9][0-9]* bytes' "$out" || true)
    done
    kill -INT "$pid"
    while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline + 3)); do sleep 0.1; done
    kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" || {
        echo "gst-launch did not stop cleanly on an interrupt"
        tail -n 20 "$out"
        return 1
    }
    count=$(grep -c 'GstIdentity:frames: last-message = chain' "$out" || true)
    [[ "$count" -eq 10 ]] || {
        echo "gst: $count buffers, want exactly 10"
        return 1
    }
    echo "finish tail frames=10 end=n/a"
}

finish_ready() {
    local pid="$1" log="$2" marker="$3" deadline
    deadline=$(awk -v now="$SECONDS" -v t="$TIMEOUT" 'BEGIN { print int(now + t + 1) }')
    while ((SECONDS < deadline)); do
        grep -q "$marker" "$log" && return 0
        harness_exited "$pid" && return 1
        sleep 0.05
    done
    return 1
}

# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_finish_reader() {
    local fifo="$1"
    shift
    run_finish_client "$@" <"$fifo"
}

# The line a finish reader prints once every check it can make has passed.
finish_marker() {
    if [[ "$1" == gst ]]; then
        echo 'finish tail frames=10 end=n/a'
    else
        echo 'finish clean frames=10 catalog=eof announce=end health=ok'
    fi
}

run_finishes() {
    local pub sub name broadcast grant pid ack ack_fd reader_fd i started late
    # moq-transport does not yet deliver a finished broadcast's late tail, withdrawal, and
    # same-session health across clients, so a pinned moq-transport pass records the case as
    # skipped rather than red.
    if [[ "$VERSION" == moq-transport-* ]]; then
        for pub in "${PUB_LIST[@]}"; do
            for sub in "${SUB_LIST[@]}"; do
                record skip case=finish publisher="$pub" subscriber="$sub" detail="moq-transport finish: quest/m2/ietf-finish.md"
            done
        done
        return
    fi
    for pub in "${PUB_LIST[@]}"; do
        name="finish-$pub"
        # The fixture and its .health sibling share a parent, so one prefix grant covers both:
        # moq-transport's AUTH extension carries prefixes alone.
        broadcast="$name-$$-$RANDOM/fixture.hang"
        grant="${broadcast%/*}/**"
        ack="$LOGS/$name-ack"
        local pids=() fds=() names=() all_ready=1 all_clean=1 publisher_ok=1
        late=0
        started=$SECONDS
        echo "=== finished media: $pub ==="
        if is_broken "$pub"; then
            for sub in "${SUB_LIST[@]}"; do record fail case=finish publisher="$pub" subscriber="$sub" detail="publisher unavailable"; done
            overall=1
            continue
        fi
        mkfifo "$ack"
        exec {ack_fd}<>"$ack"
        for sub in "${SUB_LIST[@]}"; do
            if is_broken "$sub"; then
                record fail case=finish publisher="$pub" subscriber="$sub" detail="finish assertions unavailable for $sub"
                echo "  FAIL  finish $pub -> $sub (finish assertions unavailable)"
                overall=1
                continue
            fi
            # GStreamer has no stdin barrier; it joins after the finish instead.
            if [[ "$sub" == gst ]]; then
                late=1
                continue
            fi
            local fifo="$LOGS/$name-$sub-read"
            mkfifo "$fifo"
            exec {reader_fd}<>"$fifo"
            harness_spawn "$name-$sub" "$LOGS/$name-$sub.log" run_finish_reader "$fifo" "$sub" subscribe "$broadcast" "$(token_url --subscribe "$grant")"
            pids+=("$HARNESS_PID")
            fds+=("$reader_fd")
            names+=("$sub")
        done
        for i in "${!pids[@]}"; do
            if ! finish_ready "${pids[$i]}" "$LOGS/$name-${names[$i]}.log" '^finish reader ready$'; then all_ready=0; fi
        done
        if [[ "$all_ready" -eq 1 ]]; then
            harness_spawn "$name-pub" "$LOGS/$name-pub.log" run_finish_publisher "$pub" "$broadcast" "$(token_url --publish "$grant")" "$ack"
            pid="$HARNESS_PID"
            if ! finish_ready "$pid" "$LOGS/$name-pub.log" '^finish ready linger_us='; then all_ready=0; fi
        else pid=""; fi
        if [[ "$all_ready" -eq 1 ]]; then
            for reader_fd in "${fds[@]}"; do printf 'read\n' >&"$reader_fd"; done
            if [[ "$late" -eq 1 ]]; then
                harness_spawn "$name-gst" "$LOGS/$name-gst.log" run_finish_gst "$broadcast" "$(token_url --subscribe "$grant")" "$LOGS/$name-gst.out"
                pids+=("$HARNESS_PID")
                fds+=("")
                names+=(gst)
            fi
            for i in "${!pids[@]}"; do
                harness_wait "${pids[$i]}" || all_clean=0
                if ! grep -qx "$(finish_marker "${names[$i]}")" "$LOGS/$name-${names[$i]}.log"; then all_clean=0; fi
            done
        else all_clean=0; fi
        if [[ "$all_clean" -eq 1 ]]; then
            printf 'complete\n' >&"$ack_fd"
            exec {ack_fd}>&-
            harness_wait "$pid" || publisher_ok=0
            grep -qx 'finish acknowledged' "$LOGS/$name-pub.log" || publisher_ok=0
        else
            publisher_ok=0
            [[ -z "$pid" ]] || harness_reap "$pid"
            exec {ack_fd}>&-
        fi
        for i in "${!pids[@]}"; do
            sub="${names[$i]}"
            local cell=(case=finish publisher="$pub" subscriber="$sub" seconds=$((SECONDS - started)) log="$LOGS/$name-$sub.log")
            if [[ "$all_ready" -eq 1 && "$publisher_ok" -eq 1 ]] && grep -qx "$(finish_marker "$sub")" "$LOGS/$name-$sub.log"; then
                if [[ "$sub" == gst ]]; then
                    echo "  PASS  finish $pub -> $sub (late tail; end N/A, moqsrc keeps watching)"
                    record pass "${cell[@]}" detail="late tail only; end N/A: moqsrc keeps watching a finished broadcast"
                else
                    echo "  PASS  finish $pub -> $sub (late tail, EOF, END, live session)"
                    record pass "${cell[@]}"
                fi
            else
                record fail "${cell[@]}" detail="missing finish tail/EOF/withdrawal/health or acknowledgement"
                echo "  FAIL  finish $pub -> $sub"
                overall=1
                cat "$LOGS/$name-$sub.log" "$LOGS/$name-pub.log" 2>/dev/null || true
                harness_reap "${pids[$i]}"
            fi
            [[ "$sub" != gst ]] || continue
            reader_fd="${fds[$i]}"
            exec {reader_fd}>&-
            rm "$LOGS/$name-$sub-read"
        done
        rm "$ack"
    done
}

# Raw-track clients have no codecs: every byte, group, and the declared end is checked.
# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_tail_client() {
    local lang="$1" role="$2" broadcast="$3" url="$4" version="${5:-}" mode="${6:-}" limit="$TIMEOUT"
    # The publisher outlives the subscriber's whole deadline, waiting for its acknowledgement.
    [[ "$role" == publish ]] && limit=$(awk -v t="$TIMEOUT" 'BEGIN { print t * 2 }')
    # A mode follows the version, which is empty for the defaults.
    local extra=()
    if [[ -n "$mode" ]]; then
        extra=("$version" "$mode")
    elif [[ -n "$version" ]]; then
        extra=("$version")
    fi
    case "$lang" in
        rust) timeout -k 3 "$limit" "$TARGET_BASE/$PROFILE/examples/interop-tail" "$role" "$url" "$broadcast" ${extra[@]+"${extra[@]}"} ;;
        js-native-node) (cd "$CLIENTS/js-native" && timeout -k 3 "$limit" node --import tsx tail.ts "$role" "$url" "$broadcast" ${extra[@]+"${extra[@]}"}) ;;
        js-native-bun) (cd "$CLIENTS/js-native" && timeout -k 3 "$limit" bun tail.ts "$role" "$url" "$broadcast" ${extra[@]+"${extra[@]}"}) ;;
        *)
            echo "unknown tail client: $lang" >&2
            return 1
            ;;
    esac
}

run_drop_pair() {
    local pub="$1" sub="$2" version="$3" pid subscriber_pid started=$SECONDS
    local name="drop-${version}-${pub}-${sub}"
    local fifo="$LOGS/tail-ack" broadcast="$name-$$-$RANDOM"
    local cell=(case=drop publisher="$pub" subscriber="$sub" version="$version" log="$LOGS/$name-sub.log")
    if is_broken "$pub" || is_broken "$sub"; then
        echo "  FAIL  drop $version $pub -> $sub (client unavailable)"
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    mkfifo "$fifo"
    exec 9<>"$fifo"
    harness_spawn "$name-pub" "$LOGS/$name-pub.log" run_tail_publisher "$fifo" "$pub" publish "$broadcast" "$(token_url --publish "$(publish_grant "$broadcast")")" "$version" drop
    pid="$HARNESS_PID"
    harness_spawn "$name-sub" "$LOGS/$name-sub.log" run_tail_client "$sub" subscribe "$broadcast" "$(token_url --subscribe "$(subscribe_grant "$broadcast")")" "$version" drop
    subscriber_pid="$HARNESS_PID"
    harness_wait "$subscriber_pid" || true
    if grep -qx 'tail drop clean end=4 groups=0,2,3 bytes=786432' "$LOGS/$name-sub.log"; then
        printf 'clean end\n' >&9
        harness_wait "$pid" || true
        if grep -qx 'tail acknowledged' "$LOGS/$name-pub.log"; then
            echo "  PASS  drop $version $pub -> $sub (group 1 cut; clean end 4)"
            record pass "${cell[@]}" seconds=$((SECONDS - started))
        else
            echo "  FAIL  drop $version $pub -> $sub (publisher did not acknowledge)"
            record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="publisher did not acknowledge"
            overall=1
        fi
    else
        echo "  FAIL  drop $version $pub -> $sub (drop did not settle before timeout)"
        record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="drop did not settle before timeout"
        overall=1
        cat "$LOGS/$name-pub.log" "$LOGS/$name-sub.log"
        harness_reap "$pid"
    fi
    exec 9>&-
    rm "$fifo"
}

# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_tail_publisher() {
    local fifo="$1"
    shift
    run_tail_client "$@" <"$fifo" 9>&-
}

# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_tail_subscriber() {
    local fifo="$1"
    shift
    run_tail_client "$@" >"$fifo" 8>&-
}

run_tail_pair() {
    local pub="$1" sub="$2" kind="${3:-tail}" version="${4:-}" pid subscriber_pid started=$SECONDS from read
    local name="${kind// /-}-$1-$2${4:+-$4}" label="$kind${4:+ $4}"
    local fifo="$LOGS/tail-ack" broadcast="$name-$$-$RANDOM" mode="" want='tail clean end=4 groups=0,1,2,3 bytes=1048576'
    [[ -n "$version" ]] || version=$(pinned_version)
    # moq-net subscribes over moq-transport from the largest group, which can be past group 0 by
    # the time the relay answers, so the reader takes any contiguous run through the last group.
    if [[ "$version" == moq-transport-* ]]; then
        mode="join"
        want='tail join clean end=4 groups=[0-3](,[0-3])*'
    fi
    if [[ "$kind" == datagram ]]; then
        mode=datagram
        want='datagram clean groups=0,1,2,3 bytes=3600'
    elif [[ "$kind" == "track status" ]]; then
        mode=status
        want="track status answered"
    elif [[ "$kind" == compress ]]; then
        mode="compress"
    fi
    local cell=(case="$kind" publisher="$pub" subscriber="$sub" log="$LOGS/$name-sub.log")
    [[ -z "${4:-}" ]] || cell+=(version="$version")
    if is_broken "$pub" || is_broken "$sub"; then
        echo "  FAIL  $label $pub -> $sub (client unavailable)"
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    mkfifo "$fifo"
    # Open both ends so neither child startup nor a failed publisher can block the harness.
    exec 9<>"$fifo"
    from=$(relay_next_line)
    # Each side's token grants this broadcast alone, like a media round's.
    harness_spawn "$name-pub" "$LOGS/$name-pub.log" run_tail_publisher "$fifo" "$pub" publish "$broadcast" "$(token_url --publish "$(publish_grant "$broadcast")")" "$version" "$mode"
    pid="$HARNESS_PID"
    if [[ "$mode" == datagram ]]; then
        # Subscription metadata, on the clients' serialized task runtimes, confirms the alias is bound. An acknowledgement
        # per datagram avoids testing a best-effort receive buffer's burst capacity.
        mkfifo "$LOGS/datagram-ack"
        exec 8<>"$LOGS/datagram-ack"
        harness_spawn "$name-sub" "$LOGS/$name-sub.log" run_tail_subscriber "$LOGS/datagram-ack" "$sub" subscribe "$broadcast" "$(token_url --subscribe "$(subscribe_grant "$broadcast")")" "$version" "$mode"
        subscriber_pid="$HARNESS_PID"
        local ack sequence
        if read -r -t "$TIMEOUT" ack <&8 && [[ "$ack" == "datagram ready" ]]; then
            for sequence in 0 1 2 3; do
                printf 'datagram %s\n' "$sequence" >&9
                read -r -t "$TIMEOUT" ack <&8 && [[ "$ack" == "datagram $sequence" ]] || break
            done
        fi
        exec 8>&-
        rm "$LOGS/datagram-ack"
    else
        harness_spawn "$name-sub" "$LOGS/$name-sub.log" run_tail_client "$sub" subscribe "$broadcast" "$(token_url --subscribe "$(subscribe_grant "$broadcast")")" "$version" "$mode"
        subscriber_pid="$HARNESS_PID"
    fi
    harness_wait "$subscriber_pid" || true
    # Like run_native, a verified data result survives the NAPI addon's exit crash.
    if read=$(grep -xE "$want" "$LOGS/$name-sub.log") && { [[ "$mode" != status ]] || grep -qx "track status answered" "$LOGS/$name-sub.log"; }; then
        printf 'clean end\n' >&9
        harness_wait "$pid" || true
        if [[ -n "$version" ]] && ! check_versions "$label $pub -> $sub" "$from" "$version"; then
            record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="a relay connection negotiated another version"
            overall=1
        elif grep -qx 'tail acknowledged' "$LOGS/$name-pub.log"; then
            echo "  PASS  $kind $pub -> $sub (${read#tail })"
            record pass "${cell[@]}" seconds=$((SECONDS - started))
        else
            echo "  FAIL  $label $pub -> $sub (publisher did not acknowledge)"
            record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="publisher did not acknowledge"
            overall=1
            cat "$LOGS/$name-pub.log"
        fi
    else
        echo "  FAIL  $label $pub -> $sub (missing groups or clean end)"
        record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="missing groups or clean end"
        overall=1
        cat "$LOGS/$name-pub.log" "$LOGS/$name-sub.log"
        harness_reap "$pid"
    fi
    exec 9>&-
    rm "$fifo"
}

# A late joiner. The publisher writes group 0's head before any subscriber exists and the rest only
# once the subscriber reports reading that head, so only a join from object 0 passes (a joining
# FETCH, a draft-22 fill, or a moq-lite group), never one that merely outran the publisher.
run_late_pair() {
    local pub="$1" sub="$2" pid subscriber_pid started=$SECONDS from version head=""
    local name="late-$1-$2" fifo="$LOGS/late-ack" heads="$LOGS/late-head" broadcast="late-$1-$2-$$-$RANDOM"
    local cell=(case="late join" publisher="$pub" subscriber="$sub" log="$LOGS/$name-sub.log")
    version=$(pinned_version)
    if is_broken "$pub" || is_broken "$sub"; then
        echo "  FAIL  late join $pub -> $sub (client unavailable)"
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    mkfifo "$fifo" "$heads"
    exec 9<>"$fifo"
    from=$(relay_next_line)
    harness_spawn "$name-pub" "$LOGS/$name-pub.log" run_tail_publisher "$fifo" "$pub" publish "$broadcast" "$(token_url --publish "$(publish_grant "$broadcast")")" "$version" late
    pid="$HARNESS_PID"
    exec 8<>"$heads"
    harness_spawn "$name-sub" "$LOGS/$name-sub.log" run_tail_subscriber "$heads" "$sub" subscribe "$broadcast" "$(token_url --subscribe "$(subscribe_grant "$broadcast")")" "$version" late
    subscriber_pid="$HARNESS_PID"
    if read -r -t "$TIMEOUT" head <&8 && [[ "$head" == "late head" ]]; then
        printf 'late tail\n' >&9
    fi
    harness_wait "$subscriber_pid" || true
    if [[ "$head" == "late head" ]] && grep -qx 'late join clean groups=0,1 frames=6' "$LOGS/$name-sub.log"; then
        printf 'clean end\n' >&9
        harness_wait "$pid" || true
        if [[ -n "$version" ]] && ! check_versions "late join $pub -> $sub" "$from" "$version"; then
            record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="a relay connection negotiated another version"
            overall=1
        elif grep -qx 'tail acknowledged' "$LOGS/$name-pub.log"; then
            echo "  PASS  late join $pub -> $sub (group 0 from its head; clean end 2)"
            record pass "${cell[@]}" seconds=$((SECONDS - started))
        else
            echo "  FAIL  late join $pub -> $sub (publisher did not acknowledge)"
            record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="publisher did not acknowledge"
            overall=1
            cat "$LOGS/$name-pub.log"
        fi
    else
        local why="missing group 0's head"
        [[ "$head" != "late head" ]] || why="missing objects or clean end"
        echo "  FAIL  late join $pub -> $sub ($why)"
        record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="$why"
        overall=1
        cat "$LOGS/$name-pub.log" "$LOGS/$name-sub.log"
        harness_reap "$pid"
        harness_reap "$subscriber_pid"
    fi
    exec 8>&- 9>&-
    rm "$fifo" "$heads"
}

# This checkout's CLI FETCHes a past group of a finished track, under the same relay close check as
# live readers. Every object has its own bytes, so a FETCH that drops, repeats, or reorders one fails.
run_fetch_round() {
    local pub="$1" name="fetch-$1" fifo="$LOGS/fetch-ack" broadcast="fetch-$1-$$-$RANDOM" pid from version
    local started=$SECONDS why=""
    local cell=(case="CLI FETCH" publisher="$pub" subscriber=rust log="$LOGS/$name-pub.log")
    if is_broken "$pub"; then
        echo "  FAIL  CLI FETCH $pub (client unavailable)"
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    version=$(pinned_version)
    from=$(relay_next_line)
    mkfifo "$fifo"
    exec 9<>"$fifo"
    harness_spawn "$name-pub" "$LOGS/$name-pub.log" run_tail_publisher "$fifo" "$pub" publish "$broadcast" "$(token_url --publish "$(publish_grant "$broadcast")")" "$version" cached
    pid="$HARNESS_PID"
    if timeout -k 3 "$TIMEOUT" "$MOQ" --connect "$(token_url --subscribe "$(subscribe_grant "$broadcast")")" ${version:+--connect-version "$version"} \
        --broadcast "$broadcast" fetch tail --group 1 --json >"$LOGS/$name.json" &&
        python3 - "$LOGS/$name.json" <<'PYCODE'
import base64, json, sys
frames = [json.loads(line) for line in open(sys.argv[1])]
assert [(frame["group"], frame["frame"], frame["size"]) for frame in frames] == [(1, index, 64 * 1024) for index in range(3)], frames
assert [base64.b64decode(frame["payload"], validate=True) for frame in frames] == [bytes([16 + index]) * (64 * 1024) for index in range(3)], "incorrect FETCH payload"
PYCODE
    then
        printf 'clean end\n' >&9
        harness_wait "$pid" || true
        if grep -qx 'tail acknowledged' "$LOGS/$name-pub.log"; then
            echo "  PASS  CLI FETCH $pub (group 1; 3 objects)"
        else
            echo "  FAIL  CLI FETCH $pub (publisher did not acknowledge)"
            why="publisher did not acknowledge"
            overall=1
            cat "$LOGS/$name-pub.log"
        fi
    else
        echo "  FAIL  CLI FETCH $pub"
        why="incorrect or missing FETCH payload"
        overall=1
        cat "$LOGS/$name-pub.log"
        harness_reap "$pid"
    fi
    exec 9>&-
    rm "$fifo"
    check_idle_outs "$name" "$from" || why="${why:-relay connections did not all close cleanly}"
    if [[ -n "$version" ]] && ! check_versions "$name" "$from" "$version"; then
        why="${why:-a relay connection negotiated another version}"
        overall=1
    fi
    cell+=(seconds=$((SECONDS - started)))
    if [[ -z "$why" ]]; then record pass "${cell[@]}"; else record fail "${cell[@]}" detail="$why"; fi
}

# One media.ts invocation. It reports its own verdict (a negative control passes by failing on the
# assertion it names), so the exit code is the whole answer.
MEDIA_RUNS=0
# shellcheck disable=SC2329  # invoked indirectly via media_runs
run_media() {
    local name="$1" log started status=0 sink=()
    shift
    log="$HARNESS_RUN/media-${name//[^[:alnum:]._-]/-}.log"
    started=$SECONDS
    MEDIA_RUNS=$((MEDIA_RUNS + 1))
    # Firefox only resumes an AudioContext on an output device, and a Linux CI runner has none. A
    # private null sink gives it one, and keeps a local run off the desktop's speakers. macOS always
    # has CoreAudio's output, and the dev shell has no PulseAudio there.
    [[ "$(uname -s)" != Linux ]] ||
        sink=(python3 "$WORKSPACE/test/lib/pulse.py" --dir "$HARNESS_RUN/pulse-$MEDIA_RUNS" --)
    (cd "$CLIENTS/js" && "${sink[@]}" bun media.ts --url "$MEDIA_URL" --timeout "$TIMEOUT" "$@") >"$log" 2>&1 ||
        status=$?
    local cell=(case="$name" publisher=js subscriber=js seconds=$((SECONDS - started)) log="$log")
    if [[ "$status" -eq 0 ]]; then
        echo "  PASS  $name ($((SECONDS - started))s)"
        record pass "${cell[@]}"
        # The measurements are the point even when nothing fails: a skew or frame rate creeping
        # toward its bound is worth seeing before it crosses.
        grep -E '^(  |=== )' "$log" || true
    else
        echo "  FAIL  $name ($((SECONDS - started))s)"
        record fail "${cell[@]}" detail="media.ts exited $status"
        sed 's/^/        /' "$log" >&2 || true
        overall=1
    fi
}

# shellcheck disable=SC2329  # invoked indirectly via media_runs
fail_media() {
    record fail case="$1" publisher=js subscriber=js detail="browser client unavailable"
}

# Check that a publisher refused for <broadcast> failed loud: Unauthorized, naming the path.
check_denied() {
    local log="$1" broadcast="$2" plain
    plain=$(sed 's/\x1b\[[0-9;]*m//g' "$log" 2>/dev/null || true)
    grep -qi 'unauthorized' <<<"$plain" && grep -q "outside our grant.*$broadcast" <<<"$plain"
}

# Every publisher's media round in this pass, then the refused publishers and the browser close code.
run_matrix() {
    local pub sub broadcast from why allowed log started
    for pub in "${PUB_LIST[@]}"; do
        broadcast="interop-${pub}-$$-${RANDOM}.hang"
        echo "=== publisher: $pub  broadcast: $broadcast ==="
        if is_broken "$pub"; then
            for sub in "${SUB_LIST[@]}"; do
                echo "  FAIL  $pub -> $sub (publisher client unavailable)"
                record fail case=media publisher="$pub" subscriber="$sub" detail="publisher client unavailable"
            done
            overall=1
            continue
        fi
        # The exact broadcast, not its subtree.
        from=$(relay_next_line)
        start_publisher "$pub" "$pub" "$broadcast" "$(token_url --publish "$(publish_grant "$broadcast")")"
        run_round "$pub" "$broadcast" "$PUB_PID" media "$from"
        if why=$(check_grant "$pub" "$LOGS/pub-$pub.log" "$(grant_line "$broadcast" "")"); then
            if prints_grant "$pub"; then
                echo "  PASS  $pub grant"
                record pass case=grant publisher="$pub" log="$LOGS/pub-$pub.log"
            fi
        else
            echo "  FAIL  $pub $why"
            record fail case=grant publisher="$pub" log="$LOGS/pub-$pub.log" detail="$why"
            overall=1
        fi
    done

    # Negative control for the tone checks: the Rust CLI swaps its channels' tones, which every
    # subscriber decodes fine but must refuse as the wrong tone, proving each one's check can fail.
    # The checks are the same in every pass, so the auto pass covers them.
    if [[ "$AUDIO" -eq 1 && "$VERSION" == auto ]]; then
        broadcast="interop-swapped-$$-${RANDOM}.hang"
        echo "=== publisher: rust  broadcast: $broadcast (tone $SWAPPED_TONE, subscribers expect $STEREO_TONE) ==="
        from=$(relay_next_line)
        start_publisher rust-swapped rust "$broadcast" "$(token_url --publish "$(publish_grant "$broadcast")")" "$SWAPPED_TONE"
        run_round rust-swapped "$broadcast" "$PUB_PID" wrong-tone "$from"
    fi

    # A publisher whose token excludes its broadcast must abort its session with
    # Unauthorized naming the path, and no subscriber may see the broadcast.
    for pub in "${PUB_LIST[@]}"; do
        if ! enforces_grant "$pub" || is_broken "$pub"; then continue; fi
        broadcast="interop-denied-${pub}-$$-${RANDOM}.hang"
        allowed="interop-allowed-*.hang"
        echo "=== publisher: $pub  broadcast: $broadcast (token grants only $allowed) ==="
        from=$(relay_next_line)
        start_publisher "$pub-denied" "$pub" "$broadcast" "$(token_url --publish "$allowed")"
        run_round "$pub-denied" "$broadcast" "$PUB_PID" nothing "$from"
        log="$LOGS/pub-$pub-denied.log"
        if ! check_denied "$log" "$broadcast"; then
            echo "  FAIL  $pub publisher did not fail with Unauthorized naming $broadcast:"
            sed 's/^/        /' "$log" 2>/dev/null || true
            record fail case="unauthorized publisher" publisher="$pub" log="$log" detail="did not fail with Unauthorized naming the broadcast"
            overall=1
        elif ! why=$(check_grant "$pub" "$log" "$(grant_line "$allowed" "")"); then
            echo "  FAIL  $pub $why"
            record fail case="unauthorized publisher" publisher="$pub" log="$log" detail="$why"
            overall=1
        else
            echo "  PASS  $pub publisher refused: Unauthorized"
            record pass case="unauthorized publisher" publisher="$pub" log="$log"
        fi
    done

    # The relay refuses a token that is not a JWT, and Chromium has to read the close code and
    # reason it sends. WebSocket must surface MalformedAuthToken and stop reconnecting too.
    # Chromium is the strict peer here, so no Rust client stands in for it. The relay refuses
    # before any version is negotiated, so one pass covers it.
    if [[ "$VERSION" == auto ]] && needs js; then
        echo "=== browser close code ==="
        if is_broken js; then
            echo "  FAIL  refused session (browser client unavailable)"
            record fail case="refused session" subscriber=js detail="browser client unavailable"
            overall=1
        else
            started=$SECONDS
            if (cd "$CLIENTS/js" && bun close.ts --url "$URL" --timeout "$TIMEOUT") >"$HARNESS_RUN/close.log" 2>&1; then
                echo "  PASS  refused session ($((SECONDS - started))s)"
                record pass case="refused session" subscriber=js seconds=$((SECONDS - started)) log="$HARNESS_RUN/close.log"
            else
                echo "  FAIL  refused session ($((SECONDS - started))s)"
                record fail case="refused session" subscriber=js seconds=$((SECONDS - started)) log="$HARNESS_RUN/close.log" detail="close.ts failed"
                sed 's/^/        /' "$HARNESS_RUN/close.log" >&2 || true
                overall=1
            fi
        fi
    fi
}

# Standalone control peers use the same pinned draft, with OS-assigned endpoint ports.
run_control_case() {
    local runtime="$1" kind="$2" version name="$2-$1" started=$SECONDS
    local cell=(case="$kind" publisher=rust subscriber="$runtime" log="$LOGS/$name.log")
    version=$(pinned_version)
    local command=("$TARGET_BASE/$PROFILE/examples/interop-control" "$kind" "$version" "${runtime##*-}")
    case "$kind" in
        status) cell=(case="wire track status" publisher=rust subscriber="$runtime" log="$LOGS/$name.log") ;;
        goaway) command=("$TARGET_BASE/$PROFILE/examples/interop-goaway" "$version" "${runtime##*-}") ;;
        reverse-goaway)
            command=("$TARGET_BASE/$PROFILE/examples/interop-goaway-client" "$version")
            cell=(case=goaway publisher="$runtime" subscriber=rust log="$LOGS/$name.log")
            ;;
    esac
    if is_broken "$runtime"; then
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    # Own the Rust launcher and its JS child together, including on a deadline or failure.
    harness_spawn "$name" "$LOGS/$name.log" timeout -k 3 "$TIMEOUT" "${command[@]}"
    if harness_wait "$HARNESS_PID"; then
        echo "  PASS  $kind rust <-> $runtime"
        record pass "${cell[@]}" seconds=$((SECONDS - started))
    else
        echo "  FAIL  $kind rust <-> $runtime"
        record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="control peer failed"
        cat "$LOGS/$name.log"
        overall=1
    fi
}

# The bytes the relay has inflated for plain readers, summed over every tier and role.
relay_inflated() {
    curl -sf --max-time 5 "$METRICS_URL" | awk '/^moq_relay_inflated_bytes_total\{/ { sum += $2 } END { print sum + 0 }'
}

# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_catalog_publisher() {
    local fifo="$1"
    shift
    timeout -k 3 $((TIMEOUT * 2)) "$TARGET_BASE/$PROFILE/examples/interop-catalog" "$@" <"$fifo" 9>&-
}

# shellcheck disable=SC2329  # invoked indirectly via harness_spawn
run_catalog_reader() {
    local runtime="$1"
    shift
    case "$runtime" in
        js-native-node) (cd "$CLIENTS/js-native" && timeout -k 3 "$TIMEOUT" node --import tsx catalog.ts "$@") ;;
        js-native-bun) (cd "$CLIENTS/js-native" && timeout -k 3 "$TIMEOUT" bun catalog.ts "$@") ;;
    esac
}

# A Rust publisher's compressed catalog reaches a native JS reader on each of CATALOG_VERSIONS at
# once through the relay, which must inflate the stored DEFLATE group exactly once for the plain
# readers: its inflate counter grows by the catalog's inflated size, not a multiple of it.
run_catalog_fanout() {
    local runtime="$1" started=$SECONDS pid reader version before after bytes from got want readers=() why=""
    local name="catalog-$1" fifo="$LOGS/catalog-ack" broadcast="catalog-$1-$$-$RANDOM"
    local cell=(case="compressed catalog" publisher=rust subscriber="$runtime" log="$LOGS/$name-pub.log")
    if is_broken "$runtime"; then
        echo "  FAIL  compressed catalog rust -> $runtime (client unavailable)"
        record fail "${cell[@]}" detail="client unavailable"
        overall=1
        return
    fi
    mkfifo "$fifo"
    exec 9<>"$fifo"
    from=$(relay_next_line)
    before=$(relay_inflated) || why="relay metrics unavailable"
    # Subtree grants, which moq-transport's AUTH extension can carry too.
    harness_spawn "$name-pub" "$LOGS/$name-pub.log" run_catalog_publisher "$fifo" "$(token_url --publish "$broadcast/**")" "$broadcast"
    pid="$HARNESS_PID"
    for version in "${CATALOG_VERSIONS[@]}"; do
        harness_spawn "$name-$version" "$LOGS/$name-$version.log" run_catalog_reader "$runtime" "$(token_url --subscribe "$broadcast/**")" "$broadcast" "$version"
        readers+=("$HARNESS_PID")
    done
    for reader in "${readers[@]}"; do harness_wait "$reader" || true; done
    for version in "${CATALOG_VERSIONS[@]}"; do
        want="catalog compress=false video=hd,sd audio=en,fr"
        [[ "$version" != moq-lite-07-wip ]] || want="catalog compress=true video=hd,sd audio=en,fr"
        # Like run_native, a verified result survives the NAPI addon's exit crash.
        grep -qx "$want" "$LOGS/$name-$version.log" || why="${why:-$version reader did not report: $want}"
    done
    printf 'clean end\n' >&9
    harness_wait "$pid" || true
    after=$(relay_inflated) || why="${why:-relay metrics unavailable}"
    bytes=$(sed -n 's/^catalog bytes=\([0-9]*\)$/\1/p' "$LOGS/$name-pub.log")
    got=$(relay_log_since "$from" | sed -n 's/.*conn{id=[0-9]* .*negotiated version=\([^ ]*\).*/\1/p' | sort | tr '\n' ' ')
    if [[ -z "$why" ]] && ! grep -qx 'catalog acknowledged' "$LOGS/$name-pub.log"; then
        why="publisher did not acknowledge"
    elif [[ -z "$why" && "$got" != "moq-lite-06 moq-lite-07-wip moq-lite-07-wip moq-transport-22 " ]]; then
        why="relay connections negotiated ${got:-nothing}"
    elif [[ -z "$why" && "$((after - before))" != "$bytes" ]]; then
        why="relay inflated $((after - before)) bytes, not the catalog's ${bytes:-?} once"
    fi
    if [[ -z "$why" ]]; then
        echo "  PASS  compressed catalog rust -> $runtime (${CATALOG_VERSIONS[*]}; one ${bytes}-byte inflate)"
        record pass "${cell[@]}" seconds=$((SECONDS - started))
    else
        echo "  FAIL  compressed catalog rust -> $runtime ($why)"
        record fail "${cell[@]}" seconds=$((SECONDS - started)) detail="$why"
        overall=1
        for version in "${CATALOG_VERSIONS[@]}"; do cat "$LOGS/$name-$version.log"; done
        cat "$LOGS/$name-pub.log"
        harness_reap "$pid"
    fi
    exec 9>&-
    rm "$fifo"
}

# This checkout's CLI FETCH and finite track tails in this pass, then the lite drop lanes.
run_tails() {
    local runtime version
    echo "=== CLI FETCH close ==="
    run_fetch_round rust
    for runtime in js-native-node js-native-bun; do
        ! needs "$runtime" || run_fetch_round "$runtime"
    done
    echo "=== late join ==="
    run_late_pair rust rust
    for runtime in js-native-node js-native-bun; do
        if needs "$runtime"; then
            run_late_pair rust "$runtime"
            run_late_pair "$runtime" rust
        fi
    done
    echo "=== finite track tails ==="
    run_tail_pair rust rust
    for runtime in js-native-node js-native-bun; do
        if needs "$runtime"; then
            run_tail_pair rust "$runtime"
            run_tail_pair "$runtime" rust
            if [[ "$VERSION" == moq-transport-* ]]; then
                run_tail_pair rust "$runtime" datagram
                run_tail_pair "$runtime" rust datagram
                run_control_case "$runtime" status
                run_control_case "$runtime" goaway
                [[ "$runtime" != js-native-bun ]] || run_control_case "$runtime" reverse-goaway
                run_tail_pair "$runtime" rust "track status"
            fi
        fi
    done
    # The compress, compressed catalog, and drop lanes pin their own versions.
    [[ "$VERSION" == auto ]] || return 0
    for runtime in js-native-node js-native-bun; do
        needs "$runtime" || continue
        for version in "${COMPRESS_VERSIONS[@]}"; do
            run_tail_pair rust "$runtime" compress "$version"
            run_tail_pair "$runtime" rust compress "$version"
        done
        run_catalog_fanout "$runtime"
    done
    if needs js-native-node; then
        for version in "${DROP_VERSIONS[@]}"; do
            run_drop_pair rust js-native-node "$version"
            run_drop_pair js-native-node rust "$version"
        done
    fi
}

if [[ "$MEDIA" -eq 1 ]]; then
    # One token for the whole run: every media case publishes and watches its own broadcast.
    MEDIA_URL=$(token_url --publish '**' --subscribe '**')
    # Media output and lifecycle, browser to browser, against the deterministic fixture. The
    # negative controls below inject a defect and name the assertion that has to catch it; each
    # passes only by failing there, which is what keeps the positive run from being vacuous.
    if is_broken js; then
        echo "  FAIL  media checks (browser client unavailable)"
        for engine in "${MEDIA_ENGINE_LIST[@]}"; do media_runs fail_media "$engine"; done
        overall=1
    else
        echo "=== media output and lifecycle ==="
        for engine in "${MEDIA_ENGINE_LIST[@]}"; do media_runs run_media "$engine"; done
    fi
elif [[ "$NEGATIVE" -eq 1 ]]; then
    # Negative control: no publisher. Every subscriber must FAIL (time out with
    # no data), proving the harness can actually report failure.
    echo "=== negative control: subscribers expect NO data ==="
    run_round "none" "interop-missing-$$-$RANDOM.hang" "" nothing "$(relay_next_line)"
else
    for VERSION in "${VERSION_LIST[@]}"; do
        LOGS="$HARNESS_RUN"
        if [[ "$VERSION" != auto ]]; then
            echo "##### version: $VERSION #####"
            LOGS="$HARNESS_RUN/$VERSION"
            mkdir -p "$LOGS"
        fi
        [[ "$TAIL_ONLY" -eq 1 || "$FINISH_ONLY" -eq 1 ]] || run_matrix
        if [[ "$IN_TREE" -eq 1 ]]; then
            [[ "$TAIL_ONLY" -eq 1 ]] || run_finishes
            [[ "$FINISH_ONLY" -eq 1 ]] || run_tails
        fi
    done
fi

if [[ "$overall" -eq 0 ]]; then
    echo "interop: all checks passed"
else
    # The relay's view is often the only place that says WHY a session died
    # (auth rejection, protocol error, close codes), so surface it on failure.
    echo "interop: FAILURES detected" >&2
    echo "--- relay log (last 150 lines) ---" >&2
    tail -n 150 "$HARNESS_RUN/relay.log" 2>/dev/null | sed 's/^/  relay: /' >&2 || true
fi
exit "$overall"
