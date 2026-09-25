#!/usr/bin/env bash
# Type-check every Python sample in the docs against the installed wrapper.
#
# Samples leave their inputs (`payload`, `pts`) to the reader, so they follow
# the declarations in moq-rs/tests/doc_prelude.py.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)/py"

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
{
    cat moq-rs/tests/doc_prelude.py
    bash ../doc/lib/samples.sh python ../doc/lib/py/index.md moq-rs/README.md moq-rs/docs/index.md
} >"$out/samples.py"
echo '{"pythonVersion": "3.10"}' >"$out/pyrightconfig.json"
uv run --no-sync pyright --project "$out" "$out/samples.py"
