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
# shellcheck source=../lib/harness.sh disable=SC1091
source "$WORKSPACE/test/lib/harness.sh"

TIMEOUT="${WASM_TIMEOUT:-30}"
# Empty means "any reserved port per flavour"; WASM_PORT pins the first instead.
PORT="${WASM_PORT:-}"

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

# WASM_PORT pins the first relay and the rest are reserved individually, so only
# the first has to be a real port. Port 0 is the trap worth naming: the relay
# would bind an arbitrary port while this script polls 0 forever.
if [[ -n "$PORT" ]] && ! harness_valid_port "$PORT"; then
    echo "error: port must be 1024..65535 (got '$PORT')" >&2
    exit 2
fi

bundle_init wasm
harness_env_array
rerun_env=("${HARNESS_ENV[@]}" "WASM_PORT=$PORT" "WASM_PROFILE=$PROFILE")
[[ -z "$RELAY" ]] || rerun_env+=("RELAY_BIN=$RELAY")
[[ -z "${MOQ_QA_QLOG:-}" ]] || rerun_env+=("MOQ_QA_QLOG=$MOQ_QA_QLOG")
bundle_rerun env "${rerun_env[@]}" just test wasm --timeout "$TIMEOUT"
TMP="$BUNDLE_WORK"
HARNESS_RUN="$TMP"
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
        if [[ -n "${MOQ_QA_QLOG:-}" ]] && ! find "$BUNDLE_QLOG_LIVE" -type f -print -quit | grep -q .; then
            bundle_capability qlog "requested, but the relays wrote no traces: this backend cannot capture them"
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

if [[ -n "$RELAY" ]]; then
    [[ -x "$RELAY" ]] || {
        echo "error: RELAY_BIN is not executable: $RELAY" >&2
        exit 1
    }
else
    echo "building moq-relay ($PROFILE)..."
    flag=()
    [[ "$PROFILE" == "debug" ]] || flag=(--profile "$PROFILE")
    # qlog is a cargo feature, so capturing traces means building a different relay.
    # Opt-in for that reason: it recompiles quinn with the qlog encoder, which a run
    # that is only going to pass has no use for.
    [[ -z "${MOQ_QA_QLOG:-}" ]] || flag+=(--features moq-relay/qlog)
    (cd "$WORKSPACE" && "${RUST_CARGO:-cargo}" build --locked ${flag[@]+"${flag[@]}"} -p moq-relay)
    TARGET_BASE="${CARGO_TARGET_DIR:-$WORKSPACE/target}"
    RELAY="$TARGET_BASE/$PROFILE/moq-relay"
fi
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
# One reservation per flavour rather than a span from a fixed base: a span means
# every concurrent run needs a different base, and nothing was handing them out.
entries=()
want="$PORT"
for flavour in "${FLAVOURS[@]}"; do
    IFS=':' read -r name version_flag expected <<<"$flavour"
    harness_port "$name" "$want"
    port="$HARNESS_PORT"
    # WASM_PORT pins only the first; the rest come from the reservation walk.
    want=""
    url="http://127.0.0.1:${port}"

    # The reservation covers other harness runs, not the rest of the machine, so
    # still refuse a port some unrelated process is already serving on.
    if harness_probe "$url/certificate.sha256"; then
        echo "error: something is already listening on 127.0.0.1:${port} (stale relay?)" >&2
        exit 1
    fi

    args=("$WASM_DIR/relay.toml" --server-bind "127.0.0.1:${port}" --web-http-listen "127.0.0.1:${port}")
    [[ -z "$version_flag" ]] || args+=(--server-version "$version_flag")
    # One directory per flavour, so a trace can be told apart by the protocol it
    # negotiated rather than only by the connection id inside it.
    [[ -z "${MOQ_QA_QLOG:-}" ]] || {
        mkdir -p "$BUNDLE_QLOG_LIVE/$name"
        args+=(--server-quic-qlog "$BUNDLE_QLOG_LIVE/$name")
    }

    echo "starting $name relay on 127.0.0.1:${port}..."
    harness_spawn "relay-$name" "$TMP/relay-$name.log" "$RELAY" "${args[@]}"
    relay_pid="$HARNESS_PID"
    RELAY_PIDS+=("$relay_pid")
    bundle_endpoint "$name" "$url" "$expected"
    bundle_process "moq-relay-$name" "$relay_pid"

    if ! harness_ready "$url/certificate.sha256" 30 "$relay_pid"; then
        echo "$name relay never became ready" >&2
        sed 's/^/  relay: /' "$HARNESS_RUN/relay-$name.log" >&2 || true
        exit 1
    fi
    harness_endpoint "$name" "$url"

    entries+=("{\"name\":\"$name\",\"url\":\"$url\",\"version\":\"$expected\"}")
done

printf '[%s]\n' "$(
    IFS=,
    echo "${entries[*]}"
)" >"$HARNESS_RUN/relays.json"

# ── run ─────────────────────────────────────────────────────────────────────
# Keep Chromium in a process group owned by the harness while preserving the
# transcript on the terminal and in the bundle.
run_driver() {
    local pipe status
    set +e
    MOQ_QA_LABEL=wasm bun driver.ts --relays "$TMP/relays.json" --timeout "$TIMEOUT" 2>&1 |
        tee "$TMP/driver.log"
    pipe=("${PIPESTATUS[@]}")
    set -e
    status="${pipe[0]}"
    if [[ "${pipe[1]}" -ne 0 ]]; then
        echo "error: the driver log could not be written to $TMP/driver.log" >&2
        return 1
    fi
    return "$status"
}
status=0
harness_spawn driver - run_driver
harness_wait "$HARNESS_PID" || status=$?
if [[ $status -eq 0 ]]; then
    bundle_result suite pass
else
    bundle_result suite fail "" "driver exited $status"
fi

if [[ $status -ne 0 ]]; then
    for flavour in "${FLAVOURS[@]}"; do
        name="${flavour%%:*}"
        echo "── $name relay log ──" >&2
        sed 's/^/  /' "$HARNESS_RUN/relay-$name.log" >&2 || true
    done
fi

exit "$status"
