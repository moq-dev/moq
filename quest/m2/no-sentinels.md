# [S] No sentinel values in public API

## Goal

No public type uses a huge value or zero as a marker for "none" or
"unbounded"; each uses `Option` (or `NonZero*` where zero is invalid):

- `moq_net::transport::ReceiveWindow.connection` is documented as
  `u64::MAX` "when streams share none"; it becomes `Option<NonZeroU64>`,
  matching `transport::Limits.receive_window`. A zero connection window lets
  no stream receive a byte, so a transport that would report one is
  misconfigured and errors at setup.
- `cache_duration` defaults to `Duration::MAX` on `origin::Config` and
  `broadcast::Info` in moq-net, and `moq_relay::cache::Cache.duration` does
  the same; each becomes `Option<Duration>`, `None` meaning no ceiling.
- `moq_net::reconnect`'s `Config.timeout`, `Backoff.timeout`, and
  `Backoff.max` use zero for unbounded; they become `Option<Duration>`, as
  `Server::with_timeout` already is. The JS mirror (`Connection.Backoff`)
  follows, and the Rust (10 s) and JS (unlimited) defaults for the backoff
  timeout agree.
- moq-relay's `runtime.workers` uses `0` for "share the session runtime";
  it becomes `"shared" | "auto" | <n>`, defaulting to `"shared"`, so a later
  default change never reinterprets an unset or zero value (from
  [shard-default](/quest/m4/perf/shard-default.md)).
- moq-ffi's `moq_ffi_test_clock_now` returns `u64::MAX` when no clock runs;
  it reports absence without a sentinel.

## Plan

Found by the 2026-11-07 API shape audit (net-model, net-session, apps,
bindings). AGENTS.md: "Never use a huge value like `u64::MAX` as a marker."
Each struct already breaks in this release, so no extra break is taken.
`Range::oldest()`'s `Relative(u64::MAX)` is owned by
[time addressing's model](/quest/m2/time-addressing/model.md), not here. Pick
the aligned backoff default in the PR and say why.
