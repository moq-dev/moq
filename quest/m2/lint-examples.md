# [S] Branch checks lint examples

## Goal

On Linux, `just check` runs clippy on the example targets of each crate it
selects, so a broken or lint-failing example fails the branch rather than the
next Nightly.

## Plan

Found 2026-10-28 by [moq-dev/moq#7142](https://github.com/moq-dev/moq/pull/7142):
`just check`'s Rust steps all use `--tests`, so the workspace's 23 examples,
including the interop clients that PR brought into scope, get clippy only from
`just fix`, the Nightly features run, and the compile-only Windows/macOS
check.

Decided 2026-10-28 (maintainer): examples only. The 68 benches stay with
Nightly, since they cost more to build. Measure the added build time on a
moq-cli change and record it in the PR.

Public API: none. Wire: none.
