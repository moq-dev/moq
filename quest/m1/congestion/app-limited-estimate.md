# [M] App-limited BBR3 estimate ages

## Goal

After a path's capacity drops, an application-limited BBR3 sender's bandwidth
estimate falls within a few seconds instead of staying at the old peak for
20 s or more. Readers of the estimate, starting with the native simulcast rung
disable, stop funding rungs the path can no longer carry.

## Plan

- Cause: `update_max_bw` in `rs/moq-congestion/src/bbr3/mod.rs` admits an
  app-limited sample only when it is at least `max_bw`, and the max filter is
  keyed on `cycle_count`, which an app-limited sender never advances. The
  bandwidth probe also stays idle while the estimate covers its target.
  [moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635) measured
  neither run's estimate falling within 20 s.
- Fix it in the estimator, not in each reader (decided 2026-10-25, root-cause
  rule): age `max_bw` while app-limited, by wall time or by delivery rounds,
  whichever matches BBRv3 and QUICHE's behavior once compared. A reader-side
  fallback to `bw_shortterm` is the rejected alternative.
- Turn the `stale_estimate` report in `rs/moq-quic/src/tests/probe.rs` into an
  assertion on a mocked clock and a bufferbloat path (a rate limit below the
  media bitrate behind a deep finite queue).

## Related

- [Native rung disable](/quest/m1/simulcast-rung-disable.md) - reads this estimate
- [BBR media study](/quest/m2/quic-bbr-study.md) - broader BBR3 behavior under media
