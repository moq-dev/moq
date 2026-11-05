#!/usr/bin/env bash
# Resolve quest for repository gates and local reviews.
set -euo pipefail

root=$(cd "${BASH_SOURCE[0]%/*}/.." && pwd)
if [[ -n ${MOQ_QUEST:-} ]]; then
    printf 'Dev-shell quest: %s\n' "$MOQ_QUEST" >&2
    exec "$MOQ_QUEST" "$@"
fi

if [[ -n ${ALLOW_HOST_TOOLCHAIN:-} || $OSTYPE == msys || $OSTYPE == cygwin ]]; then
    pin=$(bun -e 'console.log((await Bun.file(process.argv[1]).json()).nodes.quest.locked.rev)' "$root/flake.lock") || pin=unavailable
    if ! executable=$(command -v quest); then
        printf 'quest not found on PATH for the host toolchain; repository pin: %s\n' "$pin" >&2
        exit 127
    fi
    version=$(quest --version) || version=unavailable
    printf 'Host quest: %s at %s; repository pin: %s (build identity unverified)\n' "$version" "$executable" "$pin" >&2
    exec quest "$@"
fi
if ! command -v nix >/dev/null 2>&1; then
    echo 'quest requires the repository dev shell: run nix develop, or set ALLOW_HOST_TOOLCHAIN=1 to use host tools' >&2
    exit 127
fi
exec nix run --no-update-lock-file "$root#quest" -- "$@"
