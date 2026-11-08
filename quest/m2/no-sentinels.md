# [S] No sentinel values in public API

## Goal

No public type uses a huge value or zero as a marker for "none" or
"unbounded"; each uses `Option` (or `NonZero*` where zero is invalid):

- `moq_net::transport::ReceiveWindow.connection` is documented as
  `u64::MAX` "when streams share none"; it becomes `Option<NonZeroU64>`,
  matching `transport::Limits.receive_window`.
- `cache_duration` defaults to `Duration::MAX` on `origin::Config` and
  `broadcast::Info` in moq-net, and `moq_relay::cache::Cache.duration` does
  the same; each becomes `Option<Duration>`, `None` meaning no ceiling.
- `moq_net::reconnect`'s `Config.timeout`, `Backoff.timeout`, and
  `Backoff.max` use zero for unbounded; they become `Option<Duration>`, as
  `Server::with_timeout` already is. The JS mirror (`Connection.Backoff`)
  follows, and the Rust (10 s) and JS (unlimited) defaults for the backoff
  timeout agree.
- moq-ffi's `moq_ffi_test_clock_now` returns `u64::MAX` when no clock runs;
  it reports absence without a sentinel.

## Plan

Found by the 2026-11-07 API shape audit (net-model, net-session, apps,
bindings). AGENTS.md: "Never use a huge value like `u64::MAX` as a marker."
Each struct already breaks in this release, so no extra break is taken.
`Range::oldest()`'s `Relative(u64::MAX)` is owned by
[anchor-oldest](https://github.com/moq-dev/moq/pull/8149), not here. Pick
the aligned backoff default in the PR and say why.
