#!/usr/bin/env bash
# sh/rs/zig-cc.sh's archiver, the AR sh/rs/cross-lint.sh hands to build scripts.
ZIG_CC_MODE="ar" exec "$(dirname "$0")/zig-cc.sh" "$@"
