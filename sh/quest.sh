#!/usr/bin/env bash
# Resolve quest for repository gates and local reviews.
set -euo pipefail

root=$(cd "${BASH_SOURCE[0]%/*}/.." && pwd)
if [[ -n ${MOQ_QUEST:-} ]]; then
    exec "$MOQ_QUEST" "$@"
fi

if [[ -n ${ALLOW_HOST_TOOLCHAIN:-} || $OSTYPE == msys || $OSTYPE == cygwin ]]; then
    pin=$(bun -e 'console.log((await Bun.file(process.argv[1]).json()).nodes.quest.locked.rev)' "$root/flake.lock")
    executable=$(command -v quest)
    version=$(quest --version)
    printf 'Host quest: %s at %s; repository pin: %s (build identity unverified)\n' "$version" "$executable" "$pin" >&2
    exec quest "$@"
fi
exec nix run --no-update-lock-file "$root#quest" -- "$@"
