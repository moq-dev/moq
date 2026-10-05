#!/usr/bin/env bash
# A relay drain, end to end: a JS viewer watching a live track through relay A
# migrates to relay B when A drains, without a dropped group. See README.md.
set -euo pipefail

DRAIN_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DRAIN_DIR/../.." && pwd)

# Run directory, reserved ports, and process-group ownership. See test/README.md.
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DRAIN_DIR/../lib/harness.sh"

# Captured before the parse below consumes it, so the rerun command carries every
# flag and every environment override this run was actually given.
RERUN="$(harness_env DRAIN_TIMEOUT DRAIN_PROFILE RELAY_BIN)just test drain$(harness_argv "$@")"

TIMEOUT="${DRAIN_TIMEOUT:-60}"
PROFILE="${DRAIN_PROFILE:-debug}"
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

if [[ ! "$TIMEOUT" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: timeout must be a positive whole number of seconds (got '$TIMEOUT')" >&2
    exit 2
fi

harness_begin drain "$RERUN"

for tool in cargo bun curl; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool not found; run inside 'nix develop'" >&2
        exit 1
    }
done

# ── build ───────────────────────────────────────────────────────────────────
echo "building moq-relay ($PROFILE)..."
flag=()
[[ "$PROFILE" == "debug" ]] || flag=(--profile "$PROFILE")
(cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay)
TARGET_BASE="${CARGO_TARGET_DIR:-$WORKSPACE/target}"
[[ -n "$RELAY" ]] || RELAY="$TARGET_BASE/$PROFILE/moq-relay"

(cd "$WORKSPACE" && bun install --frozen-lockfile)

# ── relays ──────────────────────────────────────────────────────────────────
# Start one relay on a reserved port: `start <name> [extra relay args...]`.
# Sets PID and PORT for the caller.
start() {
    local name="$1"
    shift
    harness_port "$name"
    PORT="$HARNESS_PORT"
    local url="http://127.0.0.1:$PORT"
    # The reservation covers other harness runs, not the rest of the machine.
    if harness_probe "$url/certificate.sha256"; then
        echo "error: something is already listening on 127.0.0.1:$PORT (stale relay?)" >&2
        exit 1
    fi

    echo "starting relay $name on 127.0.0.1:$PORT..."
    harness_spawn "relay-$name" "$HARNESS_RUN/relay-$name.log" "$RELAY" "$DRAIN_DIR/relay.toml" \
        --listen "127.0.0.1:$PORT" --web-http-listen "127.0.0.1:$PORT" "$@"
    PID="$HARNESS_PID"
    if ! harness_ready "$url/certificate.sha256" 30 "$PID"; then
        echo "relay $name never became ready" >&2
        sed 's/^/  relay: /' "$HARNESS_RUN/relay-$name.log" >&2 || true
        exit 1
    fi
    harness_endpoint "relay-$name" "$url"
}

# B survives and holds the publisher; A drains and serves the track by pulling it
# from B, the way a sibling in a fleet does.
start b
B_PORT="$PORT"
start a --cluster-connect "http://127.0.0.1:$B_PORT/"
A_PORT="$PORT"
A_PID="$PID"

harness_port proxy
PROXY_PORT="$HARNESS_PORT"
harness_endpoint name "http://127.0.0.1:$PROXY_PORT"

# ── run ─────────────────────────────────────────────────────────────────────
# Spawned rather than run in the foreground so a SIGTERM lands while the shell is
# in `wait`, where a trap can run.
status=0
harness_spawn driver - bun "$DRAIN_DIR/drain.ts" \
    --a-port "$A_PORT" --b-port "$B_PORT" --proxy-port "$PROXY_PORT" --a-pid "$A_PID" --timeout "$TIMEOUT"
harness_wait "$HARNESS_PID" || status=$?

# The driver saw the viewer leave A; A itself has to notice every session is gone
# and exit cleanly, well before its 20s deadline would have forced anyone out.
if [[ $status -eq 0 ]]; then
    deadline=$((SECONDS + 10))
    while ! harness_exited "$A_PID" && ((SECONDS < deadline)); do
        sleep 0.1
    done
    if ! harness_exited "$A_PID"; then
        echo "error: relay A was still running 10s after its last session left" >&2
        status=1
    elif ! harness_wait "$A_PID"; then
        echo "error: relay A exited with a failure" >&2
        status=1
    elif ! grep -q "drain complete: every session left" "$HARNESS_RUN/relay-a.log"; then
        echo "error: relay A did not report that every session left on its own" >&2
        status=1
    else
        echo "relay A drained: every session left"
    fi
fi

if [[ $status -ne 0 ]]; then
    for name in a b; do
        echo "── relay $name log ──" >&2
        sed 's/^/  /' "$HARNESS_RUN/relay-$name.log" >&2 || true
    done
fi

exit $status
