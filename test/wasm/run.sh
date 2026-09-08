#!/usr/bin/env bash
# Headless-browser coverage for the @moq/wasm bindings.
#
# rs/moq-wasm is `#![cfg(target_arch = "wasm32")]`, so a host-target
# `cargo check --workspace` compiles it to nothing, and `just rs wasm` only
# compiles it. Neither says whether the bindings still work, which is how a
# missing `with_protocols` shipped on main unable to open a session at all
# (#2811). This runs them: a real moq-relay, real WebTransport, and the
# generated `js/wasm/dist` loaded by a real browser.
#
# One relay per protocol flavour, because negotiation is the part that broke:
#   lite   default versions        -> moq-lite-05 over its own ALPN
#   ietf   --server-version 19     -> moq-transport-19 over its own ALPN
#   setup  --server-version lite-02 -> the "moql" ALPN, version chosen by SETUP
#
# The publisher is @moq/net (TypeScript) and the subscriber is @moq/wasm (Rust),
# so each case is also an interop check. See README.md.
set -euo pipefail

WASM_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$WASM_DIR/../.." && pwd)

# Every run writes into a debug bundle instead of a temp dir, so a failure keeps
# its relay logs, browser trace, and stacks. See test/README.md.
# shellcheck source=../lib/bundle.sh disable=SC1091
source "$WORKSPACE/test/lib/bundle.sh"

TIMEOUT="${WASM_TIMEOUT:-30}"
PORT="${WASM_PORT:-4460}"

# Cargo profile for the relay. Debug compiles faster, which is what a test
# fixture wants; the workload is three connections and a few hundred KiB.
PROFILE="${WASM_PROFILE:-debug}"
RELAY="${RELAY_BIN:-}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --timeout)
            if [[ $# -lt 2 || -z "${2:-}" || "$2" == -* ]]; then
                echo "error: --timeout requires a value" >&2
                exit 2
            fi
            TIMEOUT="$2"
            shift 2
            ;;
        *)
            echo "unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

# Zero is rejected along with the non-numeric: it parses, but every case then
# times out on the first tick, which reads as nine broken bindings.
if [[ ! "$TIMEOUT" =~ ^[0-9]+(\.[0-9]+)?$ ]] || [[ "$TIMEOUT" =~ ^0+(\.0+)?$ ]]; then
    echo "error: timeout must be a positive number (got '$TIMEOUT')" >&2
    exit 2
fi

# name:version-flag:expected-version. An empty flag leaves the relay at its
# defaults, which is the ALPN both sides prefer.
FLAVOURS=(
    "lite::moq-lite-05"
    "ietf:moq-transport-19:moq-transport-19"
    "setup:moq-lite-02:moq-lite-02"
)

# The relays take PORT and the next few, so the whole span has to be bindable.
# Port 0 is the trap worth naming: the relay would bind an arbitrary port while
# this script polls 0 forever.
if [[ ! "$PORT" =~ ^[0-9]+$ ]] || ((PORT < 1024 || PORT + ${#FLAVOURS[@]} - 1 > 65535)); then
    echo "error: port must be 1024..$((65535 - ${#FLAVOURS[@]} + 1)) (got '$PORT')" >&2
    exit 2
fi

bundle_init wasm
bundle_rerun just test wasm --timeout "$TIMEOUT"
TMP="$BUNDLE_WORK"
RELAY_PIDS=()

# shellcheck disable=SC2329  # invoked indirectly via 'trap cleanup EXIT'
cleanup() {
    local status=$? pid
    # A relay still up here is one the run never finished with, and killing it
    # is what erases where it was. Read the stacks before the signals.
    if [[ "$status" -ne 0 ]]; then
        for pid in ${RELAY_PIDS[@]+"${RELAY_PIDS[@]}"}; do
            bundle_stack "relay-$pid" "$pid"
        done
        if [[ -n "${MOQ_QA_QLOG:-}" ]] && ! find "$BUNDLE_QLOG" -type f -print -quit | grep -q .; then
            bundle_capability qlog "requested, but the relays wrote no traces: this backend cannot capture them"
        fi
    fi
    if [[ "$status" -eq 0 ]] || ! bundle_retained; then
        for pid in ${RELAY_PIDS[@]+"${RELAY_PIDS[@]}"}; do
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        done
    fi
    bundle_finish "$status"
}
trap cleanup EXIT

for tool in cargo bun; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool not found; run inside 'nix develop'" >&2
        exit 1
    }
done
command -v wasm-bindgen >/dev/null 2>&1 || {
    echo "error: wasm-bindgen not found; run inside 'nix develop'" >&2
    exit 1
}

# ── build ───────────────────────────────────────────────────────────────────
echo "building @moq/wasm..."
(cd "$WORKSPACE" && just wasm)

echo "building moq-relay ($PROFILE)..."
flag=()
[[ "$PROFILE" == "debug" ]] || flag=(--profile "$PROFILE")
# qlog is a cargo feature, so capturing traces means building a different relay.
# Opt-in for that reason: it recompiles quinn with the qlog encoder, which a run
# that is only going to pass has no use for.
[[ -z "${MOQ_QA_QLOG:-}" ]] || flag+=(--features moq-relay/qlog)
(cd "$WORKSPACE" && "${RUST_CARGO:-cargo}" build --locked ${flag[@]+"${flag[@]}"} -p moq-relay)
TARGET_BASE="${CARGO_TARGET_DIR:-$WORKSPACE/target}"
[[ -n "$RELAY" ]] || RELAY="$TARGET_BASE/$PROFILE/moq-relay"
# Which binary the stacks and logs below came out of: a backtrace is only as
# useful as the symbols it can be matched against.
bundle_binary moq-relay "$RELAY"

cd "$WASM_DIR"
bun install --frozen-lockfile
# Playwright ships its own Chromium; PLAYWRIGHT_BROWSERS_PATH means CI installed
# it already, in which case this is a no-op anyway.
bunx playwright install chromium

# Type-check against the .d.ts wasm-bindgen just generated. This is the only
# thing that reads it, and it is worth reading: wasm-bindgen resolves a type in
# a signature by its Rust identifier alone, so a binding can compile, run, and
# still publish typings that name the wrong class. Not part of `just check`,
# which never builds js/wasm/dist.
echo "type-checking against js/wasm/dist/moq.d.ts..."
bunx tsc --noEmit

echo "bundling the harness page..."
rm -rf dist
bun build src/main.ts --outdir dist --target browser >/dev/null
cp index.html dist/index.html

# ── relays ──────────────────────────────────────────────────────────────────
entries=()
offset=0
for flavour in "${FLAVOURS[@]}"; do
    IFS=':' read -r name version_flag expected <<<"$flavour"
    port=$((PORT + offset))
    offset=$((offset + 1))
    url="http://127.0.0.1:${port}"

    if curl -sf "$url/certificate.sha256" >/dev/null 2>&1; then
        echo "error: something is already listening on 127.0.0.1:${port} (stale relay?)" >&2
        exit 1
    fi

    args=("$WASM_DIR/relay.toml" --server-bind "127.0.0.1:${port}" --web-http-listen "127.0.0.1:${port}")
    [[ -z "$version_flag" ]] || args+=(--server-version "$version_flag")
    # One directory per flavour, so a trace can be told apart by the protocol it
    # negotiated rather than only by the connection id inside it.
    [[ -z "${MOQ_QA_QLOG:-}" ]] || {
        mkdir -p "$BUNDLE_QLOG/$name"
        args+=(--server-quic-qlog "$BUNDLE_QLOG/$name")
    }

    echo "starting $name relay on 127.0.0.1:${port}..."
    "$RELAY" "${args[@]}" >"$TMP/relay-$name.log" 2>&1 &
    relay_pid=$!
    RELAY_PIDS+=("$relay_pid")
    bundle_endpoint "$name" "$url" "$expected"
    bundle_process "moq-relay-$name" "$relay_pid"

    # Polled tight rather than on a half-second tick: a relay binds in about
    # 130ms, so a coarse interval spends most of the wait asleep, three times over.
    for _ in $(seq 1 600); do
        curl -sf "$url/certificate.sha256" >/dev/null 2>&1 && break
        sleep 0.05
    done
    if ! curl -sf "$url/certificate.sha256" >/dev/null 2>&1; then
        echo "$name relay never became ready" >&2
        sed 's/^/  relay: /' "$TMP/relay-$name.log" >&2 || true
        exit 1
    fi

    entries+=("{\"name\":\"$name\",\"url\":\"$url\",\"version\":\"$expected\"}")
done

printf '[%s]\n' "$(
    IFS=,
    echo "${entries[*]}"
)" >"$TMP/relays.json"

# ── run ─────────────────────────────────────────────────────────────────────
# `set +e` around the pipeline rather than `|| status=...`: PIPESTATUS is reset
# by the next simple command, so the `||` branch would read the status of its
# own assignment. Both halves matter -- a `tee` that could not write leaves the
# bundle without the transcript the driver just printed.
set +e
MOQ_QA_LABEL=wasm bun driver.ts --relays "$TMP/relays.json" --timeout "$TIMEOUT" 2>&1 |
    tee "$TMP/driver.log"
pipe=("${PIPESTATUS[@]}")
set -e

status="${pipe[0]}"
if [[ "${pipe[1]}" -ne 0 ]]; then
    echo "error: the driver log could not be written to $TMP/driver.log" >&2
    status=1
fi
if [[ $status -eq 0 ]]; then
    bundle_result suite pass
else
    bundle_result suite fail "" "driver exited $status"
fi

if [[ $status -ne 0 ]]; then
    for flavour in "${FLAVOURS[@]}"; do
        name="${flavour%%:*}"
        echo "── $name relay log ──" >&2
        sed 's/^/  /' "$TMP/relay-$name.log" >&2 || true
    done
fi

exit "$status"
