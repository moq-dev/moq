#!/usr/bin/env bash
#
# Run a command inside the pinned gstreamer-ci image (.github/docker/gstreamer),
# so moq-gst's Linux jobs get Ubuntu's GStreamer without reaching an apt mirror.
# The checkout, $HOME (rustup's toolchain), /nix (the Nix tools) and the CA
# certificates are mounted at the same paths and PATH passes through, so the
# host's tools run against the image's GStreamer. It runs as the caller's user
# so the files it writes stay theirs. Linux only; log in to ghcr.io first if
# the image is private.
#
# Usage:
#   gstreamer.sh <command> [args...]

set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "Usage: $0 <command> [args...]" >&2
    exit 2
fi

root="$PWD"
image="$(<"$root/.github/docker/gstreamer/image")"
just="$(readlink -f "$(command -v just)")"

args=(
    --rm
    --user "$(id -u):$(id -g)"
    -v "$root:$root" -w "$root"
    -v "$HOME:$HOME" -e HOME -e PATH
    -v "$just:$just:ro"
)
# The image has no CA certificates; cargo's git fetches use the host's.
if [[ -d /etc/ssl/certs ]]; then
    args+=(-v /etc/ssl/certs:/etc/ssl/certs:ro)
fi
if [[ -d /nix ]]; then
    args+=(-v /nix:/nix:ro)
fi
# The toolchain's own settings, such as CARGO_HOME and the step's
# PKG_CONFIG_ALLOW_SYSTEM_LIBS.
while read -r name; do
    case "$name" in
        CARGO_* | RUSTUP_* | RUSTFLAGS | PKG_CONFIG_*) args+=(-e "$name") ;;
    esac
done < <(compgen -e)

exec docker run "${args[@]}" "$image" "$@"
