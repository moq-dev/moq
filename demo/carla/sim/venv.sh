#!/usr/bin/env bash
#
# Create (or refresh) the CARLA worker's virtualenv.
#
# Python 3.12 exactly: CARLA 0.9.16 publishes wheels for cp310-cp312 only.
#
# moq comes from this checkout's py/ rather than PyPI, so the worker always
# runs the bindings of the tree it ships in: moq-ffi built by maturin from
# rs/moq-ffi, and the pure-Python moq-rs wrapper installed editable over it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
venv="$here/.venv"

# The devshell exports a PYTHONPATH into its own Python's site-packages. A venv
# does not override it, so a 3.12 interpreter would import packages built for
# another version and fail somewhere unrelated. python.sh does the same.
export PYTHONPATH=

uv venv --allow-existing --python 3.12 "$venv"

# The worker and its CARLA client wheel.
uv pip install --python "$venv/bin/python" --quiet -e "$here"

# maturin comes from the root workspace's locked dev tools, the same one
# `just py` builds with, but installs into this venv: VIRTUAL_ENV picks the
# target. Release, since this is the runtime the worker publishes through.
uv sync --project "$repo" --locked --no-install-workspace --quiet
(
    cd "$repo"
    VIRTUAL_ENV="$venv" "$repo/.venv/bin/maturin" develop --locked --release --uv
)

# --no-deps: moq-rs depends on moq-ffi, which maturin just installed from this
# tree; resolving it again would fetch a release from PyPI over it.
uv pip install --python "$venv/bin/python" --quiet --no-deps -e "$repo/py/moq-rs"

echo "venv: ready at $venv" >&2
