#!/usr/bin/env bash
# Install the CARLA simulator and worker as rootless systemd user services.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
unit_dir="${CARLA_SYSTEMD_USER_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}"
env_dir="${CARLA_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/moq}"
systemctl_bin="${SYSTEMCTL:-systemctl}"

if [[ ! "$repo" =~ ^/[A-Za-z0-9._/-]+$ ]]; then
    echo "service install: unsupported repository path: $repo" >&2
    exit 1
fi
nix_bin="$(command -v nix)" || {
    echo "service install: nix is not available in PATH" >&2
    exit 1
}
if [[ ! "$nix_bin" =~ ^/[A-Za-z0-9._/-]+$ ]]; then
    echo "service install: unsupported nix path: $nix_bin" >&2
    exit 1
fi

install -d -m 0700 "$unit_dir" "$env_dir"

for name in carla-sim.service carla-worker.service; do
    template="$(<"$here/$name")"
    template="${template//@CARLA_REPO@/$repo}"
    template="${template//@NIX_BIN@/$nix_bin}"
    install -m 0600 /dev/null "$unit_dir/$name"
    printf '%s\n' "$template" >"$unit_dir/$name"
done

"$systemctl_bin" --user daemon-reload

cat <<EOF
CARLA user services installed from:
  $repo

Before starting, write the relay settings to:
  $env_dir/carla.env

  MOQ_URL=https://cdn.moq.dev/demo?jwt=<token>
  CARLA_IMAGE=localhost/moq-carla:<tag printed by the moq-dev/carla build>

The token is a demo-project JWT scoped to publish carla/** and subscribe
viewer/carla/**. For local development, MOQ_URL=http://localhost:4443 needs
no token.

Then run:
  just carla service-enable
EOF
