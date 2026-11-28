# [S] Rust tests that fail only on macOS pass there

## Goal

`just check` on an Apple Silicon macOS host reports only a branch's own
failures: these tests pass there as they do on Linux CI, each fixed at its root
cause.

- `moq-cli::integration identities::ietf22`: the held reader's `recv_group`
  returns `Err(Old)` (`read` in `rs/moq-cli/tests/integration/identities.rs`).
  `lite07` and `lite06` pass.
- `moq-net model::track::test::fetch_refresh_survives_eviction` and
  `same_tick_fetch_protects`: the FETCH of seq 0, made while the 11,000-byte
  pool should still hold it, returns `NotFound`.

## Plan

Found 2026-11-28 while reviewing
[moq-dev/moq#9579](https://github.com/moq-dev/moq/pull/9579). All three fail
deterministically on macOS at `origin/main` `22e3f0ad89` (`cargo nextest run
--locked -p moq-cli --test integration -E 'test(identities::)'` and `cargo
nextest run --locked -p moq-net --all-features -E
'test(fetch_refresh_survives_eviction) | test(same_tick_fetch_protects)'`),
while `main`'s Linux CI passes them.

Both `model::track` tests lose seq 0 before any pressure they intend, so
suspect a platform difference in what a group bills to the pool; reproduce
before choosing. The `ietf22` case may share it: the held reader sees
a group already evicted.

## Hosts

- macos - the failures reproduce only there
