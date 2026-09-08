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

# Run directory, reserved ports, and process-group ownership. See test/README.md.
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$WASM_DIR/../lib/harness.sh"

# Captured before the parse below consumes it, so the rerun command carries every
# flag this run was actually given.
RERUN="just test wasm$(harness_argv "$@")"

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
if [[ -n "$PORT" ]] && { [[ ! "$PORT" =~ ^[0-9]+$ ]] || ((PORT < 1024 || PORT > 65535)); }; then
    echo "error: port must be 1024..65535 (got '$PORT')" >&2
    exit 2
fi

harness_begin wasm "$RERUN"

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
(cd "$WORKSPACE" && "${RUST_CARGO:-cargo}" build --locked ${flag[@]+"${flag[@]}"} -p moq-relay)
TARGET_BASE="${CARGO_TARGET_DIR:-$WORKSPACE/target}"
[[ -n "$RELAY" ]] || RELAY="$TARGET_BASE/$PROFILE/moq-relay"

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
    if curl -sf "$url/certificate.sha256" >/dev/null 2>&1; then
        echo "error: something is already listening on 127.0.0.1:${port} (stale relay?)" >&2
        exit 1
    fi

    args=("$WASM_DIR/relay.toml" --server-bind "127.0.0.1:${port}" --web-http-listen "127.0.0.1:${port}")
    [[ -z "$version_flag" ]] || args+=(--server-version "$version_flag")

    echo "starting $name relay on 127.0.0.1:${port}..."
    harness_spawn "relay-$name" "$HARNESS_RUN/relay-$name.log" "$RELAY" "${args[@]}"

    if ! harness_ready "$url/certificate.sha256" 30 "$HARNESS_PID"; then
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
# Spawned rather than run in the foreground so a SIGTERM lands while the shell is
# in `wait`, where a trap can run: bash defers a trap until a foreground child
# returns, which would leave Chromium behind for as long as the driver hangs.
status=0
harness_spawn driver - bun driver.ts --relays "$HARNESS_RUN/relays.json" --timeout "$TIMEOUT"
harness_wait "$HARNESS_PID" || status=$?

if [[ $status -ne 0 ]]; then
    for flavour in "${FLAVOURS[@]}"; do
        name="${flavour%%:*}"
        echo "── $name relay log ──" >&2
        sed 's/^/  /' "$HARNESS_RUN/relay-$name.log" >&2 || true
    done
fi

exit $status
