#!/usr/bin/env bash
# Use this checkout's pinned quest for repository gates and local reviews.
set -euo pipefail

root=$(cd "${BASH_SOURCE[0]%/*}/.." && pwd)
exec nix run --no-update-lock-file "$root#quest" -- "$@"
