#!/usr/bin/env bash
# Unit-test the plugin sources against stubbed libobs/libmoq/ffmpeg, under
# ThreadSanitizer.
#
# Manual, like `just rs macos` and `just rs windows`, because ThreadSanitizer
# needs its own build. `just obs ci` runs the same tests without it, so a
# regression these assertions catch still turns PR CI red; the sanitizer is what
# adds the races on top. Run this whenever you touch src/, especially the session
# callback plumbing, whose orderings the build can't check.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$here/../../cpp/obs"

# Unlike the lint recipes, this one never skips: a test gate that reports
# success without running anything is worse than no gate.
cxx="${CXX:-c++}"
unsupported="'$cxx' cannot build and run -fsanitize=thread binaries. These tests need a
Clang or GCC whose ThreadSanitizer runtime works on this host: set CXX, or on Windows run
them from WSL, since neither MSVC nor Clang on Windows implements ThreadSanitizer.
They also run without the sanitizer as part of 'just obs ci'."
if ! command -v "$cxx" >/dev/null; then
    echo "no C++ compiler at '$cxx'." >&2
    echo "$unsupported" >&2
    exit 1
fi

probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT

# Run the probe, don't just link it: a mismatch between the compiler's TSan
# runtime and the host kernel (macOS is where this shows up) links fine and
# then segfaults before main, which is an unreadable way for the real test
# to fail.
# The subshell keeps bash's own "Segmentation fault" job message out of the
# output, so the explanation below is all the reader gets.
if ! printf 'int main(){}\n' | "$cxx" -x c++ -fsanitize=thread -o "$probe/probe" - >/dev/null 2>&1 ||
    ! ("$probe/probe" >/dev/null 2>&1) 2>/dev/null; then
    echo "$unsupported" >&2
    exit 1
fi

"$here/unit.sh" tsan
