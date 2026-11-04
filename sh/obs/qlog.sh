#!/usr/bin/env bash
# Build the in-tree C++ package with MOQ_FFI_QLOG, which an OBS build passes to
# show the qlog directory setting, and check that moq-ffi reports qlog support.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true && "${LOCAL_SUPERVISED:-}" != 1 ]]; then
    exec bash "$(dirname "$0")/../local.sh" bash "$0" "$@"
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

moq_prefix=$("$here/moq.sh" qlog)
export PKG_CONFIG_PATH="$moq_prefix/lib/pkgconfig"
# Assigned first: set -e ignores a failing substitution inside a command's arguments.
moq_sources=$(pkg-config --variable=sources moq-cpp)
moq_flags=$(pkg-config --cflags --libs moq-cpp)

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

printf '#include <moq/moq.hpp>\nint main() { return moq::qlog_supported() ? 0 : 1; }\n' >"$out/qlog.cpp"
# shellcheck disable=SC2086 # moq_flags is a flag list.
"${CXX:-c++}" -std=c++17 -pthread -o "$out/qlog" "$out/qlog.cpp" "$moq_sources" $moq_flags
if ! "$out/qlog"; then
    echo "MOQ_FFI_QLOG=ON built moq-ffi without qlog support" >&2
    exit 1
fi
echo "moq::qlog_supported(): ok"
