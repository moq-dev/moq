#!/usr/bin/env bash
#
# Run the CARLA worker's venv interpreter with an environment its wheels can
# actually load. Every entry point that runs the worker or a CARLA client goes
# through here, so the two fixes below live in one place.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
venv="$here/.venv"

if [[ ! -x "$venv/bin/python" ]]; then
    echo "python.sh: no venv; run 'just carla venv' first." >&2
    exit 1
fi

# 1. The devshell exports a PYTHONPATH into its own Python's site-packages. A
#    venv does not override it, so a 3.12 interpreter would import packages
#    built for another version and fail somewhere unrelated.
export PYTHONPATH=

# 2. manylinux wheels (CARLA's client and moq's own cdylib) link libstdc++ and
#    libz, which under Nix are in no default loader path. Ask the compiler on
#    PATH where they are rather than hardcoding store hashes that move with
#    every nixpkgs bump. A library it cannot place is left to the host's loader.
libs=()
for lib in libstdc++.so.6 libz.so.1; do
    found="$(g++ -print-file-name="$lib")"
    # g++ echoes the bare name back when it cannot place the library.
    [[ "$found" == /* ]] && libs+=("$(dirname "$found")")
done
if [[ ${#libs[@]} -eq 0 ]]; then
    echo "python.sh: g++ cannot locate libstdc++ or libz; run inside the devshell." >&2
    exit 1
fi

# No NVIDIA driver: the simulator encodes, and the worker only republishes.
LD_LIBRARY_PATH="$(
    IFS=:
    echo "${libs[*]}"
)${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LD_LIBRARY_PATH

exec "$venv/bin/python" "$@"
