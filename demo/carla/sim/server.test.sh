#!/usr/bin/env bash
# Offline regression for the CARLA container lifecycle contract.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin" "$tmp/config/cdi"
cat >"$tmp/bin/podman" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >"${CARLA_TEST_PODMAN_ARGS:?}"
EOF
chmod +x "$tmp/bin/podman"

: >"$tmp/config/cdi/nvidia.yaml"
if PATH="$tmp/bin:$PATH" CARLA_CDI_SPEC="$tmp/config/cdi/nvidia.yaml" bash "$here/server.sh" >"$tmp/empty.out" 2>&1; then
    echo "empty CDI spec unexpectedly accepted" >&2
    exit 1
fi
grep -Fq 'just carla cdi' "$tmp/empty.out"

touch "$tmp/device"
# The fixtures mirror nvidia-ctk's layout: entries are YAML list items, the
# hook's path is a bare key under `- hookName:`.
printf -- '- hostPath: %s\n  path: %s\n' "$tmp/device" "$tmp/missing/nvidia-ctk" >"$tmp/config/cdi/nvidia.yaml"
if PATH="$tmp/bin:$PATH" CARLA_CDI_SPEC="$tmp/config/cdi/nvidia.yaml" bash "$here/server.sh" >"$tmp/stale.out" 2>&1; then
    echo "stale CDI hook unexpectedly accepted" >&2
    exit 1
fi
grep -Fq 'just carla cdi' "$tmp/stale.out"

touch "$tmp/bin/nvidia-ctk"
chmod +x "$tmp/bin/nvidia-ctk"
printf -- '- hostPath: %s\n  path: %s\n' "$tmp/device" "$tmp/bin/nvidia-ctk" >"$tmp/config/cdi/nvidia.yaml"
PATH="$tmp/bin:$PATH" \
    CARLA_CDI_SPEC="$tmp/config/cdi/nvidia.yaml" \
    XDG_RUNTIME_DIR="$tmp/run" \
    CARLA_TEST_PODMAN_ARGS="$tmp/podman.args" \
    bash "$here/server.sh"

# NixOS's declarative CDI generator emits JSON instead of nvidia-ctk's default
# YAML. It receives the same stale-path validation before the launcher uses it.
cat >"$tmp/config/cdi/nvidia.yaml" <<EOF
{
  "containerEdits": {
    "mounts": [
      {
        "hostPath": "$tmp/device"
      }
    ],
    "hooks": [
      {
        "path": "$tmp/bin/nvidia-ctk"
      }
    ]
  }
}
EOF
PATH="$tmp/bin:$PATH" \
    CARLA_CDI_SPEC="$tmp/config/cdi/nvidia.yaml" \
    XDG_RUNTIME_DIR="$tmp/run" \
    CARLA_TEST_PODMAN_ARGS="$tmp/podman.args" \
    bash "$here/server.sh"

grep -Fxq -- '--rm' "$tmp/podman.args"
grep -Fxq -- '--replace' "$tmp/podman.args"
grep -Fxq -- '--device' "$tmp/podman.args"
grep -Fxq -- 'nvidia.com/gpu=all' "$tmp/podman.args"
grep -Fxq -- 'docker.io/carlasim/carla:0.9.16' "$tmp/podman.args"
# The launcher owns both the executable and quality. An image entrypoint must
# not silently append experimental flags or lower the graphics preset.
grep -Fxq -- '--entrypoint' "$tmp/podman.args"
grep -Fxq -- '-quality-level=Epic' "$tmp/podman.args"
# The engine must be container PID 1 so SIGTERM reaches it; the image's
# CarlaUE4.sh wrapper would swallow it.
grep -Fxq -- '/workspace/CarlaUE4/Binaries/Linux/CarlaUE4-Linux-Shipping' "$tmp/podman.args"
if grep -Fq 'CarlaUE4.sh' "$tmp/podman.args"; then
    echo "server.sh runs the engine through the image's wrapper script" >&2
    exit 1
fi

# The bridge listens in a directory the worker shares, as the same user, at the
# path the worker defaults to.
grep -Fxq -- '--userns=keep-id' "$tmp/podman.args"
grep -Fxq -- "$tmp/run/carla-bridge:$tmp/run/carla-bridge" "$tmp/podman.args"
grep -Fxq -- "-moq-bridge-socket=$tmp/run/carla-bridge/bridge.sock" "$tmp/podman.args"
[[ "$(stat -c %a "$tmp/run/carla-bridge")" == 700 ]]

printf 'CARLA server lifecycle test: ok\n'
