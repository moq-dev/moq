#!/usr/bin/env bash
# Pinned upstream conformance under the relay profile, subgroup zero, one relay.
set -euo pipefail

CONFORMANCE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=$(cd "$CONFORMANCE_DIR/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../lib/harness.sh
source "$CONFORMANCE_DIR/../lib/harness.sh"

system=$(nix eval --impure --raw --expr builtins.currentSystem)
package=$(nix build --cores 4 --no-link --print-out-paths "$WORKSPACE#legacyPackages.$system.moxygen")
python3 "$CONFORMANCE_DIR/selection.test.py" "$package/share/moxygen/conformance_test.sh"
cargo build --locked -p moq-relay
relay="$WORKSPACE/${CARGO_TARGET_DIR:-target}/debug/moq-relay"
# Cargo accepts an absolute target directory too.
[[ "${CARGO_TARGET_DIR:-}" != /* ]] || relay="$CARGO_TARGET_DIR/debug/moq-relay"

harness_begin ietf-conformance "$(harness_env MOQ_TEST_RESULTS)just test ietf-conformance"
python3 "$CONFORMANCE_DIR/prepare.py" "$package/share/moxygen/conformance_test.sh" "$HARNESS_RUN/conformance.sh"
# LibreSSL in the dev shell has no system-wide openssl.cnf.
cat >"$HARNESS_RUN/openssl.cnf" <<'EOF'
[req]
prompt = no
distinguished_name = dn
x509_extensions = extensions
[dn]
CN = localhost
[extensions]
subjectAltName = DNS:localhost,IP:127.0.0.1
EOF
openssl req -config "$HARNESS_RUN/openssl.cnf" -x509 -newkey rsa:2048 -nodes \
    -keyout "$HARNESS_RUN/key.pem" -out "$HARNESS_RUN/cert.pem" -days 1 \
    >"$HARNESS_RUN/certificate.log" 2>&1

start_relay() {
    local label="$1" version="$2"
    harness_port "$label"
    local port="$HARNESS_PORT" config="$HARNESS_RUN/$label.toml"
    cat >"$config" <<EOF
[log]
level = "info"
[listen]
bind = "127.0.0.1:$port"
version = ["moq-transport-$version"]
tls.cert = ["$HARNESS_RUN/cert.pem"]
tls.key = ["$HARNESS_RUN/key.pem"]
[web.http]
listen = "127.0.0.1:$port"
[auth]
public = "**"
EOF
    harness_spawn "$label" "$HARNESS_RUN/$label.log" "$relay" "$config"
    harness_ready "http://127.0.0.1:$port/certificate.sha256" 30 "$HARNESS_PID"
    harness_endpoint "$label" "http://127.0.0.1:$port"
}

# Wait for routing state, not an elapsed startup delay. A reconnect is a failure,
# even if the upstream server would recover before the next conformance case.
publisher_healthy() {
    ! harness_exited "$publisher_pid" &&
        ! grep -Eq 'Relay (connect failed|setup failed|session ended)|Failed to establish relay session' "$HARNESS_RUN/$label-publisher.log"
}

namespace_ready() {
    local deadline=$((SECONDS + 30))
    while ((SECONDS < deadline)); do
        publisher_healthy || return 1
        if curl -sf --max-time 1 "http://127.0.0.1:$relay_port/announced" | grep -qx 'moq-test-00'; then
            return 0
        fi
        sleep 0.05
    done
    echo "publisher namespace was not advertised at the subscriber relay" >&2
    return 1
}

conformance_client() {
    cd "$HARNESS_RUN"
    SECTIONS="1 2 3 4 5 6 7" SKIP_FETCH=1 MOXYGEN_DIR="$package/libexec" \
        bash ./conformance.sh "$@" Q
}

run_suite() {
    local name="$1" url="$2" status=0
    harness_endpoint "$name-subscriber" "$url"
    echo "=== $name / draft $version / sections 1-7 / subgroup 0 ==="
    # Upstream owns each case's deadline. No retry or replacement verdict.
    harness_spawn "$name-client" "$HARNESS_RUN/$name-client.log" \
        conformance_client "$url" "$version"
    harness_wait "$HARNESS_PID" || status=$?
    cat "$HARNESS_RUN/$name-client.log"
    if ((status != 0)) || ! publisher_healthy; then
        harness_result fail suite=ietf-conformance case="$name" version="moq-transport-$version" transport=quic
        failed=1
    else
        harness_result pass suite=ietf-conformance case="$name" version="moq-transport-$version" transport=quic
    fi
}

# The pinned MoQVersions.h supports drafts 16 and 18; this relay supports both.
failed=0
# A two-relay cell waits on a test publisher that answers TRACK_STATUS; see the
# passthrough quest.
for version in 16 18; do
    label="single-$version"
    start_relay "$label-origin" "$version"
    relay_port="$HARNESS_PORT"
    relay_pid="$HARNESS_PID"
    harness_port "$label-publisher"
    publisher_port="$HARNESS_PORT"
    harness_spawn "$label-publisher" "$HARNESS_RUN/$label-publisher.log" \
        "$package/bin/moqtest_server" --port="$HARNESS_PORT" \
        --cert="$HARNESS_RUN/cert.pem" --key="$HARNESS_RUN/key.pem" \
        --relay_url="https://127.0.0.1:$relay_port/" --versions="$version" --transport=quic --logging=INFO
    publisher_pid="$HARNESS_PID"
    namespace_ready
    # A direct-origin control distinguishes suite/pin failures from relay behavior.
    run_suite "reference-$version" "https://127.0.0.1:$publisher_port/test"
    run_suite "$label" "https://127.0.0.1:$relay_port/"
    harness_reap "$publisher_pid"
    harness_reap "$relay_pid"
done
if ((failed != 0)); then
    echo "Moxygen conformance failed; see each cell's client and endpoint logs" >&2
    exit 1
fi
echo "Moxygen conformance: all shared drafts passed through the relay"
