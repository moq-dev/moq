# [S] Three Rust tests that fail every run on macOS pass there

## Goal

These tests pass every run on macOS, each fixed at its cause. Today each fails every run on an
Apple Silicon host at `origin/main` `5a7973d818`, while the merge queue's Linux run passes, so
the queue does not catch them:

- `moq-cli::integration identities::ietf22` (`rs/moq-cli/tests/integration/identities.rs`):
  `read` panics with `Err(Old)` at the `group.read_frame().await.unwrap()` that checks a group
  holds one finished frame.
- `moq-net model::track::test::same_tick_fetch_protects`: `fetch_group(0)` returns `NotFound`.
- `moq-net model::track::test::fetch_refresh_survives_eviction`: a FETCH of a cached group fails.

## Plan

Reproduce with `cargo nextest run -p moq-cli --test integration identities::ietf22` and
`cargo nextest run -p moq-net --lib --features async -E 'test(same_tick_fetch_protects) | test(fetch_refresh_survives_eviction)'`.
The `moq-net` tests need `--features async`: without it nextest filters them out and reports
success. Both build a pooled producer sized in bytes (`pooled_producer(11_000)`), so check
whether the pool rounds to Apple Silicon's 16 KiB pages and evicts differently than on Linux's
4 KiB pages. [moq-dev/moq#9807](https://github.com/moq-dev/moq/pull/9807) last changed the
`identities` test; check whether its failure starts there.

Public API: none. Wire: none.

## Hosts

- macos - they fail only there so far
