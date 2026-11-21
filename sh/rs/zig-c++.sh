#!/usr/bin/env bash
# sh/rs/zig-cc.sh for C++, the CXX sh/rs/cross-lint.sh hands to build scripts.
ZIG_CC_MODE=c++ exec "$(dirname "$0")/zig-cc.sh" "$@"
