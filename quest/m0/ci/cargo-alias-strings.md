# [XS] Cargo aliases work in a nested checkout

## Goal

`cargo build-ffi` and `cargo moq` run correctly from a worktree nested inside
another moq checkout, such as an agent worktree under
`~/work/moq/.claude/worktrees/`.

## Plan

Found 2026-10-28 by [moq-dev/moq#7156](https://github.com/moq-dev/moq/pull/7156):
cargo reads every `.cargo/config.toml` from the working directory up, and
concatenates list values across files instead of overriding. In a nested
worktree both checkouts' list aliases join, so `build-ffi` becomes
`build --locked … build --locked …` and fails with "unexpected argument
'build'". Every C, C++, OBS, Kotlin, Go and Dart check fails locally for
agents. `cargo moq` duplicates the same way, but the copy lands after `--`.

Decided 2026-10-28 (maintainer): write the aliases in `.cargo/config.toml` as
strings, which override rather than concatenate. Check whether any other list
value there, such as the wasm32 `rustflags`, also duplicates harmfully. Add a
check that runs `cargo build-ffi --help` from a checkout nested inside
another. Agent worktrees stay where they are.

Public API: none. Wire: none.
