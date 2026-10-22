#!/usr/bin/env bash
# Protocol-level interoperability smoke test using moxygen's published client.
#
# This is separate from smoke.sh's Hang media matrix: moxygen uses MoQ Media
# Interop packaging, while this client exercises transport setup, namespace
# publication, subscription errors, and publisher/subscriber routing directly.
set -euo pipefail

MOQ_REPO="https://github.com/moq-dev/moq"
MOQ_RELAY="${MOQ_RELAY_BIN:-}"
DOCKER="${MOXYGEN_DOCKER:-docker}"
MOXYGEN_IMAGE="${MOXYGEN_IMAGE:-ghcr.io/facebookexperimental/moxygen-interop-client:latest-amd64}"
PORT="${MOXYGEN_SMOKE_PORT:-4444}"
TIMEOUT="${MOXYGEN_SMOKE_TIMEOUT:-45}"
VERSION="${MOXYGEN_MOQT_VERSION:-16}"
HTTPS_URL="https://127.0.0.1:${PORT}"
MOQT_URL="moqt://127.0.0.1:${PORT}"
CONTAINER_PREFIX="moxygen-smoke-$$"

TMP=$(mktemp -d)
RELAY_PID=""

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

# shellcheck disable=SC2329  # invoked indirectly via 'trap cleanup EXIT'
kill_tree() {
    local pid="$1" child
    for child in $(pgrep -P "$pid" 2>/dev/null || true); do kill_tree "$child"; done
    kill -KILL "$pid" 2>/dev/null || true
}

# shellcheck disable=SC2329  # invoked indirectly via 'trap cleanup EXIT'
cleanup() {
    "$DOCKER" rm -f "${CONTAINER_PREFIX}-webtransport" "${CONTAINER_PREFIX}-quic" >/dev/null 2>&1 || true
    [[ -n "$RELAY_PID" ]] && kill_tree "$RELAY_PID"
    rm -rf "$TMP"
}
trap cleanup EXIT

missing=()
for tool in curl openssl pgrep timeout "$DOCKER"; do
    have "$tool" || missing+=("$tool")
done
if [[ -z "$MOQ_RELAY" ]]; then
    have cargo || missing+=("cargo")
    have git || missing+=("git")
fi
if [[ ${#missing[@]} -gt 0 ]]; then
    echo "error: missing required tools: ${missing[*]}" >&2
    exit 1
fi
if ! "$DOCKER" info >"$TMP/docker-info.log" 2>&1; then
    echo "error: $DOCKER daemon is not running" >&2
    sed 's/^/  /' "$TMP/docker-info.log" >&2 || true
    exit 1
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

[[ -x "$MOQ_RELAY" ]] || {
    echo "error: moq-dev relay is not executable: $MOQ_RELAY" >&2
    exit 1
}

echo "pulling moxygen interop client: $MOXYGEN_IMAGE"
if ! "$DOCKER" pull --platform linux/amd64 "$MOXYGEN_IMAGE" >"$TMP/docker-pull.log" 2>&1; then
    echo "error: failed to pull moxygen interop client" >&2
    sed 's/^/  /' "$TMP/docker-pull.log" >&2 || true
    exit 1
fi

# The relay requires certificate files. The moxygen client disables verification
# only for this loopback test.
if ! openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TMP/localhost.key" -out "$TMP/localhost.crt" -days 1 \
    -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1 \
    >"$TMP/openssl.log" 2>&1; then
    echo "error: failed to generate the temporary relay certificate" >&2
    sed 's/^/  /' "$TMP/openssl.log" >&2 || true
    exit 1
fi

echo "starting moq-dev relay on 127.0.0.1:${PORT}..."
# Git HEAD relay: the >= 0.15 flag names and pattern-based public auth.
"$MOQ_RELAY" --listen "127.0.0.1:${PORT}" \
    --listen-tls-cert "$TMP/localhost.crt" --listen-tls-key "$TMP/localhost.key" \
    --web-https-listen "127.0.0.1:${PORT}" \
    --web-https-cert "$TMP/localhost.crt" --web-https-key "$TMP/localhost.key" \
    --auth-public "**" >"$TMP/relay.log" 2>&1 &
RELAY_PID=$!

ready=0
for _ in $(seq 1 60); do
    if curl --cacert "$TMP/localhost.crt" -sf "$HTTPS_URL/certificate.sha256" >/dev/null 2>&1; then
        ready=1
        break
    fi
    kill -0 "$RELAY_PID" 2>/dev/null || break
    sleep 0.5
done
if [[ "$ready" -ne 1 ]]; then
    echo "error: moq-dev relay never became ready" >&2
    sed 's/^/  relay: /' "$TMP/relay.log" >&2 || true
    exit 1
fi

overall=0
run_transport() {
    local transport="$1" url="$2" name
    name="${CONTAINER_PREFIX}-${transport}"
    echo "=== moxygen / moq-dev / $transport / draft-$VERSION ==="
    if timeout -k 3 "$TIMEOUT" "$DOCKER" run --rm \
        --name "$name" --network host --platform linux/amd64 \
        "$MOXYGEN_IMAGE" --relay "$url" --tls_disable_verify \
        --versions "$VERSION" >"$TMP/$transport.tap" 2>&1; then
        sed 's/^/  /' "$TMP/$transport.tap"
    else
        echo "  FAIL  moxygen interop client" >&2
        sed 's/^/        client: /' "$TMP/$transport.tap" >&2 || true
        sed 's/^/        relay: /' "$TMP/relay.log" >&2 || true
        overall=1
    fi
    "$DOCKER" rm -f "$name" >/dev/null 2>&1 || true
}

# moxygen recommends draft-16 for new integrations while its draft-18 support
# remains experimental. Pinning the wire draft is intentional; both source-head
# implementations are still refreshed on every run.
run_transport webtransport "$HTTPS_URL"
run_transport quic "$MOQT_URL"

if [[ "$overall" -eq 0 ]]; then
    echo "moxygen interop smoke: all checks passed"
else
    echo "moxygen interop smoke: FAILURES detected" >&2
fi
exit "$overall"
