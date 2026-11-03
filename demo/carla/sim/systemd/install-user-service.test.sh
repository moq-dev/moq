#!/usr/bin/env bash
# Offline regression for the rootless CARLA systemd installer.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin"
cat >"$tmp/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"${CARLA_SYSTEMCTL_LOG:?}"
EOF
cat >"$tmp/bin/nix" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$tmp/bin/systemctl" "$tmp/bin/nix"
nix_bin="$tmp/bin/nix"

space_repo="$tmp/repo space"
git init -q "$space_repo"
cp -R "$here" "$space_repo/systemd"
if CARLA_SYSTEMD_USER_DIR="$tmp/space-units" \
    CARLA_CONFIG_DIR="$tmp/space-config" \
    PATH="$tmp/bin:$PATH" \
    SYSTEMCTL="$tmp/bin/systemctl" \
    bash "$space_repo/systemd/install-user-service.sh" >"$tmp/space.out" 2>&1; then
    echo "installer accepted a repository path that breaks unit syntax" >&2
    exit 1
fi
grep -Fq 'unsupported repository path' "$tmp/space.out"

CARLA_SYSTEMD_USER_DIR="$tmp/units" \
    CARLA_CONFIG_DIR="$tmp/config" \
    CARLA_SYSTEMCTL_LOG="$tmp/systemctl.log" \
    PATH="$tmp/bin:$PATH" \
    SYSTEMCTL="$tmp/bin/systemctl" \
    bash "$here/install-user-service.sh" >"$tmp/install.out"

# The printed settings are what an operator copies: a scoped token for the
# hosted relay, the locally built image, and the tokenless local relay.
grep -Fxq '  MOQ_URL=https://cdn.moq.dev/demo?jwt=<token>' "$tmp/install.out"
grep -Fxq '  CARLA_IMAGE=localhost/moq-carla:<tag printed by the moq-dev/carla build>' "$tmp/install.out"
grep -Fq 'scoped to publish carla/** and subscribe' "$tmp/install.out"
grep -Fq 'viewer/carla/**. For local development, MOQ_URL=http://localhost:4443 needs' "$tmp/install.out"
test "$(stat -c %a "$tmp/units")" = 700
test "$(stat -c %a "$tmp/config")" = 700
test "$(stat -c %a "$tmp/units/carla-sim.service")" = 600
test "$(stat -c %a "$tmp/units/carla-worker.service")" = 600
grep -Fq "Environment=CARLA_REPO=$repo" "$tmp/units/carla-sim.service"
grep -Fq "Environment=CARLA_REPO=$repo" "$tmp/units/carla-worker.service"
grep -Fxq "ExecStart=$nix_bin develop --accept-flake-config \${CARLA_REPO} --command just --justfile \${CARLA_REPO}/justfile carla server" "$tmp/units/carla-sim.service"
grep -Fxq "ExecStart=$nix_bin develop --accept-flake-config \${CARLA_REPO} --command just --justfile \${CARLA_REPO}/justfile carla worker" "$tmp/units/carla-worker.service"
# The worker must not race the simulator's RPC port on a cold boot.
grep -Fxq "ExecStartPre=$nix_bin develop --accept-flake-config \${CARLA_REPO} --command just --justfile \${CARLA_REPO}/justfile carla wait-port 127.0.0.1 2000 600" "$tmp/units/carla-worker.service"
# A simulator waiting for its worker renders frames nobody asked for, so every
# start parks it. Both steps are optional: neither is a verdict on the server.
grep -Fxq "ExecStartPost=-$nix_bin develop --accept-flake-config \${CARLA_REPO} --command just --justfile \${CARLA_REPO}/justfile carla wait-port 127.0.0.1 2000 600" "$tmp/units/carla-sim.service"
grep -Fxq "ExecStartPost=-$nix_bin develop --accept-flake-config \${CARLA_REPO} --command just --justfile \${CARLA_REPO}/justfile carla park" "$tmp/units/carla-sim.service"
# The CARLA_IMAGE pin lives in the shared env file, so reinstalling the units
# cannot silently return the simulator to the stock image.
grep -Fq 'EnvironmentFile=-%E/moq/carla.env' "$tmp/units/carla-sim.service"
if grep -E '^ExecStartPost=[^-]' "$tmp/units/carla-sim.service"; then
    echo "a parking step can fail the simulator unit into a restart loop" >&2
    exit 1
fi
if grep -Fq '@CARLA_REPO@' "$tmp/units/carla-sim.service" ||
    grep -Fq '@NIX_BIN@' "$tmp/units/carla-sim.service" ||
    grep -Fq '@CARLA_REPO@' "$tmp/units/carla-worker.service" ||
    grep -Fq '@NIX_BIN@' "$tmp/units/carla-worker.service"; then
    echo "installed unit retained a placeholder" >&2
    exit 1
fi
grep -Fxq -- '--user daemon-reload' "$tmp/systemctl.log"

if command -v systemd-analyze >/dev/null; then
    systemd-analyze verify \
        "$tmp/units/carla-sim.service" "$tmp/units/carla-worker.service"
fi

printf 'CARLA user service installer test: ok\n'
