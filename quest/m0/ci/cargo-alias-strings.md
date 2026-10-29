# [XS] Cargo aliases work in a nested checkout

## Goal

`cargo build-ffi` and `cargo moq` run correctly from a worktree nested inside
another moq checkout, such as an agent worktree under
`~/work/moq/.claude/worktrees/`.

## Plan

[moq-dev/moq#7245](https://github.com/moq-dev/moq/pull/7245) implements the
2026-10-28 maintainer decision to use string aliases. The nested-config
regression reproduces `build-ffi` rejecting the extra `build` with lists,
and passes with strings. It also checks that `cargo moq` forwards only the
caller's arguments, including spaces and `--`. It runs in `just gh test`,
which the local and CI gates always select.

The remaining list value, wasm32 `rustflags`, stays unchanged: one copy and
three concatenated copies produce identical `rustc --print cfg` output.
Cargo through three nested configs produces the same output too. An enclosing
checkout must also adopt the string aliases; Cargo refuses mixed string/list
config types. Agent worktrees stay where they are.

Remaining: run `quest gate just ci` in the Nix dev shell, resolve any relevant
failures or review findings, then remove this quest and its links and mark
the PR ready. On 2026-10-29 the command remained waiting for host capacity
and was stopped before any CI lane started. The focused regression, TOML
and Justfile formatting, and whitespace checks pass. Full validation is
unverified; this is separate from the mbx `Argument list too long` failure.

Public API: none. Wire: none.
