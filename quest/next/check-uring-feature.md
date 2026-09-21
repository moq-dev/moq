# [S] A moq-relay change checks its io_uring feature

## Goal

`just check` compiles `moq-relay` with `--features io-uring` whenever
the diff selects moq-relay, so a change that breaks the ring listener fails
its own PR instead of the nightly. Today the only per-PR build of that
feature is `bench check` in `bench/justfile`, which `just check` runs only
for diffs under `bench/`; dev shipped an unresolved `crate::peer` under the
feature for days (fixed in #3749) with every PR green.

## Plan

In `rs/justfile`, `check-changed` runs the feature clippy from the nightly
`uring` recipe (`--features io-uring`, `-D warnings`, no tests) when
`_select` includes moq-relay; `check --all` already covers it through `bench
check`. Keep the nightly `uring` matrix for the tests. Verify by breaking the
feature on a scratch branch and watching `just check` fail on an
`rs/moq-relay` diff, and by timing the added compile on a warm cache.

## Related

- [Thin justfiles](/quest/next/tooling/justfiles.md) - the impact map that later owns this rule
