#!/usr/bin/env bash
# Stream credit by priority, end to end: a Chromium or Firefox tab reads 300
# live tracks through a Rust relay over WebTransport, past the browser's
# ~100-stream cap.
# See README.md.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../../sh/local.sh" bash "$0" "$@"
fi

DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$DIR/../.." && pwd)

# Run directory, reserved ports, and process-group ownership. See test/README.md.
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$DIR/../lib/harness.sh"

RERUN="$(harness_env STREAM_CREDIT_PROFILE RELAY_BIN)just test stream-credit$(harness_argv "$@")"

TIMEOUT=60
PROFILE="${STREAM_CREDIT_PROFILE:-debug}"
RELAY="${RELAY_BIN:-}"
VERSIONS=(moq-lite-07-wip moq-lite-06)
ENGINES=(chromium firefox)

while [[ $# -gt 0 ]]; do
    case "$1" in
        --timeout | --version | --engine)
            if [[ $# -lt 2 || -z "${2:-}" || "$2" == -* ]]; then
                echo "error: $1 requires a value" >&2
                exit 2
            fi
            case "$1" in
                --timeout) TIMEOUT="$2" ;;
                --version) VERSIONS=("$2") ;;
                --engine) ENGINES=("$2") ;;
            esac
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

harness_begin stream-credit "$RERUN"

for tool in cargo bun curl; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: $tool not found; run inside 'nix develop'" >&2
        exit 1
    }
done

# ── build ───────────────────────────────────────────────────────────────────
if [[ -z "$RELAY" ]]; then
    echo "building moq-relay ($PROFILE)..."
    flag=()
    [[ "$PROFILE" == "debug" ]] || flag=(--profile "$PROFILE")
    (cd "$WORKSPACE" && cargo build --locked ${flag[@]+"${flag[@]}"} -p moq-relay)
    harness_stage moq-relay "${CARGO_TARGET_DIR:-$WORKSPACE/target}/$PROFILE/moq-relay"
    RELAY="$HARNESS_RUN/moq-relay"
fi

uv run --no-project python "$WORKSPACE/sh/js/install.py" frozen

# ── relay ───────────────────────────────────────────────────────────────────
harness_port relay
PORT="$HARNESS_PORT"
URL="http://127.0.0.1:$PORT"
if harness_probe "$URL/certificate.sha256"; then
    echo "error: something is already listening on 127.0.0.1:$PORT (stale relay?)" >&2
    exit 1
fi
# The publisher's scheduling decisions, which show the cap bound and groups yielded.
RUST_LOG="info,moq_net::lite::publisher=debug" harness_spawn relay "$HARNESS_RUN/relay.log" \
    "$RELAY" "$DIR/relay.toml" --listen "127.0.0.1:$PORT" --web-http-listen "127.0.0.1:$PORT"
RELAY_PID="$HARNESS_PID"
if ! harness_ready "$URL/certificate.sha256" 30 "$RELAY_PID"; then
    echo "relay never became ready" >&2
    sed 's/^/  relay: /' "$HARNESS_RUN/relay.log" >&2 || true
    exit 1
fi
harness_endpoint relay "$URL"

# ── run ─────────────────────────────────────────────────────────────────────
status=0
for version in "${VERSIONS[@]}"; do
    for engine in "${ENGINES[@]}"; do
        echo "── $engine on $version ──"
        started=$SECONDS
        case_status=0
        # Spawned rather than run in the foreground so a SIGTERM lands while the shell
        # is in `wait`, where a trap can run.
        harness_spawn "driver-$engine-$version" - bun "$DIR/driver.ts" --port "$PORT" --version "$version" \
            --engine "$engine" --out "$HARNESS_RUN/$engine-$version" --relay-log "$HARNESS_RUN/relay.log" \
            --timeout "$TIMEOUT"
        harness_wait "$HARNESS_PID" || case_status=$?
        cell=(suite=stream-credit case="stream credit $engine" subscriber=js transport=webtransport version="$version")
        if [[ $case_status -eq 0 ]]; then
            harness_result pass "${cell[@]}" seconds=$((SECONDS - started))
        else
            harness_result fail "${cell[@]}" seconds=$((SECONDS - started)) log="$HARNESS_RUN/relay.log"
            status=1
        fi
    done
done

exit $status
