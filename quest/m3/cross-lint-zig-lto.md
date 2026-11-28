# [XS] The macOS cross lint builds a C dependency that asks for LTO

## Goal

`just check`'s `rs_cross` leg (`sh/rs/cross-lint.sh`) passes on a macOS host
for a branch whose selection pulls in `alloca` (through `criterion` and
`bench-harness`), as it does on Linux.

## Plan

Found 2026-11-28 reviewing [moq-dev/moq#9715](https://github.com/moq-dev/moq/pull/9715),
reproduced unchanged on `main`: `alloca` 0.4.0's build script adds `-flto`,
and `sh/rs/zig-cc.sh` hands it to zig, which refuses it for the
`aarch64-macos` target (`zig cc -target aarch64-macos -flto -c a.c` prints
"LTO requires using LLD").

Drop `-flto` in `sh/rs/zig-cc.sh` beside the flags it already strips: a lint
links nothing, so LTO changes no result. Check why the Linux gate passes the
same selection (an mbx hit on the build script's output would hide it) so the
fix is verified on a cold cache, not a warm one.

Public API: none. Wire: none.

