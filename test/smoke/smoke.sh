#!/usr/bin/env bash
# Cross-language media interop smoke test against the PUBLIC packages.
#
# Unlike the in-repo smoke test (which builds from the moq-dev/moq workspace),
# this stands up a relay and clients installed straight from public package
# registries: cargo / brew / apt for the Rust binaries, PyPI for Python, the
# Go module proxy for Go, and npm for the browser. The point is to catch breakage
# that only a real consumer would see: a missing wheel, a stale formula, an
# export that didn't survive packaging.
#
# It stands up a real moq-relay, then for each publisher language publishes an
# H.264 broadcast and confirms every subscriber sees data flowing (a non-empty
# frame before the timeout). We check that bytes move end-to-end across
# implementations, not that H.264 decodes.
#
# With --relay URL (repeatable) it skips the local relay and runs the same matrix
# through each external relay instead; relays.sh drives that mode against the
# public relays in the moq-interop-runner registry. An external http(s) or moqt
# URL is that transport alone: the WebSocket fallback stays off. A ws(s) URL is
# the WebSocket column, which relays.sh only adds for relays that opted in.
# On those external URLs the browser (js-web) and bun (js-bun) run only on
# WebTransport. The bun polyfill has no raw-QUIC mode. A client that does not
# belong is left out of the results, not recorded as a skip.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CLIENTS="$SMOKE_DIR/clients"

PUBLISHERS="rust"
SUBSCRIBERS="rust"
TIMEOUT="${SMOKE_TIMEOUT:-20}"
FPS="${SMOKE_FPS:-30}"
SIZE="${SMOKE_SIZE:-320x240}"
PORT="${SMOKE_PORT:-4443}"
URL="http://127.0.0.1:${PORT}"
NEGATIVE=0
EXTERNAL_RELAYS=() # --relay URLs; empty means stand up the local moq-relay
RESULTS=""         # --results FILE: append one TSV row per cell (see record())
LOGS=""            # --logs DIR: keep each cell's subscriber + publisher log (see record())

# Binaries under test. Whatever channel installed them (cargo/brew/apt) just has
# to leave them on PATH; override here to point at a specific build.
RELAY="${RELAY_BIN:-moq-relay}"
# Name of the docker-channel relay container, shared with clients/docker/moq-relay
# so cleanup() can reap it after a SIGKILL of the wrapper (see cleanup()). Exported
# so the wrapper binds the same name; ignored by the non-docker channels.
RELAY_CONTAINER="${MOQ_RELAY_CONTAINER:-moq-relay-smoke}"
export MOQ_RELAY_CONTAINER="$RELAY_CONTAINER"
# The CLI binary under test. The package/crate is still distributed as moq-cli in
# some channels, but the installed executable is `moq`. Honor MOQ_BIN if set.
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
        --relay)
            require_value "$@"
            EXTERNAL_RELAYS+=("$2")
            shift 2
            ;;
        --results)
            require_value "$@"
            RESULTS="$2"
            shift 2
            ;;
        --logs)
            require_value "$@"
            LOGS="$2"
            shift 2
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

TMP=$(mktemp -d)
RELAY_PID=""
PY=""             # python interpreter with moq-rs installed (set in prepare)
GO_SMOKE=""       # compiled Go client binary (set in prepare)
SWIFT_SMOKE=""    # compiled Swift client binary (set in prepare)
KOTLIN_SMOKE=""   # Kotlin run script from `gradle installDist` (set in prepare)
C_SMOKE=""        # C client, hand-rolled -I/-L/-l build (set in prepare)
C_PC_SMOKE=""     # C client, built via pkg-config + moq.pc (set in prepare)
C_CMAKE_SMOKE=""  # C client, built via the shipped CMake package (set in prepare)
LIBMOQ_ROOT=""    # extracted libmoq prebuilt tarball root, shared by the 3 C builds
LIBMOQ_OS_LIBS="" # system libs for the hand-rolled C build (set in libmoq_fetch)
GST_PLUGIN_DIR="" # dir holding the moq-gst plugin (libgstmoq.{so,dylib}; set in prepare)
BROKEN_LANGS=""   # clients whose public package failed to install/build

mark_broken() {
    # A client whose published package won't install/build fails only its own
    # matrix cells, instead of aborting the whole run. That's the point: a broken
    # registry artifact should show up as a red cell, not hide every other result.
    BROKEN_LANGS="$BROKEN_LANGS $1"
    echo "  WARN  $1 client unavailable: $2"
}

is_broken() {
    local lang="$1" x
    for x in $BROKEN_LANGS; do [[ "$x" == "$lang" ]] && return 0; done
    return 1
}

kill_tree() {
    # SIGKILL, depth-first. moq ignores SIGTERM (handles only SIGINT), so a
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
    # The docker-channel relay runs in a container that outlives a SIGKILL of its
    # wrapper's runtime client, so kill_tree alone leaves it holding the port and
    # blocking the next run in the same job. Reap it by name -- but only when the
    # docker wrapper is actually the relay, so a non-docker run on a host that
    # happens to have docker doesn't delete an unrelated container of that name.
    if [[ "$RELAY" == */clients/docker/moq-relay ]] && have "${SMOKE_DOCKER:-docker}"; then
        "${SMOKE_DOCKER:-docker}" rm -f "$RELAY_CONTAINER" >/dev/null 2>&1 || true
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT

have() { command -v "$1" >/dev/null 2>&1; }

# Run a command up to N times with a short growing backoff. For fetches from
# registries/proxies that fail transiently (sum.golang.org stream resets, etc.);
# without it one blip marks a client broken and fails its whole column.
retry() {
    local n="$1" i
    shift
    for ((i = 1; i <= n; i++)); do
        "$@" && return 0
        [[ $i -lt $n ]] && sleep $((i * 2))
    done
    return 1
}

# Download a release tarball to a file and extract it into a directory. Two
# deliberate choices: curl retries every failure (--retry alone skips partial
# transfers, and a truncated body is exactly the failure GitHub's CDN produces),
# and the archive lands on disk before tar sees it, so a short read can't leave a
# half-extracted tree behind while `tar xz` reports only "unexpected end of file".
fetch_tarball() {
    local url="$1" dir="$2"
    local archive="$dir/.download.tar.gz"
    mkdir -p "$dir"
    if ! curl -sfL --retry 5 --retry-delay 2 --retry-all-errors -o "$archive" "$url" ||
        ! tar xzf "$archive" -C "$dir"; then
        echo "download/extract failed: $url" >&2
        return 1
    fi
    rm -f "$archive"
}

require_tools() {
    # Resolve the CLI binary name unless MOQ_BIN pinned it.
    if [[ -z "$MOQ" ]]; then
        MOQ=moq
    fi
    # Only the relay, CLI, and harness essentials are hard requirements. A missing
    # per-client toolchain (uv / go / bun / swift / gradle / cc) just marks that
    # client broken in prepare, so it fails its own cells instead of the whole run.
    local missing=() t
    for t in ffmpeg curl pgrep timeout; do
        have "$t" || missing+=("$t")
    done
    if [[ ${#EXTERNAL_RELAYS[@]} -eq 0 ]]; then
        have "$RELAY" || missing+=("$RELAY (cargo/brew/apt/nix install moq-relay)")
    fi
    have "$MOQ" || missing+=("moq (cargo/brew/apt install moq-cli, or nix build github:moq-dev/moq#moq)")
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "error: missing required tools: ${missing[*]}" >&2
        exit 1
    fi
}

# Run swift with the Xcode toolchain. A loaded nix devShell exports SDKROOT /
# DEVELOPER_DIR pointing at a nix Apple SDK, which the Xcode swiftc rejects;
# clear those so swift picks its own matching SDK.
run_swift() {
    env -u SDKROOT -u DEVELOPER_DIR -u NIX_CFLAGS_COMPILE -u NIX_CFLAGS_LINK \
        -u CPATH -u LIBRARY_PATH -u MACOSX_DEPLOYMENT_TARGET swift "$@"
}

# Download the latest libmoq prebuilt release for this platform. Sets LIBMOQ_ROOT
# (extracted tarball root) and LIBMOQ_OS_LIBS (system libs for the hand-rolled
# build). Idempotent: the three C build variants share one download. The tarball
# ships include/moq.h, lib/libmoq.a, lib/pkgconfig/moq.pc and lib/cmake/moq/.
libmoq_fetch() {
    [[ -n "$LIBMOQ_ROOT" && -d "$LIBMOQ_ROOT" ]] && return 0
    local target tag ver url hdr=()
    have curl || {
        echo "curl required" >&2
        return 1
    }
    have jq || {
        echo "jq required" >&2
        return 1
    }
    case "$(uname -s)/$(uname -m)" in
        Darwin/arm64) target=aarch64-apple-darwin ;;
        Darwin/x86_64) target=x86_64-apple-darwin ;;
        Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
        Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-gnu ;;
        *)
            echo "unsupported platform: $(uname -s)/$(uname -m)" >&2
            return 1
            ;;
    esac
    case "$(uname -s)" in
        Darwin) LIBMOQ_OS_LIBS="-framework CoreFoundation -framework Security -framework CoreServices -framework Foundation -framework AVFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox -framework ScreenCaptureKit -framework CoreGraphics -lc++" ;;
        # No -lva: moq-vaapi dlopen's libva at runtime, so libmoq.a carries no
        # libva symbols and a libva-less host just falls back to another encoder.
        *) LIBMOQ_OS_LIBS="-lpthread -ldl -lm -lstdc++" ;;
    esac
    # Latest libmoq-v* release, never pinned. Authenticated if a token is around
    # (CI) to dodge the 60/hr anonymous GitHub API limit.
    [[ -n "${GITHUB_TOKEN:-}" ]] && hdr=(-H "Authorization: Bearer $GITHUB_TOKEN")
    # macOS ships bash 3.2, where "${hdr[@]}" on an empty array trips `set -u`;
    # the ${arr[@]+...} guard expands to nothing when the array is unset/empty.
    tag=$(curl -sf --retry 3 --retry-all-errors ${hdr[@]+"${hdr[@]}"} "https://api.github.com/repos/moq-dev/moq/releases?per_page=100" |
        jq -r '.[].tag_name' | grep '^libmoq-v' | head -1)
    [[ -n "$tag" ]] || {
        echo "no libmoq-v* release found" >&2
        return 1
    }
    ver=${tag#libmoq-v}
    url="https://github.com/moq-dev/moq/releases/download/$tag/moq-$ver-$target.tar.gz"
    echo "libmoq $tag ($target)"
    fetch_tarball "$url" "$TMP/libmoq" || return 1
    LIBMOQ_ROOT="$TMP/libmoq/moq-$ver-$target"
}

# Hand-rolled build: -I/-L/-l with system libs we hardcode here. The baseline
# "does the static archive link at all" check; ignores moq.pc and the CMake
# config entirely.
c_build_direct() {
    local cc="${CC:-cc}" out="$1"
    have "$cc" || {
        echo "no C compiler ($cc) on PATH" >&2
        return 1
    }
    # shellcheck disable=SC2086  # LIBMOQ_OS_LIBS is a deliberate multi-flag word list
    "$cc" "$CLIENTS/c/subscribe.c" -I"$LIBMOQ_ROOT/include" -L"$LIBMOQ_ROOT/lib" -lmoq \
        $LIBMOQ_OS_LIBS -o "$out"
}

# pkg-config build: resolve cflags/libs from the shipped moq.pc, the way a real
# consumer (e.g. FFmpeg --enable-libmoq) does. libmoq is a static archive, so
# --static pulls Libs.private (the system frameworks/libs) too. A wrong
# includedir/libdir or a missing framework in moq.pc fails here -- exactly the
# breakage the hand-rolled build above can't see.
c_build_pkgconfig() {
    local cc="${CC:-cc}" out="$1" cflags libs
    have "$cc" || {
        echo "no C compiler ($cc) on PATH" >&2
        return 1
    }
    have pkg-config || {
        echo "pkg-config not on PATH (apt install pkg-config / brew install pkgconf)" >&2
        return 1
    }
    export PKG_CONFIG_PATH="$LIBMOQ_ROOT/lib/pkgconfig"
    cflags=$(pkg-config --cflags moq) || {
        echo "pkg-config --cflags moq failed" >&2
        return 1
    }
    libs=$(pkg-config --static --libs moq) || {
        echo "pkg-config --static --libs moq failed" >&2
        return 1
    }
    # shellcheck disable=SC2086  # cflags/libs are deliberate multi-flag word lists
    "$cc" "$CLIENTS/c/subscribe.c" $cflags $libs -o "$out"
}

# CMake build: find_package(moq) against the shipped lib/cmake/moq package, the
# way a CMake consumer imports the moq::moq target. A broken config (wrong paths,
# missing target or system libs) fails configure/build here. CMAKE_PREFIX_PATH
# points at the extracted tarball root; the binary lands at <builddir>/smoke.
c_build_cmake() {
    local builddir="$1"
    have cmake || {
        echo "cmake not on PATH (apt install cmake / brew install cmake)" >&2
        return 1
    }
    rm -rf "$builddir"
    cmake -S "$CLIENTS/c" -B "$builddir" -DCMAKE_PREFIX_PATH="$LIBMOQ_ROOT" \
        -DCMAKE_BUILD_TYPE=Release >/dev/null || {
        echo "cmake configure failed" >&2
        return 1
    }
    cmake --build "$builddir" >/dev/null || {
        echo "cmake build failed" >&2
        return 1
    }
}

# Download the latest moq-gst prebuilt release for this platform and confirm the
# plugin loads against the host's *system* GStreamer. The plugin (libgstmoq.so /
# .dylib) dynamic-links libgstreamer, so this is the customer-facing scenario:
# install the .deb / brew tap / tarball, then `gst-inspect-1.0 moq`. Sets
# GST_PLUGIN_DIR to the dir holding the plugin. moqsrc connects over the network,
# so it's relay-channel-agnostic; GST_PLUGIN_DIR + a system GStreamer is all it
# needs. (Local nix shells have no GStreamer, and a prebuilt plugin wouldn't link
# against nixpkgs' gst anyway, so this cell wants a real system gst install.)
gst_prepare() {
    local target tag asset_name url root releases hdr=()
    have gst-launch-1.0 || {
        echo "gst-launch-1.0 not on PATH (apt install gstreamer1.0-tools / brew install gstreamer)" >&2
        return 1
    }
    have gst-inspect-1.0 || {
        echo "gst-inspect-1.0 not on PATH" >&2
        return 1
    }
    # Escape hatch: point at a locally-built plugin dir (e.g. target/release after
    # `cargo build -p moq-gst`) instead of the published tarball, mirroring the
    # RELAY_BIN/MOQ_BIN overrides. Skips the download but still load-checks it.
    if [[ -n "${MOQ_GST_PLUGIN_DIR:-}" ]]; then
        GST_PLUGIN_DIR="$MOQ_GST_PLUGIN_DIR"
        echo "moq-gst (local: $GST_PLUGIN_DIR)"
        [[ -d "$GST_PLUGIN_DIR" ]] || {
            echo "MOQ_GST_PLUGIN_DIR is not a directory: $GST_PLUGIN_DIR" >&2
            return 1
        }
        GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_PLUGIN_SYSTEM_PATH_1_0="" \
            GST_REGISTRY_1_0="$TMP/gst-registry.bin" \
            gst-inspect-1.0 moq 2>/dev/null | grep -qE '^[[:space:]]+moqsrc:' ||
            {
                echo "moqsrc not exposed (plugin failed to load against this GStreamer)" >&2
                return 1
            }
        return 0
    fi
    have curl || {
        echo "curl required" >&2
        return 1
    }
    have jq || {
        echo "jq required" >&2
        return 1
    }
    case "$(uname -s)/$(uname -m)" in
        Darwin/arm64) target=aarch64-apple-darwin ;;
        Darwin/x86_64) target=x86_64-apple-darwin ;;
        Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
        Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-gnu ;;
        *)
            echo "unsupported platform: $(uname -s)/$(uname -m)" >&2
            return 1
            ;;
    esac
    # Latest moq-gst-v* release, never pinned. Authenticated if a token is around
    # (CI) to dodge the 60/hr anonymous GitHub API limit.
    [[ -n "${GITHUB_TOKEN:-}" ]] && hdr=(-H "Authorization: Bearer $GITHUB_TOKEN")
    releases="$TMP/moq-gst-releases.json"
    curl -sf --retry 3 --retry-all-errors ${hdr[@]+"${hdr[@]}"} \
        "https://api.github.com/repos/moq-dev/moq/releases?per_page=100" -o "$releases" || {
        echo "failed to query moq-gst releases" >&2
        return 1
    }
    tag=$(jq -r '.[].tag_name' "$releases" | grep '^moq-gst-v' | head -1)
    [[ -n "$tag" ]] || {
        echo "no moq-gst-v* release found" >&2
        return 1
    }
    # Release asset names are part of the release metadata, not a derivation of
    # the crate version. In particular, 0.3.7 added the tag's `v` to the archive
    # and extracted-directory name. Select the platform asset that was actually
    # published so future naming changes fail as a missing asset, not a guessed URL.
    asset_name=$(jq -r --arg tag "$tag" --arg suffix "-$target.tar.gz" \
        '.[] | select(.tag_name == $tag) | .assets[].name |
        select(startswith("moq-gst-") and endswith($suffix))' "$releases" | head -1)
    [[ -n "$asset_name" ]] || {
        echo "no moq-gst asset for $target in release $tag" >&2
        return 1
    }
    url=$(jq -r --arg tag "$tag" --arg name "$asset_name" \
        '.[] | select(.tag_name == $tag) | .assets[] |
        select(.name == $name) | .browser_download_url' "$releases" | head -1)
    [[ -n "$url" ]] || {
        echo "moq-gst asset has no download URL: $asset_name" >&2
        return 1
    }
    echo "moq-gst $tag ($target)"
    fetch_tarball "$url" "$TMP/moq-gst" || return 1
    root="$TMP/moq-gst/${asset_name%.tar.gz}"
    GST_PLUGIN_DIR="$root/lib/gstreamer-1.0"
    [[ -d "$GST_PLUGIN_DIR" ]] || {
        echo "plugin dir missing in tarball: $GST_PLUGIN_DIR" >&2
        return 1
    }
    # gst-inspect exits 0 even when the .so fails to load, so grep for the
    # factory. Isolate discovery to our dir + a temp registry so a system-wide moq
    # plugin can't shadow it (mirrors moq-gst's own smoke.sh).
    GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_PLUGIN_SYSTEM_PATH_1_0="" \
        GST_REGISTRY_1_0="$TMP/gst-registry.bin" \
        gst-inspect-1.0 moq 2>/dev/null | grep -qE '^[[:space:]]+moqsrc:' ||
        {
            echo "moqsrc not exposed (plugin failed to load against this GStreamer)" >&2
            return 1
        }
}

# ── setup ───────────────────────────────────────────────────────────────────
require_tools

# Surface any "we've drifted off latest" problem up front. Non-fatal here so the
# matrix still runs; `just freshness` / CI enforce it hard.
"$SMOKE_DIR/freshness.sh" || echo "WARN: freshness check failed (see above); continuing" >&2

if [[ ${#EXTERNAL_RELAYS[@]} -eq 0 ]]; then
    echo "relay:   $(command -v "$RELAY")"
else
    echo "relays:  ${EXTERNAL_RELAYS[*]}"
fi
echo "moq:     $(command -v "$MOQ")"

# moq-cli 0.12 renamed --client-connect to --connect and rejects the old name.
# Pick whichever this build accepts until every channel ships 0.12.
# `--connect-websocket-enabled` contains `--connect`, and `-` is a word
# boundary, so the match has to end on the option that takes the URL.
moq_help=$("$MOQ" --help 2>&1 || true)
if grep -qE -- '(^|[[:space:]])--connect([[:space:]=]|$)' <<<"$moq_help"; then
    MOQ_CONNECT=--connect
else
    MOQ_CONNECT=--client-connect
fi
# Present once moq-cli grew `--connect-websocket-enabled`. Older binaries ignore
# the env var below and still race the fallback; they reject an unknown flag.
MOQ_WS_OFF=()
if grep -qF -- '--connect-websocket-enabled' <<<"$moq_help"; then
    MOQ_WS_OFF=(--connect-websocket-enabled=false)
fi

# The local relay may race its WebSocket binding. An external ws(s) URL is the
# WebSocket column. Every other external URL is WebTransport or raw QUIC.
allow_websocket() {
    [[ ${#EXTERNAL_RELAYS[@]} -eq 0 || "$URL" == ws://* || "$URL" == wss://* ]]
}

# Run a client. When this URL must not race WebSocket, say so in the environment.
# The Rust CLI reads MOQ_CONNECT_WEBSOCKET_ENABLED. The browser page and native JS
# read SMOKE_WEBSOCKET. Python, Go, Swift, Kotlin, and C have no published switch
# for it yet; the relays lane does not use them.
run_client() {
    if allow_websocket; then
        "$@"
    else
        SMOKE_WEBSOCKET=0 MOQ_CONNECT_WEBSOCKET_ENABLED=false "$@"
    fi
}

rust_moq() {
    # Clap takes the argv after --connect as the URL. A flag in that position
    # is not a value, so the websocket switch goes before --connect.
    if ! allow_websocket && [[ ${#MOQ_WS_OFF[@]} -gt 0 ]]; then
        run_client "$MOQ" "${MOQ_WS_OFF[@]}" "$MOQ_CONNECT" "$@"
    else
        run_client "$MOQ" "$MOQ_CONNECT" "$@"
    fi
}

if needs python; then
    echo "installing python client (moq-rs from PyPI)..."
    PY="$TMP/venv/bin/python"
    # Latest published wheel; sdist fallback needs a Rust toolchain + C compiler.
    if ! have uv; then
        mark_broken python "uv not found"
    elif uv venv --quiet "$TMP/venv" && uv pip install --quiet --python "$PY" moq-rs; then :; else
        mark_broken python "uv pip install moq-rs failed"
    fi
fi

if needs go; then
    echo "building go client (moq.dev/moq from the module proxy)..."
    GO_SMOKE="$TMP/go-smoke"
    # Pull the latest published module, then build the client against it. The
    # ergonomic moq.dev/moq wrapper pulls a transitive moq.dev/moq-ffi; `go get`
    # records only the wrapper's own checksum, so `go mod tidy` fetches the rest (no
    # go.sum is committed -- freshness bans lockfiles -- so it's regenerated here).
    # The module proxy / sum.golang.org occasionally reset mid-stream; retry the
    # fetch (idempotent) and build only once it succeeds.
    # shellcheck disable=SC2329  # invoked via retry
    go_fetch() (cd "$CLIENTS/go" && go get "moq.dev/moq@latest" && go mod tidy)
    if ! have go; then
        mark_broken go "go not found"
    elif (retry 3 go_fetch && cd "$CLIENTS/go" && CGO_ENABLED=1 go build -o "$GO_SMOKE" .) >"$TMP/go-build.log" 2>&1; then :; else
        mark_broken go "go get/build of moq.dev/moq failed"
        sed 's/^/        /' "$TMP/go-build.log" >&2 || true
    fi
fi

# The browser client ships three delivery variants (js-web = vite, js-esbuild,
# js-jsdelivr) that all drive the same <moq-publish>/<moq-watch> elements; they
# differ only in how the published npm packages reach the page.
if needs js-web || needs js-esbuild || needs js-jsdelivr; then
    echo "installing browser clients (@moq/watch + @moq/publish from npm)..."
    if ! have bun; then
        for v in js-web js-esbuild js-jsdelivr; do mark_broken "$v" "bun not found"; done
    else
        js_base() {
            (cd "$CLIENTS/js" && bun install) || return 1
            # Nix provides Chromium via PLAYWRIGHT_BROWSERS_PATH; otherwise fetch it.
            [[ -n "${PLAYWRIGHT_BROWSERS_PATH:-}" ]] || (cd "$CLIENTS/js" && bunx playwright install chromium) || return 1
        }
        if js_base >"$TMP/js-base.log" 2>&1; then
            # jsdelivr imports from the CDN at runtime, so it needs no build. The
            # bundler variants each build their own page; a build failure fails
            # only that variant.
            if needs js-web && ! (cd "$CLIENTS/js" && bunx vite build) >"$TMP/js.log" 2>&1; then
                mark_broken js-web "vite build failed"
                sed 's/^/        /' "$TMP/js.log" >&2 || true
            fi
            if needs js-esbuild && ! (cd "$CLIENTS/js" && bun build-esbuild.ts) >"$TMP/js-esbuild.log" 2>&1; then
                mark_broken js-esbuild "esbuild build failed"
                sed 's/^/        /' "$TMP/js-esbuild.log" >&2 || true
            fi
        else
            for v in js-web js-esbuild js-jsdelivr; do mark_broken "$v" "bun install / playwright failed"; done
            sed 's/^/        /' "$TMP/js-base.log" >&2 || true
        fi
    fi
fi

# Native (non-browser) JS: the published @moq/net + @moq/hang under a runtime
# with no native WebTransport, using moq's own @moq/web-transport polyfill.
# Run under bun (js-bun) and node (js-node).
if needs js-bun || needs js-node; then
    echo "installing native-js client (@moq/net + @moq/hang + webtransport polyfill)..."
    if ! have bun; then
        for v in js-bun js-node; do mark_broken "$v" "bun not found (needed to install)"; done
    elif (cd "$CLIENTS/js-native" && bun install) >"$TMP/js-native.log" 2>&1; then
        if needs js-node && ! have node; then
            mark_broken js-node "node not found"
        fi
    else
        for v in js-bun js-node; do mark_broken "$v" "bun install failed"; done
        sed 's/^/        /' "$TMP/js-native.log" >&2 || true
    fi
fi

if needs swift; then
    echo "building swift client (moq-dev/moq-swift via SPM)..."
    SWIFT_SMOKE="$CLIENTS/swift/.build/debug/smoke"
    # `from: 0.2.0` + `swift package update` resolves the latest 0.x each run.
    if ! have swift; then
        mark_broken swift "swift not found (needs the Xcode toolchain; macOS only)"
    elif (cd "$CLIENTS/swift" && run_swift package update && run_swift build) >"$TMP/swift-build.log" 2>&1; then :; else
        mark_broken swift "swift package update / build failed"
        sed 's/^/        /' "$TMP/swift-build.log" >&2 || true
    fi
fi

if needs kotlin; then
    echo "building kotlin client (dev.moq:moq from Maven Central)..."
    KOTLIN_SMOKE="$CLIENTS/kotlin/build/install/smoke/bin/smoke"
    # `latest.release` + disabled dynamic-version caching resolves the newest each run.
    if ! have gradle; then
        mark_broken kotlin "gradle not found"
    elif (cd "$CLIENTS/kotlin" && gradle --quiet --console=plain installDist) >"$TMP/kotlin-build.log" 2>&1; then :; else
        mark_broken kotlin "gradle installDist failed"
        sed 's/^/        /' "$TMP/kotlin-build.log" >&2 || true
    fi
fi

# The C client ships three build variants (c, c-pkgconfig, c-cmake) that all
# produce the same subscribe-only binary from the same prebuilt libmoq; they
# differ only in how the build discovers it: hand-rolled flags, pkg-config (the
# moq.pc consumer path), or find_package (the CMake package consumer path).
if needs c || needs c-pkgconfig || needs c-cmake; then
    echo "building c client(s) against the libmoq prebuilt release..."
    if libmoq_fetch >"$TMP/c-fetch.log" 2>&1; then
        cat "$TMP/c-fetch.log"
        if needs c; then
            C_SMOKE="$TMP/c-smoke"
            if c_build_direct "$C_SMOKE" >"$TMP/c-build.log" 2>&1; then :; else
                mark_broken c "direct compile failed"
                sed 's/^/        /' "$TMP/c-build.log" >&2 || true
            fi
        fi
        if needs c-pkgconfig; then
            C_PC_SMOKE="$TMP/c-pc-smoke"
            if c_build_pkgconfig "$C_PC_SMOKE" >"$TMP/c-pc-build.log" 2>&1; then :; else
                mark_broken c-pkgconfig "pkg-config build failed"
                sed 's/^/        /' "$TMP/c-pc-build.log" >&2 || true
            fi
        fi
        if needs c-cmake; then
            C_CMAKE_SMOKE="$TMP/c-cmake/smoke"
            if c_build_cmake "$TMP/c-cmake" >"$TMP/c-cmake-build.log" 2>&1; then :; else
                mark_broken c-cmake "cmake build failed"
                sed 's/^/        /' "$TMP/c-cmake-build.log" >&2 || true
            fi
        fi
    else
        sed 's/^/        /' "$TMP/c-fetch.log" >&2 || true
        for v in c c-pkgconfig c-cmake; do mark_broken "$v" "libmoq download failed"; done
    fi
fi

if needs gst; then
    echo "installing gstreamer client (moq-gst prebuilt release)..."
    if gst_prepare >"$TMP/gst-build.log" 2>&1; then :; else
        mark_broken gst "moq-gst download / plugin load failed"
        sed 's/^/        /' "$TMP/gst-build.log" >&2 || true
    fi
fi

if [[ ${#EXTERNAL_RELAYS[@]} -eq 0 ]]; then
    # A relay from a previous run in the same job is reaped by cleanup(), but the
    # kernel can take a moment to release the (host-network) socket afterwards, so
    # wait briefly for the port to clear before treating it as a real conflict.
    for _ in $(seq 1 20); do
        curl -sf "$URL/certificate.sha256" >/dev/null 2>&1 || break
        sleep 0.5
    done
    if curl -sf "$URL/certificate.sha256" >/dev/null 2>&1; then
        echo "error: something is already listening on 127.0.0.1:4443 (stale relay?)" >&2
        exit 1
    fi

    echo "starting relay on 127.0.0.1:${PORT}..."
    # smoke.toml is the source of truth; rewrite its port into a scratch copy so a
    # busy 4443 (a dev relay, a parallel run) doesn't require editing the committed file.
    # Relays before 0.15 (no --listen flag) reject the renamed keys, so they get the
    # legacy layout until every channel ships 0.15.
    relay_config="$SMOKE_DIR/smoke.toml"
    relay_help=$("$RELAY" --help 2>&1 || true)
    if ! grep -q -- '--listen\b' <<<"$relay_help"; then
        relay_config="$SMOKE_DIR/smoke-legacy.toml"
    fi
    echo "relay config: $(basename "$relay_config")"
    sed "s/4443/${PORT}/g" "$relay_config" >"$TMP/relay.toml"
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

# The driver.ts --variant for a browser cell. js-web is the vite bundle.
js_variant() {
    case "$1" in
        js-web) echo vite ;;
        js-esbuild) echo esbuild ;;
        js-jsdelivr) echo jsdelivr ;;
        *)
            echo "unknown browser variant: $1" >&2
            return 1
            ;;
    esac
}

# Sets global PUB_PID. Called in the current shell (no command substitution) so
# $! refers to the backgrounded job and kill_tree can reap the whole pipeline.
# Every publisher just consumes the same ffmpeg Annex-B stream on stdin; the
# client frames it (moq / the FFI importers only frame-and-forward, ffmpeg
# encodes).
PUB_PID=""
start_publisher() {
    local lang="$1" broadcast="$2" log="$TMP/pub-$1.log"
    case "$lang" in
        rust)
            (ffmpeg_h264 | rust_moq "$URL" --broadcast "$broadcast" import avc3) >"$log" 2>&1 &
            ;;
        python)
            (ffmpeg_h264 | run_client "$PY" "$CLIENTS/python/smoke.py" \
                publish --url "$URL" --broadcast "$broadcast") >"$log" 2>&1 &
            ;;
        go)
            (ffmpeg_h264 | run_client "$GO_SMOKE" publish --url "$URL" --broadcast "$broadcast") >"$log" 2>&1 &
            ;;
        js-web | js-esbuild | js-jsdelivr)
            # Headless Chromium encodes its own H.264 from a fake camera via
            # WebCodecs (lazily, once a subscriber creates demand). The variant
            # selects how the published packages reach the page.
            (cd "$CLIENTS/js" && run_client bun driver.ts publish --variant "$(js_variant "$lang")" \
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
            # moq only handles SIGINT, so -k forces SIGKILL if it ignores the
            # SIGTERM that fires when no data arrives within the timeout.
            local n
            local -a ws=()
            # stderr stays in the cell log: it names the negotiated version.
            # timeout is a binary, so the flag has to be on its argv; the env
            # prefix still reaches moq through it. The websocket switch goes
            # before --connect so clap takes the URL as that option's value.
            if ! allow_websocket && [[ ${#MOQ_WS_OFF[@]} -gt 0 ]]; then
                ws=("${MOQ_WS_OFF[@]}")
            fi
            n=$(run_client timeout -k 3 "$TIMEOUT" "$MOQ" ${ws[@]+"${ws[@]}"} "$MOQ_CONNECT" "$URL" --broadcast "$broadcast" \
                export fmp4 | head -c 1 | wc -c | tr -d ' ' || true)
            [[ "${n:-0}" -ge 1 ]]
            ;;
        python)
            # moq-rs occasionally SIGABRTs from its dedicated runtime thread
            # during process teardown, after the subscriber has received data.
            # Retry only that abnormal exit once; ordinary protocol/time-out
            # failures remain failures, and a repeated abort is still surfaced.
            local status
            if RUST_BACKTRACE="${RUST_BACKTRACE:-1}" run_client "$PY" "$CLIENTS/python/smoke.py" \
                subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"; then
                return 0
            else
                status=$?
            fi
            [[ "$status" -eq 134 ]] || return "$status"
            echo "warning: moq-rs subscriber aborted during teardown; retrying once" >&2
            RUST_BACKTRACE="${RUST_BACKTRACE:-1}" run_client "$PY" "$CLIENTS/python/smoke.py" \
                subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        go)
            run_client "$GO_SMOKE" subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        swift)
            run_client "$SWIFT_SMOKE" subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        kotlin)
            run_client "$KOTLIN_SMOKE" subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        c | c-pkgconfig | c-cmake)
            # Same subscribe-only client; the three cells differ only in how it
            # was built (hand-rolled, pkg-config, or CMake) against the same
            # prebuilt libmoq. The run is identical.
            local bin
            case "$lang" in
                c) bin="$C_SMOKE" ;;
                c-pkgconfig) bin="$C_PC_SMOKE" ;;
                c-cmake) bin="$C_CMAKE_SMOKE" ;;
            esac
            run_client "$bin" subscribe --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT"
            ;;
        gst)
            # moqsrc exposes the broadcast's video as a Sometimes pad (video_%u,
            # ANY caps); gst-launch links it to filesink once it appears. We pipe
            # the raw frames to stdout and grab one byte, the same "bytes moved"
            # bar (and the same head -c 1 early-exit idiom) as the rust subscriber
            # -- no decode. head closing the pipe SIGPIPEs gst-launch, so success
            # returns at once; no data just runs out the timeout. Our plugin dir
            # rides on top of the system path (which provides filesink); a private
            # registry keeps the scan off the user's cache. buffer-mode=2 makes
            # filesink unbuffered so the first frame reaches head immediately.
            local n
            n=$(GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" GST_REGISTRY_1_0="$TMP/gst-run-registry.bin" \
                run_client timeout -k 3 "$TIMEOUT" gst-launch-1.0 -q \
                moqsrc url="$URL" broadcast="$broadcast" ! filesink location=/dev/stdout buffer-mode=2 \
                2>/dev/null | head -c 1 | wc -c | tr -d ' ' || true)
            [[ "${n:-0}" -ge 1 ]]
            ;;
        js-web | js-esbuild | js-jsdelivr)
            # Headless Chromium decodes via WebCodecs; exits 0 once a frame lands.
            (cd "$CLIENTS/js" && run_client bun driver.ts subscribe --variant "$(js_variant "$lang")" \
                --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT")
            ;;
        js-bun)
            # Native @moq/net via the WebTransport polyfill, under bun.
            (cd "$CLIENTS/js-native" && run_client bun subscribe.ts subscribe \
                --url "$URL" --broadcast "$broadcast" --timeout "$TIMEOUT")
            ;;
        js-node)
            # Same, under node (tsx runs the TS directly).
            (cd "$CLIENTS/js-native" && run_client node --import tsx subscribe.ts subscribe \
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

negotiated() {
    # negotiated <log>: the wire version a client reported, or nothing. The Rust
    # and native-JS clients log `connected version=moq-transport-18`; the browser page logs the
    # WebTransport subprotocol it was handed (see clients/js/jsdelivr/setup.js),
    # whose `moqt-16` ALPN spelling is normalized to match.
    [[ -f "$1" ]] || return 0
    sed 's/\x1b\[[0-9;]*m//g' "$1" |
        grep -oE 'connected version=[^ ]+|negotiated protocol: [^ ]+' | tail -1 |
        sed -E 's/.*(=|: )//; s/^moqt-/moq-transport-/' || true
}

failure_reason() {
    # failure_reason <log>: the first error-looking line of a cell log, minus its
    # timestamp/level/target prefix and the browser's [page] tag. The first error is
    # usually the cause; later ones tend to be the teardown it triggered.
    [[ -f "$1" ]] || return 0
    sed 's/\x1b\[[0-9;]*m//g' "$1" |
        grep -m1 -E '(^|[^a-z])(err|error|Error|ERROR|failed|Failed|timed out)([^a-z]|$)' |
        sed -E 's/^[0-9T:.-]+Z +[A-Z]+ +[a-z0-9_:]+: //; s/^\[page\] //; s/^[[:space:]]+//' |
        tr '\t' ' ' | cut -c1-240 || true
}

log_dir() {
    # log_dir: where --logs keeps the current relay's cell logs, relative to $LOGS.
    sed -E 's#[^A-Za-z0-9._-]+#_#g; s#_+$##' <<<"$URL"
}

record() {
    # record <pub> <sub> <PASS|FAIL|SKIP> [note]: print the cell and, with
    # --results, append a TSV row keyed by the relay URL: relay, pub, sub, status,
    # pub version, sub version, reason (why it failed or was skipped), and the
    # subscriber / publisher log paths under --logs. relays.sh summarises these.
    local pub="$1" sub="$2" status="$3" note="${4:-}" pub_ver="" sub_ver=""
    local reason="$note" sub_log="" pub_log="" dir
    if [[ "$status" != SKIP ]]; then
        pub_ver=$(negotiated "$TMP/pub-$pub.log")
        sub_ver=$(negotiated "$TMP/$pub-$sub.log")
    fi
    if [[ "$status" == FAIL && -z "$reason" ]]; then
        reason=$(failure_reason "$TMP/$pub-$sub.log")
        [[ -n "$reason" ]] || reason=$(failure_reason "$TMP/pub-$pub.log" | sed 's/^/publisher: /')
        [[ -n "$reason" ]] || reason="no data within ${TIMEOUT}s"
    fi
    # The publisher log is copied once its round ends (see run_round).
    if [[ -n "$LOGS" && -z "$note" && "$status" != SKIP ]]; then
        dir=$(log_dir)
        mkdir -p "$LOGS/$dir"
        if [[ -f "$TMP/$pub-$sub.log" ]]; then
            sub_log="$dir/$pub-$sub.log"
            cp "$TMP/$pub-$sub.log" "$LOGS/$sub_log"
        fi
        [[ "$pub" == none ]] || pub_log="$dir/pub-$pub.log"
    fi
    echo "  $status  $pub -> $sub${note:+ ($note)}${sub_ver:+ [$sub_ver]}"
    [[ "$status" == FAIL ]] && overall=1
    if [[ -n "$RESULTS" ]]; then
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$URL" "$pub" "$sub" "$status" \
            "$pub_ver" "$sub_ver" "$reason" "$sub_log" "$pub_log" >>"$RESULTS"
    fi
    return 0
}

belongs() {
    # belongs <lang>: this client is part of the matrix for $URL.
    # The local relay runs whoever was requested. On external relays the browser
    # and bun run only on WebTransport (http/https). @moq/web-transport is HTTP/3
    # and has no raw-QUIC mode, so bun is not dialed at moqt://. Anything else is
    # left out, not recorded as a skip.
    [[ ${#EXTERNAL_RELAYS[@]} -eq 0 ]] && return 0
    case "$1" in
        js-web | js-esbuild | js-jsdelivr | js-node | js-bun)
            [[ "$URL" == http://* || "$URL" == https://* ]]
            ;;
        *) return 0 ;;
    esac
}

run_round() {
    local pub="$1" broadcast="$2" pub_pid="$3"
    # pids[i] is the subscriber's PID, or "STATUS:note" for a cell decided
    # without running it; either way cells are reported in --subscribers order.
    local pids=() names=() i sub
    for sub in "${SUB_LIST[@]}"; do
        if ! belongs "$sub"; then
            continue
        fi
        names+=("$sub")
        if is_broken "$sub"; then
            pids+=("FAIL:subscriber client unavailable")
        else
            (run_subscriber "$sub" "$broadcast") >"$TMP/$pub-$sub.log" 2>&1 &
            pids+=("$!")
        fi
    done
    # A publisher that streams forever should still be alive; if it died, the
    # subscriber failures below are a publisher bug, so surface its log.
    if [[ -n "$pub_pid" ]] && ! kill -0 "$pub_pid" 2>/dev/null; then
        echo "  WARN  publisher '$pub' exited early:"
        sed 's/^/        /' "$TMP/pub-$pub.log" 2>/dev/null || true
    fi
    local want_pass=1 got
    [[ "$NEGATIVE" -eq 1 ]] && want_pass=0
    # ${arr[@]+...} guard: --subscribers may be empty,
    # and bash 3.2 (macOS) errors on "${!pids[@]}" for an empty array under `set -u`.
    for i in ${pids[@]+"${!pids[@]}"}; do
        if [[ "${pids[$i]}" == *:* ]]; then
            record "$pub" "${names[$i]}" "${pids[$i]%%:*}" "${pids[$i]#*:}"
            continue
        fi
        if wait "${pids[$i]}"; then got=1; else got=0; fi
        if [[ "$got" -eq "$want_pass" ]]; then
            record "$pub" "${names[$i]}" PASS
        else
            record "$pub" "${names[$i]}" FAIL
            sed 's/^/        /' "$TMP/$pub-${names[$i]}.log" 2>/dev/null || true
        fi
    done
    if [[ -n "$pub_pid" ]]; then
        kill_tree "$pub_pid"
        wait "$pub_pid" 2>/dev/null || true
        if [[ -n "$LOGS" && -f "$TMP/pub-$pub.log" ]]; then
            mkdir -p "$LOGS/$(log_dir)"
            cp "$TMP/pub-$pub.log" "$LOGS/$(log_dir)/pub-$pub.log"
        fi
        # Don't let cleanup() later signal this now-reaped (possibly recycled) PID.
        [[ "${PUB_PID:-}" == "$pub_pid" ]] && PUB_PID=""
    fi
    return 0
}

run_matrix() {
    local pub sub broadcast
    if [[ "$NEGATIVE" -eq 1 ]]; then
        # Negative control: no publisher. Every subscriber must FAIL (time out with
        # no data), proving the harness can actually report failure.
        echo "=== negative control: subscribers expect NO data ==="
        run_round "none" "smoke-missing-$$-$RANDOM.hang" ""
        return
    fi
    for pub in "${PUB_LIST[@]}"; do
        broadcast="smoke-${pub}-$$-${RANDOM}.hang"
        if ! belongs "$pub"; then
            continue
        fi
        echo "=== publisher: $pub  broadcast: $broadcast ==="
        if is_broken "$pub"; then
            # Same filter as run_round: a client that does not belong on this
            # URL stays omitted, not a failure invented by the broken publisher.
            for sub in "${SUB_LIST[@]}"; do
                if ! belongs "$sub"; then
                    continue
                fi
                record "$pub" "$sub" FAIL "publisher client unavailable"
            done
            continue
        fi
        start_publisher "$pub" "$broadcast"
        run_round "$pub" "$broadcast" "$PUB_PID"
    done
}

if [[ ${#EXTERNAL_RELAYS[@]} -eq 0 ]]; then
    run_matrix
else
    for URL in "${EXTERNAL_RELAYS[@]}"; do
        echo "##### relay: $URL #####"
        run_matrix
        # Completion marker: lets relays.sh tell a finished matrix from one cut
        # short after its first cells.
        if [[ -n "$RESULTS" ]]; then printf '%s\tDONE\n' "$URL" >>"$RESULTS"; fi
    done
fi

if [[ "$overall" -eq 0 ]]; then
    echo "smoke: all checks passed"
else
    echo "smoke: FAILURES detected" >&2
fi
exit "$overall"
