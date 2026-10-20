#!/usr/bin/env bash
# End-to-end smoke test using the current cloudflare/moq-rs default branch.
#
# This is intentionally separate from smoke.sh's public-package media matrix:
# cloudflare/moq-rs uses a different application catalog/track layout. We build
# a purpose-built Cloudflare client and both projects' relays from their latest
# Git source. The client validates every byte of sustained subgroup payloads
# through both relays; Cloudflare's relay also exercises datagrams.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CLOUDFLARE_CLIENT="$SMOKE_DIR/clients/cloudflare"
CLOUDFLARE_MOQ_REPO="https://github.com/cloudflare/moq-rs"
MOQ_REPO="https://github.com/moq-dev/moq"
PORT="${CLOUDFLARE_SMOKE_PORT:-4443}"
TIMEOUT="${CLOUDFLARE_SMOKE_TIMEOUT:-20}"
HTTPS_URL="https://127.0.0.1:${PORT}"
MOQT_URL="moqt://127.0.0.1:${PORT}"

TMP=$(mktemp -d)
RELAY_PID=""
PUB_PID=""
CLOUDFLARE_RELAY="${CLOUDFLARE_RELAY_BIN:-}"
MOQ_RELAY="${MOQ_RELAY_BIN:-}"
CLIENT="${CLOUDFLARE_CLIENT_BIN:-}"

have() { command -v "$1" >/dev/null 2>&1; }

# build_git <repo> <package> <dir>: build one package from a repo's default
# branch with its committed Cargo.lock, into <dir>/target/release. A plain
# `cargo build` in a checkout rather than `cargo install --git`: a compile cache
# (mbx in CI) reuses unchanged crates across builds, but not across installs.
build_git() {
    local repo="$1" package="$2" dir="$3"
    git clone --quiet --depth 1 "$repo" "$dir" &&
        cargo build --quiet --release --locked --manifest-path "$dir/Cargo.toml" -p "$package" \
            --target-dir "$dir/target"
}

kill_tree() {
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2>/dev/null || true); do kill_tree "$child"; done
    kill -KILL "$pid" 2>/dev/null || true
}

# shellcheck disable=SC2329  # invoked indirectly via 'trap cleanup EXIT'
cleanup() {
    [[ -n "$PUB_PID" ]] && kill_tree "$PUB_PID"
    [[ -n "$RELAY_PID" ]] && kill_tree "$RELAY_PID"
    rm -rf "$TMP"
}
trap cleanup EXIT

missing=()
for tool in curl openssl pgrep timeout; do
    have "$tool" || missing+=("$tool")
done
if [[ -z "$CLOUDFLARE_RELAY" || -z "$MOQ_RELAY" || -z "$CLIENT" ]]; then
    have cargo || missing+=("cargo")
    have git || missing+=("git")
fi
if [[ ${#missing[@]} -gt 0 ]]; then
    echo "error: missing required tools: ${missing[*]}" >&2
    exit 1
fi

if [[ -z "$CLOUDFLARE_RELAY" ]]; then
    echo "building moq-relay-ietf from $CLOUDFLARE_MOQ_REPO (latest default branch)..."
    # Follow Git HEAD, but honor that checkout's committed Cargo.lock. Without
    # --locked Cargo currently selects a newer hyper-util that does not compile
    # with moq-rs's hyper-serve dependency.
    if build_git "$CLOUDFLARE_MOQ_REPO" moq-relay-ietf "$TMP/cloudflare-moq" >"$TMP/relay-build.log" 2>&1; then
        CLOUDFLARE_RELAY="$TMP/cloudflare-moq/target/release/moq-relay-ietf"
    else
        echo "error: failed to build moq-relay-ietf" >&2
        sed 's/^/  /' "$TMP/relay-build.log" >&2 || true
        exit 1
    fi
fi

if [[ -z "$MOQ_RELAY" ]]; then
    echo "building moq-relay from $MOQ_REPO (latest default branch)..."
    if build_git "$MOQ_REPO" moq-relay "$TMP/moq" >"$TMP/moq-relay-build.log" 2>&1; then
        MOQ_RELAY="$TMP/moq/target/release/moq-relay"
    else
        echo "error: failed to build moq-relay" >&2
        sed 's/^/  /' "$TMP/moq-relay-build.log" >&2 || true
        exit 1
    fi
fi

if [[ -z "$CLIENT" ]]; then
    echo "building cloudflare smoke client against $CLOUDFLARE_MOQ_REPO..."
    mkdir -p "$TMP/client"
    cp "$CLOUDFLARE_CLIENT/Cargo.toml" "$TMP/client/Cargo.toml"
    cp -R "$CLOUDFLARE_CLIENT/src" "$TMP/client/src"
    if cargo build --quiet --release --manifest-path "$TMP/client/Cargo.toml" \
        --target-dir "$TMP/target" >"$TMP/client-build.log" 2>&1; then
        CLIENT="$TMP/target/release/cloudflare-smoke"
    else
        echo "error: failed to build cloudflare smoke client" >&2
        sed 's/^/  /' "$TMP/client-build.log" >&2 || true
        exit 1
    fi
fi

[[ -x "$CLOUDFLARE_RELAY" ]] || {
    echo "error: Cloudflare relay is not executable: $CLOUDFLARE_RELAY" >&2
    exit 1
}
[[ -x "$MOQ_RELAY" ]] || {
    echo "error: moq-dev relay is not executable: $MOQ_RELAY" >&2
    exit 1
}
[[ -x "$CLIENT" ]] || {
    echo "error: client is not executable: $CLIENT" >&2
    exit 1
}

echo "Cloudflare relay: $CLOUDFLARE_RELAY"
echo "moq-dev relay:    $MOQ_RELAY"
echo "client:           $CLIENT"

# The upstream relay requires certificate files. Generate a throwaway localhost
# certificate; clients explicitly disable verification only for this loopback test.
if ! openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TMP/localhost.key" -out "$TMP/localhost.crt" -days 1 \
    -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1 \
    >"$TMP/openssl.log" 2>&1; then
    echo "error: failed to generate the temporary relay certificate" >&2
    sed 's/^/  /' "$TMP/openssl.log" >&2 || true
    exit 1
fi

start_relay() {
    local implementation="$1" ready
    echo "starting $implementation relay on 127.0.0.1:${PORT}..."
    case "$implementation" in
        cloudflare)
            "$CLOUDFLARE_RELAY" --bind "127.0.0.1:${PORT}" \
                --tls-cert "$TMP/localhost.crt" --tls-key "$TMP/localhost.key" \
                --tls-root "$TMP/localhost.crt" --coordinator-file "$TMP/coordinator.json" --dev \
                >"$TMP/relay-cloudflare.log" 2>&1 &
            ready="$HTTPS_URL/fingerprint"
            ;;
        moq-dev)
            # Git HEAD relay: the >= 0.15 flag names and pattern-based public auth.
            "$MOQ_RELAY" --listen "127.0.0.1:${PORT}" \
                --listen-tls-cert "$TMP/localhost.crt" --listen-tls-key "$TMP/localhost.key" \
                --web-https-listen "127.0.0.1:${PORT}" \
                --web-https-cert "$TMP/localhost.crt" --web-https-key "$TMP/localhost.key" \
                --auth-public "**" >"$TMP/relay-moq-dev.log" 2>&1 &
            ready="$HTTPS_URL/certificate.sha256"
            ;;
        *)
            echo "error: unknown relay implementation: $implementation" >&2
            exit 1
            ;;
    esac
    RELAY_PID=$!

    for _ in $(seq 1 60); do
        curl --cacert "$TMP/localhost.crt" -sf "$ready" >/dev/null 2>&1 && return
        kill -0 "$RELAY_PID" 2>/dev/null || break
        sleep 0.5
    done

    echo "error: $implementation relay never became ready" >&2
    sed 's/^/  relay: /' "$TMP/relay-$implementation.log" >&2 || true
    exit 1
}

stop_relay() {
    kill_tree "$RELAY_PID"
    wait "$RELAY_PID" 2>/dev/null || true
    RELAY_PID=""
}

overall=0
run_round() {
    local implementation="$1" transport="$2" url="$3" delivery="$4"
    local object_size="$5" objects="$6" interval_ms="$7"
    local namespace="smoke-${implementation}-${transport}-${delivery}-$$-${RANDOM}"
    local name="$implementation / $transport / $delivery"

    "$CLIENT" --url "$url" --namespace "$namespace" --tls-disable-verify \
        --tls-root "$TMP/localhost.crt" \
        --delivery "$delivery" --object-size "$object_size" \
        --interval-ms "$interval_ms" --start-delay-ms 4000 publish \
        >"$TMP/pub-${implementation}-${transport}-${delivery}.log" 2>&1 &
    PUB_PID=$!

    # Both relays reject an unknown namespace instead of holding an early
    # subscription. Probe until PUBLISH_NAMESPACE is accepted; exit 75 is the
    # client's specific unknown-namespace signal. A probe can also remain pending
    # while publication is settling, so bound each attempt to two seconds and
    # retry timeout(1)'s exit 124 within the outer deadline. Every semantic error
    # still fails immediately. Payload validation then runs exactly once and
    # cannot be masked.
    local subscribe_deadline=$((SECONDS + 10)) probe_ok=0 probe_status
    while true; do
        if timeout -k 1 2 "$CLIENT" --url "$url" --namespace "$namespace" \
            --tls-disable-verify --tls-root "$TMP/localhost.crt" \
            --delivery "$delivery" --object-size "$object_size" \
            probe >"$TMP/probe-${implementation}-${transport}-${delivery}.log" 2>&1; then
            probe_ok=1
            break
        else
            probe_status=$?
        fi
        [[ "$probe_status" -eq 75 || "$probe_status" -eq 124 ]] || break
        kill -0 "$PUB_PID" 2>/dev/null || break
        ((SECONDS >= subscribe_deadline)) && break
        sleep 0.25
    done

    if [[ "$probe_ok" -ne 1 ]]; then
        echo "  FAIL  $name (namespace publication was not accepted; probe exit $probe_status)"
        sed 's/^/        publisher: /' "$TMP/pub-${implementation}-${transport}-${delivery}.log" >&2 || true
        sed 's/^/        probe: /' "$TMP/probe-${implementation}-${transport}-${delivery}.log" >&2 || true
        sed 's/^/        relay: /' "$TMP/relay-$implementation.log" >&2 || true
        overall=1
    elif timeout -k 3 "$TIMEOUT" "$CLIENT" --url "$url" --namespace "$namespace" \
        --tls-disable-verify --tls-root "$TMP/localhost.crt" \
        --delivery "$delivery" --object-size "$object_size" \
        subscribe --objects "$objects" >"$TMP/sub-${implementation}-${transport}-${delivery}.log" 2>&1; then
        echo "  PASS  $name"
        sed 's/^/        /' "$TMP/sub-${implementation}-${transport}-${delivery}.log"
    else
        echo "  FAIL  $name"
        sed 's/^/        publisher: /' "$TMP/pub-${implementation}-${transport}-${delivery}.log" >&2 || true
        sed 's/^/        subscriber: /' "$TMP/sub-${implementation}-${transport}-${delivery}.log" >&2 || true
        sed 's/^/        relay: /' "$TMP/relay-$implementation.log" >&2 || true
        overall=1
    fi

    kill_tree "$PUB_PID"
    wait "$PUB_PID" 2>/dev/null || true
    PUB_PID=""
}

echo "=== cloudflare/moq-rs relay payload integrity ==="
start_relay cloudflare
for transport in webtransport quic; do
    case "$transport" in
        webtransport) url="$HTTPS_URL" ;;
        quic) url="$MOQT_URL" ;;
    esac
    # Large subgroup objects force multi-packet/stream transfer. Datagrams stay
    # below a typical path MTU and are validated independently because loss and
    # reordering are legal for that delivery mode.
    run_round cloudflare "$transport" "$url" subgroup 65536 32 5
    run_round cloudflare "$transport" "$url" datagram 900 32 5
done
stop_relay

echo "=== moq-dev/moq relay interoperability ==="
start_relay moq-dev
for transport in webtransport quic; do
    case "$transport" in
        webtransport) url="$HTTPS_URL" ;;
        quic) url="$MOQT_URL" ;;
    esac
    # moq-dev's IETF adapter currently forwards subgroup streams, not datagrams.
    # A moderate cadence avoids conflating lossy live-edge turnover with protocol
    # interoperability while still validating sustained multi-group delivery.
    run_round moq-dev "$transport" "$url" subgroup 65536 32 25
done
stop_relay

if [[ "$overall" -eq 0 ]]; then
    echo "cloudflare interop smoke: all checks passed"
else
    echo "cloudflare interop smoke: FAILURES detected" >&2
fi
exit "$overall"
