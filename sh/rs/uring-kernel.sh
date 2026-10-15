#!/usr/bin/env bash
# CI must run io_uring tests, rather than pass them on an unsupported kernel.
set -euo pipefail

release=$(uname -r)
if [[ $(uname -s) != Linux || ! $release =~ ^([0-9]+)\.([0-9]+)([.-]|$) ]]; then
    echo "io_uring tests require Linux 6.12 or later, found: $release" >&2
    exit 1
fi
major=${BASH_REMATCH[1]}
minor=${BASH_REMATCH[2]}
if ((10#$major < 6 || (10#$major == 6 && 10#$minor < 12))); then
    echo "io_uring tests require Linux 6.12 or later, found: $release" >&2
    exit 1
fi
echo "io_uring kernel: $release"
