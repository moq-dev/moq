#!/usr/bin/env bash
# Generate a rootless Podman CDI spec whose hook survives Nix garbage collection.
set -euo pipefail

state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/moq"
cdi_dir="${XDG_CONFIG_HOME:-$HOME/.config}/cdi"
toolkit="$state_dir/nvidia-container-toolkit"
hook="$toolkit/bin/nvidia-ctk"

mkdir -p "$state_dir" "$cdi_dir"

# The CDI spec embeds the hook's absolute path. A bare `nix shell` leaves that
# path unrooted, so a later Nix GC breaks every container start. The out-link is
# both the stable path written into the spec and the GC root that keeps it alive.
nix build --out-link "$toolkit" nixpkgs#nvidia-container-toolkit

tmp="$(mktemp "$cdi_dir/nvidia.XXXXXX.yaml")"
trap 'rm -f "$tmp"' EXIT
"$hook" cdi generate \
    --mode=nvml \
    --nvidia-cdi-hook-path="$hook" \
    --output="$tmp"
mv "$tmp" "$cdi_dir/nvidia.yaml"
trap - EXIT

printf 'carla: wrote %s with rooted hook %s\n' "$cdi_dir/nvidia.yaml" "$hook" >&2
